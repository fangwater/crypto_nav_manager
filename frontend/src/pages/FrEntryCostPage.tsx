import { ArrowLeft, RefreshCw } from 'lucide-react'
import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { getFrCostStrategies, getFrEntryCost } from '../api'
import { FrAlignmentChart, FrCostChart } from '../components/FrCostChart'
import { costBps, coverage } from '../frEntryCost'
import type { FrEntryCost } from '../frEntryCost'
import type { Strategy } from '../types'
import './FrEntryCostPage.css'

const fmt = (n: number | null, digits = 4) => n === null ? '未知' : n.toLocaleString('en-US', { maximumFractionDigits: digits })
const time = (n: number | null) => n === null ? '—' : new Date(n).toLocaleString('zh-CN', { hour12: false })

export function FrEntryCostPage() {
  const { slug } = useParams()
  const navigate = useNavigate()
  const [strategies, setStrategies] = useState<Strategy[]>([])
  const [data, setData] = useState<FrEntryCost | null>(null)
  const [symbol, setSymbol] = useState('')
  const [asOf, setAsOf] = useState('')
  const [reference, setReference] = useState('')
  const [query, setQuery] = useState('')
  const [refresh, setRefresh] = useState(0)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState('')
  const [page, setPage] = useState(0)
  const [crossOnly, setCrossOnly] = useState(false)

  useEffect(() => {
    const controller = new AbortController()
    getFrCostStrategies(controller.signal).then(rows => {
      const fr = rows.filter(s => s.strategyKind === 'funding_rate')
      setStrategies(fr)
      if (!slug && fr.length) navigate(`/fr-entry-cost/${fr[0].slug}`, { replace: true })
    }).catch(e => { if (!controller.signal.aborted) setError(String(e)) })
    return () => controller.abort()
  }, [slug, navigate])

  useEffect(() => {
    if (!slug) return
    const controller = new AbortController()
    setLoading(true)
    setError('')
    setData(null)
    setPage(0)
    getFrEntryCost(slug, new URLSearchParams(query), controller.signal).then(result => {
      if (controller.signal.aborted) return
      setData(result)
      setSymbol(result.symbol)
    }).catch(e => { if (!controller.signal.aborted) setError(e instanceof Error ? e.message : String(e)) })
      .finally(() => { if (!controller.signal.aborted) setLoading(false) })
    return () => controller.abort()
  }, [slug, query, refresh])

  function run() {
    const params = new URLSearchParams()
    if (symbol) params.set('symbol', symbol)
    if (asOf) params.set('endMs', String(new Date(asOf).getTime()))
    if (reference.trim()) params.set('referenceBps', reference)
    setQuery(params.toString())
    setRefresh(v => v + 1)
  }
  const summary = data?.report.summary
  const pairs = data?.report.pairs.filter(p => !crossOnly || p.crossMinute) ?? []
  const pages = Math.max(1, Math.ceil(pairs.length / 50))

  return <>
    <header className="detail-header"><div className="detail-header__inner">
      <div className="detail-title"><Link className="icon-button" to="/" aria-label="返回总览"><ArrowLeft size={18} /></Link>
        <div><p>FR · 成交重建仓位 · FIFO</p><h1>建仓成本</h1></div></div>
      {slug && <Link className="config-button" to={`/strategies/${slug}`}>账户净值</Link>}
    </div></header>
    <main className="fr-cost-shell">
      <form className="fr-cost-controls" onSubmit={e => { e.preventDefault(); run() }}>
        <label>FR 账户<select value={slug ?? ''} onChange={e => {
          setSymbol(''); setQuery(''); setReference(''); setAsOf(''); navigate(`/fr-entry-cost/${e.target.value}`)
        }}><option value="" disabled>选择账户</option>{strategies.map(s => <option key={s.slug} value={s.slug}>{s.displayName}</option>)}</select></label>
        <label>币种<select value={symbol} onChange={e => setSymbol(e.target.value)} disabled={!data}>
          {(data?.availableSymbols ?? []).map(s => <option key={s} value={s}>{s}</option>)}</select></label>
        <label>持仓截至时间（留空为现在）<input type="datetime-local" value={asOf} onChange={e => setAsOf(e.target.value)} /></label>
        <label>参考基差假设 bps（留空用市场数据）<input type="number" step="any" min="-10000" max="10000" value={reference} placeholder="自动读取分钟 premium" onChange={e => setReference(e.target.value)} /></label>
        <button className="config-button" disabled={loading || !slug}><RefreshCw size={15} />{loading ? '重建与补齐中…' : '计算 / 刷新'}</button>
      </form>
      {error && <p className="fr-cost-notice" role="alert">{error}</p>}
      {!slug && !error && strategies.length === 0 && <p>没有可访问的 FR 账户。</p>}
      {loading && <p className="fr-cost-notice" role="status">重放实际成交并补齐分钟参考，首次加载可能需要约 40 秒。</p>}
      {data && summary && <>
        <section className="fr-cost-hero">
          <div><p>{data.symbol || '无成交'} · {data.referenceBps === null ? '建仓成本估算' : '建仓成本 · 假设情景'}</p>
            <h2>{summary.pairCount ? fmt(summary.estimatedCostUsdt) : '无配对仓位'} <small>USDT</small></h2>
            <strong>{fmt(summary.estimatedCostBps)} bps</strong>
            <p>参考基差 − 实际成交基差 ＋ 两腿手续费；正值为损耗，负值为相对参考的优势。</p>
            <p>成本只覆盖历史重建中已配对的剩余仓位；数量不齐时，不能代表整组真实持仓的总成本。</p>
          </div>
          <div className="fr-cost-hero-meta"><span>配对现货名义本金 <b>{fmt(summary.notionalUsdt, 2)} USDT</b></span>
            <span>完整成本覆盖 <b>{fmt(coverage(summary.costCoveredQuantity, summary.quantity), 2)}%</b></span>
            <span>两腿库存配对覆盖 <b>{fmt(coverage(2 * summary.quantity, Math.abs(data.report.spotQuantity) + Math.abs(data.report.futuresQuantity)), 2)}%</b></span>
            <span>持仓截至 <b>{time(data.endMs)}</b></span><span>最新入库成交 <b>{time(data.lastTradeMs)}</b></span></div>
        </section>
        {Math.abs(data.report.netQuantity) > 1e-8 && <p className="fr-cost-notice">成交历史重建数量未完全对齐。这可能来自两腿成交时间差、期初库存、划转或历史缺口，不足以证明实盘仓位不中性。下方净数量仅用于核对，平均参考值不会消除数量缺口。</p>}
        <div className="fr-cost-metrics">
          <article><span>实际成交基差</span><strong>{fmt(costBps(summary.entrySpreadUsdt, summary.notionalUsdt))} bps</strong><small>已按套利方向调整，正值有利</small></article>
          <article><span>参考基差</span><strong>{fmt(costBps(summary.referenceSpreadUsdt, summary.notionalUsdt))} bps</strong><small>{data.referenceBps === null ? '跨分钟 premium 均值' : `手动恒定假设 ${data.referenceBps} bps`}</small></article>
          <article><span>执行偏离估算</span><strong>{fmt(summary.executionDeviationUsdt)} USDT</strong><small>含等待与指数口径差异，非纯滑点</small></article>
          <article><span>剩余仓位分摊手续费</span><strong>{fmt(summary.feeUsdt)} USDT</strong><small>实际成交费；负数为返佣</small></article>
        </div>
        <section className="fr-cost-panel"><h2>数量与时间对齐</h2>
          <p>事实快照：{time(data.factualSnapshotMs)}。{data.factualPosition
            ? `现货 ${fmt(data.factualPosition.spotQuantity, 8)}，合约 ${fmt(data.factualPosition.futuresQuantity, 8)}。请与下方成交重建数量核对；快照时点可能早于查询截止时间。`
            : '该快照未列示此币种的完整两腿数量，不能据此当作零仓位。'}</p>
          <div className="fr-cost-alignment"><span>现货净数量 <b>{fmt(data.report.spotQuantity, 8)}</b></span><span>合约净数量 <b>{fmt(data.report.futuresQuantity, 8)}</b></span>
            <span>未对齐净数量 <b>{fmt(data.report.netQuantity, 8)}</b></span><span>已配对数量 <b>{fmt(summary.quantity, 8)}</b></span>
            <span>跨分钟配对占比 <b>{fmt(coverage(summary.crossMinuteQuantity, summary.quantity), 2)}%</b></span><span>最大两腿间隔 <b>{fmt(summary.maxLagMs / 60_000, 2)} 分钟</b></span></div>
          <p>FIFO 跨分钟连续匹配，不按分钟重置。只计算截至所选时刻仍未平掉的库存；未配对部分不混入中性仓位成本。</p>
          <FrAlignmentChart points={data.alignment} />
          <p>最近 24 小时的分钟末净数量，用于观察跨分钟错位；最后一分钟截至所选时刻。成交重建不等同于实时账户资产快照。</p>
        </section>
        <section className="fr-cost-panel"><h2>1 分钟建仓归因</h2><p>归入两腿较晚成交所在分钟。每个 FIFO 片段平均两腿之间所覆盖分钟的参考值，再按配对名义本金汇总。</p>
          {data.report.minutes.length ? <FrCostChart minutes={data.report.minutes} /> : <p>暂无配对库存。</p>}
        </section>
        <section className="fr-cost-panel"><div className="fr-cost-table-head"><h2>FIFO 成本明细 · {pairs.length} 个片段</h2>
          <label><input type="checkbox" checked={crossOnly} onChange={e => { setCrossOnly(e.target.checked); setPage(0) }} /> 仅看跨分钟</label></div>
          <div className="fr-cost-table-wrap"><table><thead><tr><th>方向 / 数量</th><th>现货成交</th><th>合约成交</th><th>时间间隔</th><th>参考覆盖</th><th>成交基差 bps</th><th>手续费 USDT</th><th>成本估算 USDT</th></tr></thead>
            <tbody>{pairs.slice(page * 50, (page + 1) * 50).map((p, i) => <tr key={`${p.spot.id}-${p.futures.id}-${i}`}>
              <td>{p.direction === 'positive' ? '正套' : '反套'}<small>{fmt(p.quantity, 8)}</small></td>
              <td title={`成交 ${p.spot.id} · 订单 ${p.spot.orderId}`}>{fmt(p.spot.price, 8)} · {p.spot.liquidity}<small>{time(p.spot.ts)}</small></td>
              <td title={`成交 ${p.futures.id} · 订单 ${p.futures.orderId}`}>{fmt(p.futures.price, 8)} · {p.futures.liquidity}<small>{time(p.futures.ts)}</small></td>
              <td>{fmt(p.lagMs / 1000, 2)} 秒{p.crossMinute && <small>跨分钟</small>}</td><td>{p.referenceMinutes} / {p.expectedMinutes} 分钟</td>
              <td>{fmt(costBps(p.entrySpreadUsdt, p.notionalUsdt))}</td><td>{fmt(p.feeUsdt)}</td><td>{fmt(p.estimatedCostUsdt)}</td>
            </tr>)}</tbody></table></div>
          <div className="fr-cost-pager"><button disabled={page === 0} onClick={() => setPage(p => p - 1)}>上一页</button><span>{page + 1} / {pages}</span><button disabled={page + 1 >= pages} onClick={() => setPage(p => p + 1)}>下一页</button></div>
        </section>
        {data.report.unmatched.length > 0 && <section className="fr-cost-panel"><h2>未配对库存 · {data.report.unmatched.length} 笔</h2><p>保留原始数量，不通过平均价格强制对齐。以下显示前 100 笔。</p>
          <div className="fr-cost-table-wrap"><table><thead><tr><th>市场</th><th>方向</th><th>数量</th><th>成交价</th><th>成交时间</th></tr></thead><tbody>{data.report.unmatched.slice(0, 100).map((lot, i) => <tr key={`${lot.id}-${i}`}><td>{lot.leg}</td><td>{lot.side === 1 ? '买' : '卖'}</td><td>{fmt(lot.quantity, 8)}</td><td>{fmt(lot.price, 8)}</td><td>{time(lot.ts)}</td></tr>)}</tbody></table></div>
        </section>}
        <details className="fr-cost-panel"><summary>计算口径与数据覆盖</summary><p>历史重放起点：{time(data.historyStartMs)}；读取成交 {data.report.tradeCount.toLocaleString()} 笔。参考来源：{data.referenceSource}。</p>
          <ul>{data.warnings.map(w => <li key={w}>{w}</li>)}</ul><p>这是建仓执行成本，不是建仓资金占用，也不是已经实现的套利亏损；资金费、借贷利息和退出基差另计。</p></details>
      </>}
    </main>
  </>
}
