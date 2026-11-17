import { useEffect, useMemo, useRef, useState } from 'react'
import { getFrCostOverview, getFrCostRow } from '../api'
import { entryCostRow } from '../frEntryCost'
import type { FrCostOverview, FrCostRow, FrEntryCost } from '../frEntryCost'
import { compareCostRows, costMetric, EMPTY_THRESHOLDS, matchesCostThresholds, numberInput } from '../frCostScreen'
import type { CostMetric, ThresholdMetric, Thresholds } from '../frCostScreen'
import { FrCostOverviewCharts } from './FrCostOverviewCharts'

const fmt = (n: number | null, digits = 2) => n === null ? '未知' : n.toLocaleString('en-US', { maximumFractionDigits: digits })
const thresholdFields: { key: ThresholdMetric; label: string; placeholder: string; max?: number; min?: number }[] = [
  { key: 'taker', label: '任一腿 taker ≥ %', placeholder: '如 30', min: 0, max: 100 },
  { key: 'deviation', label: '不利执行偏差 ≥ bps', placeholder: '如 10', min: 0 },
  { key: 'basisAbs', label: '|入场基差| ≥ bps', placeholder: '如 50', min: 0 },
  { key: 'feeBps', label: '手续费 ≥ bps', placeholder: '不限' },
  { key: 'costBps', label: '成本估算 ≥ bps', placeholder: '不限' },
]
const columns: { key: CostMetric | 'symbol'; label: string }[] = [
  { key: 'symbol', label: '币种 / 方向' },
  { key: 'notional', label: '配对本金 USDT' },
  { key: 'spotTaker', label: '现货 taker %' },
  { key: 'futuresTaker', label: '合约 taker %' },
  { key: 'feeBps', label: '手续费 bps' },
  { key: 'basis', label: '入场基差 bps' },
  { key: 'basisAbs', label: '|基差| bps' },
  { key: 'deviation', label: '执行偏差 bps' },
  { key: 'costBps', label: '成本估算 bps' },
  { key: 'lastPair', label: '最后对齐时间' },
]

export function FrCostOverviewTable({ slug, endMs, refresh, detail, onSelect, onLoaded }: {
  slug: string; endMs: number; refresh: number; detail: FrEntryCost | null; onSelect: (symbol: string) => void; onLoaded: (overview: FrCostOverview) => void
}) {
  const [overview, setOverview] = useState<FrCostOverview | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState('')
  const [thresholds, setThresholds] = useState<Thresholds>({ ...EMPTY_THRESHOLDS })
  const [search, setSearch] = useState('')
  const [minNotional, setMinNotional] = useState('')
  const [mode, setMode] = useState<'any' | 'all'>('any')
  const [keepUnknown, setKeepUnknown] = useState(true)
  const [sort, setSort] = useState<CostMetric | 'symbol'>('costBps')
  const [ascending, setAscending] = useState(false)
  const [page, setPage] = useState(0)
  const backfill = useRef<AbortController | null>(null)
  const autoStarted = useRef(false)
  const [progress, setProgress] = useState<{ done: number; total: number; symbol: string; failed: number } | null>(null)
  const [backfillNote, setBackfillNote] = useState('')

  useEffect(() => {
    const controller = new AbortController()
    autoStarted.current = false
    setLoading(true); setError(''); setOverview(null); setProgress(null); setBackfillNote('')
    getFrCostOverview(slug, new URLSearchParams({ endMs: String(endMs) }), controller.signal)
      .then(result => { if (!controller.signal.aborted) { setOverview(result); onLoaded(result) } })
      .catch(e => { if (!controller.signal.aborted) setError(e instanceof Error ? e.message : String(e)) })
      .finally(() => { if (!controller.signal.aborted) setLoading(false) })
    return () => { controller.abort(); backfill.current?.abort(); backfill.current = null }
  }, [slug, endMs, refresh, onLoaded])

  // A detailed calculation can enrich one row, but a manual scenario must never
  // enter the cross-symbol market-reference comparison.
  useEffect(() => {
    if (!detail || loading || detail.strategySlug !== slug || detail.endMs !== endMs || detail.referenceBps !== null) return
    const row = entryCostRow(detail)
    setOverview(previous => previous ? { ...previous, rows: previous.rows.map(r => r.symbol === row.symbol ? row : r) } : previous)
  }, [detail, loading, slug, endMs])

  const filtered = useMemo(() => {
    const minimum = numberInput(minNotional)
    return (overview?.rows ?? []).filter(row => {
      if (!row.symbol.includes(search.trim().toUpperCase())) return false
      const match = matchesCostThresholds(row, thresholds, mode, minimum)
      return match === true || (match === null && keepUnknown)
    }).sort((a, b) => compareCostRows(a, b, sort, ascending))
  }, [overview, search, minNotional, thresholds, mode, keepUnknown, sort, ascending])
  const pages = Math.max(1, Math.ceil(filtered.length / 25))
  const activePage = Math.min(page, pages - 1)
  const pending = filtered.filter(row => matchesCostThresholds(row, thresholds, mode, numberInput(minNotional)) === null).length
  const incomplete = overview?.rows.filter(row => row.summary.pairCount > 0 && row.summary.referenceCoveredQuantity + 1e-8 < row.summary.quantity) ?? []
  const supportsReference = overview && ['binance', 'bybit', 'gate'].includes(overview.exchange)

  useEffect(() => {
    if (!loading && overview?.strategySlug === slug && overview?.endMs === endMs && supportsReference && !autoStarted.current) {
      autoStarted.current = true
      void fillReferences()
    }
  })

  async function fillReferences() {
    if (!overview || backfill.current) return
    const controller = new AbortController()
    backfill.current = controller
    setBackfillNote('')
    let done = 0, failed = 0
    for (const row of incomplete) {
      if (controller.signal.aborted) break
      setProgress({ done, total: incomplete.length, symbol: row.symbol, failed })
      try {
        const result = await getFrCostRow(slug, row.symbol, endMs, controller.signal)
        if (controller.signal.aborted) break
        setOverview(previous => previous ? { ...previous, rows: previous.rows.map(r => r.symbol === result.symbol ? result : r) } : previous)
      } catch {
        if (controller.signal.aborted) break
        failed++
      }
      done++
    }
    if (backfill.current === controller) {
      backfill.current = null
      setProgress(null)
      setBackfillNote(`${controller.signal.aborted ? '已停止' : '本轮补齐结束'}：已处理 ${done} / ${incomplete.length} 个币种${failed ? `，${failed} 个请求失败` : ''}。仍有缺口的指标保留未知，可再次补齐。`)
    }
  }

  function sortBy(key: CostMetric | 'symbol') {
    setAscending(sort === key ? !ascending : key === 'symbol')
    setSort(key); setPage(0)
  }
  function reset() {
    setThresholds({ ...EMPTY_THRESHOLDS }); setSearch(''); setMinNotional(''); setMode('any'); setKeepUnknown(true); setPage(0)
  }
  function cell(row: FrCostRow, key: CostMetric | 'symbol') {
    const value = key === 'symbol' ? null : costMetric(row, key)
    if (key === 'symbol') return <><button className="fr-cost-symbol" onClick={() => onSelect(row.symbol)}>{row.symbol}</button>
      <small>{row.direction === 'positive' ? '正套' : row.direction === 'reverse' ? '反套' : 'FIFO 成本待核对'}{matchesCostThresholds(row, thresholds, mode, numberInput(minNotional)) === null ? ' · 筛选指标未知' : ''}</small></>
    if (key === 'lastPair') return value === null ? '未知' : new Date(value).toLocaleString('zh-CN', { hour12: false })
    if (key === 'spotTaker' || key === 'futuresTaker') {
      const leg = row.pairedLiquidity[key === 'spotTaker' ? 'spot' : 'futures']
      return <>{fmt(value)}<small>maker {row.summary.pairCount ? fmt(leg.makerNotionalPct) : '未知'}% · 未知 {row.summary.pairCount ? fmt(leg.unknownNotionalPct) : '未知'}%</small></>
    }
    if (key === 'feeBps' || key === 'costBps' || key === 'deviation') {
      const metric = row.covered[key === 'feeBps' ? 'fee' : key === 'costBps' ? 'cost' : 'deviation']
      const coverage = metric.quantityCoveragePct ?? 0
      const missingFee = row.pairedLiquidity.spot.total.unconvertedFeeParts + row.pairedLiquidity.futures.total.unconvertedFeeParts > 0
      return <><span className={key === 'deviation' && value !== null && value > 0 ? 'fr-cost-adverse' : ''}>{value === null ? '待补齐' : fmt(value)}</span>
        <small>{value === null ? missingFee && key !== 'deviation' ? '缺少费用折算 / 参考' : '缺少分钟参考或成交' : `${coverage >= 99.9999 ? '完整' : '部分'} · 数量覆盖 ${fmt(coverage)}%`}</small>
        {value !== null && <small>{fmt(metric.valueUsdt)} USDT{coverage < 99.9999 ? ' · 仅已覆盖部分' : ''}</small>}</>
    }
    return fmt(value)
  }

  return <section className="fr-cost-panel fr-cost-overview" aria-label="按币种筛选建仓成本">
    <div className="fr-cost-table-head"><h2>按币种比较建仓成本</h2><span className="fr-cost-count">{loading ? '读取当前持仓中…' : `当前持仓 ${filtered.length} / ${overview?.rows.length ?? 0} 个币种`}</span></div>
    <p>仅当前持仓（实时合约数量非零），合约平仓后不再追踪；成本按 FIFO 剩余配对库存计算。阈值留空表示不限，点击表头切换升降序。入场基差 = (合约均价 − 现货均价) / 现货均价，保留正负；大基差不直接等于高成本。执行偏差正值为不利，仅为相对 premium 的估算。</p>
    <div className="fr-cost-screen-controls">
      <label>搜索币种<input type="search" value={search} onChange={e => { setSearch(e.target.value); setPage(0) }} placeholder="如 BTC" /></label>
      <label>最低配对本金 USDT<input type="number" min="0" step="any" value={minNotional} onChange={e => { setMinNotional(e.target.value); setPage(0) }} placeholder="不限" /></label>
      {thresholdFields.map(field => <label key={field.key}>{field.label}<input type="number" step="any" min={field.min} max={field.max} value={thresholds[field.key]} placeholder={field.placeholder}
        onChange={e => { setThresholds(previous => ({ ...previous, [field.key]: e.target.value })); setPage(0) }} /></label>)}
    </div>
    <div className="fr-cost-screen-actions">
      <label>指标阈值组合<select value={mode} onChange={e => { setMode(e.target.value as 'any' | 'all'); setPage(0) }}><option value="any">满足任一阈值</option><option value="all">满足全部阈值</option></select></label>
      <label><input type="checkbox" checked={keepUnknown} onChange={e => { setKeepUnknown(e.target.checked); setPage(0) }} /> 保留无法判断的币种</label>
      <button type="button" onClick={reset}>清空筛选</button>
      {progress ? <button type="button" onClick={() => backfill.current?.abort()}>停止补齐</button>
        : <button type="button" disabled={!supportsReference || !incomplete.length} onClick={() => void fillReferences()}>补齐缺失参考（{incomplete.length}）</button>}
    </div>
    {loading && <p role="status">读取实时持仓，仅重建当前持有币种的成交…</p>}
    {error && <p className="fr-cost-notice" role="alert">币种汇总加载失败：{error}。可使用上方“计算 / 刷新”重试。</p>}
    {pending > 0 && <p>当前保留 {pending} 个无法判断是否满足阈值的币种，不代表已命中或正常。</p>}
    {progress && <p role="status">自动补齐 {progress.symbol}，已处理 {progress.done} / {progress.total}；每个币种最多约 8 秒，可继续筛选或停止。</p>}
    {backfillNote && <p role="status">{backfillNote}</p>}
    {overview && <>
      <div className="fr-cost-coverage-cards">
        <article><span>当前持仓</span><strong>{overview.rows.length}</strong><small>以实时合约持仓为准</small></article>
        <article><span>可计算成本</span><strong>{overview.rows.filter(r => costMetric(r, 'costBps') !== null).length}</strong><small>完整 {overview.rows.filter(r => r.summary.estimatedCostBps !== null).length} · 其余标注覆盖率</small></article>
        <article><span>参考尚有缺口</span><strong>{incomplete.length}</strong><small>自动逐币种补齐，不填零</small></article>
        <article><span>费用折算待补齐</span><strong>{overview.rows.filter(r => r.pairedLiquidity.spot.total.unconvertedFeeParts + r.pairedLiquidity.futures.total.unconvertedFeeParts > 0).length}</strong><small>例如 BNB 手续费缺少成交时估值</small></article>
      </div>
      <FrCostOverviewCharts rows={filtered} takerThreshold={numberInput(thresholds.taker)} basisThreshold={numberInput(thresholds.basisAbs)} onSelect={onSelect} />
      <p>表格、排序和阈值使用当前可计算片段的 bps。部分覆盖按该部分本金计算，不外推为整仓成本；鼠标悬停图表可查看覆盖率，点击可看明细。</p>
      <div className="fr-cost-table-wrap"><table><thead><tr>{columns.map(column => <th key={column.key} aria-sort={sort === column.key ? ascending ? 'ascending' : 'descending' : 'none'}>
        <button className="fr-cost-sort" onClick={() => sortBy(column.key)}>{column.label} <span aria-hidden="true">{sort === column.key ? ascending ? '↑' : '↓' : '↕'}</span></button>
      </th>)}</tr></thead><tbody>{filtered.slice(activePage * 25, (activePage + 1) * 25).map(row => <tr key={row.symbol} data-symbol={row.symbol} className={detail?.symbol === row.symbol ? 'fr-cost-selected' : ''}>
        {columns.map(column => <td key={column.key}>{cell(row, column.key)}</td>)}
      </tr>)}</tbody></table></div>
      {!filtered.length && <p>{overview.rows.length ? '没有符合当前筛选条件的持仓币种。' : '当前没有 FR 持仓。'}</p>}
      <div className="fr-cost-pager"><button disabled={activePage === 0} onClick={() => setPage(activePage - 1)}>上一页</button><span>{activePage + 1} / {pages}</span><button disabled={activePage + 1 >= pages} onClick={() => setPage(activePage + 1)}>下一页</button></div>
      <p>实时持仓快照：{new Date(overview.factualSnapshotMs).toLocaleString('zh-CN', { hour12: false })}。汇总截至 {new Date(overview.endMs).toLocaleString('zh-CN', { hour12: false })}。taker 按各腿成交额计算，未知成交角色仍计入分母。成本仅覆盖配对库存；缺失原始成本、费用或参考时保留未知。汇总使用市场参考，手动假设仅影响单币明细。{!supportsReference ? '本交易所自动参考尚不可用。' : '自动补齐当前持仓的参考；仍有缺口可再次补齐。'}</p>
    </>}
  </section>
}
