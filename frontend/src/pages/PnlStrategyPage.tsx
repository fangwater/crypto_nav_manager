import {
  ArrowLeft,
  CalendarRange,
  Check,
  CircleAlert,
  Database,
  ExternalLink,
  FlaskConical,
  GitCompareArrows,
  LoaderCircle,
  RefreshCw,
  Search,
  Settings,
  Trash2,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link, useParams } from 'react-router-dom'
import {
  clearInitialSnapshot,
  getAlignmentStatuses,
  getInitialSnapshot,
  getStrategy,
  getStrategyPnl,
  getStrategySnapshots,
  setInitialSnapshot,
} from '../api'
import {
  alignmentLabel,
  alignmentTime,
  alignmentTitle,
  alignmentTone,
} from '../alignment'
import { strategySurfaceAnalysisLink } from '../analysisNav'
import { PnlChart } from '../components/PnlChart'
import { PositionChart } from '../components/PositionChart'
import type {
  PnlSeriesKey,
  PositionSeriesKey,
  PositionUnit,
  AlignmentStatus,
  Strategy,
  StrategyPnl,
  StrategySnapshotSummary,
  SymbolPnlSummary,
} from '../types'

const rangeOptions = [
  { key: 'ALL', days: null },
  { key: '1D', days: 1 },
  { key: '3D', days: 3 },
  { key: '7D', days: 7 },
  { key: '30D', days: 30 },
] as const

const DAY_MS = 86_400_000
const DEFAULT_RANGE_DAYS = 3
const PNL_EPSILON = 1e-9

type PnlSymbolSelection = 'all' | 'positive' | 'negative' | 'custom'

const seriesOptions: Array<{
  key: PnlSeriesKey
  label: string
  color: string
}> = [
  { key: 'totalPnlUsdt', label: 'Total', color: '#176b5b' },
  { key: 'feeBeforePnlUsdt', label: 'Fee 前净值', color: '#2563a7' },
  { key: 'feeAfterPnlUsdt', label: 'Fee 后净值', color: '#7357a3' },
  { key: 'floatingPnlUsdt', label: '浮动盈亏', color: '#4b6478' },
  { key: 'fundingPnlUsdt', label: 'Funding', color: '#b7791f' },
  { key: 'interestCostUsdt', label: 'Interest', color: '#c2413b' },
]

const positionSeriesOptions: Array<{
  key: PositionSeriesKey
  label: string
  lineType: 'solid' | 'dashed' | 'area'
}> = [
  { key: 'spotPositionUsdt', label: 'Spot 仓位', lineType: 'solid' },
  { key: 'futuresPositionUsdt', label: 'Swap 仓位', lineType: 'dashed' },
  { key: 'exposureUsdt', label: '敞口', lineType: 'area' },
]

const marketMakingPositionSeriesOptions = positionSeriesOptions
  .filter((option) => option.key !== 'spotPositionUsdt')
  .map((option) =>
    option.key === 'futuresPositionUsdt'
      ? { ...option, label: '合约净仓位' }
      : option,
  )


function kindLabel(kind: Strategy['strategyKind']) {
  if (kind === 'funding_rate') return '资金费套利'
  if (kind === 'market_making') return '做市'
  if (kind === 'cta') return 'CTA'
  return '所内套利'
}

function toDatetimeLocal(ms: number) {
  const date = new Date(ms)
  const local = new Date(ms - date.getTimezoneOffset() * 60_000)
  return local.toISOString().slice(0, 16)
}

function fromDatetimeLocal(value: string) {
  return new Date(value).getTime()
}

function money(value: number, signed = false) {
  const formatted = Math.abs(value).toLocaleString('en-US', {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  })
  if (signed && value !== 0) return (value > 0 ? '+' : '-') + formatted
  return (value < 0 ? '-' : '') + formatted
}

function compactNumber(value: number) {
  return value.toLocaleString('en-US', { maximumFractionDigits: 0 })
}

function valueClass(value: number) {
  if (value > 1e-9) return 'is-positive'
  if (value < -1e-9) return 'is-negative'
  return ''
}

function exchangeBadge(exchange: Strategy['exchange']) {
  switch (exchange) {
    case 'bybit':
      return 'BY'
    case 'binance':
      return 'BN'
    case 'gate':
      return 'GT'
    case 'bitget':
      return 'BG'
    case 'okx':
      return 'OK'
  }
}

function SymbolRow({
  row,
  selected,
  onToggle,
}: {
  row: SymbolPnlSummary
  selected: boolean
  onToggle: () => void
}) {
  return (
    <tr className={selected ? 'is-selected' : ''}>
      <td>
        <button
          className={'symbol-check' + (selected ? ' is-checked' : '')}
          type="button"
          onClick={onToggle}
          title={selected ? '移出组合' : '加入组合'}
          aria-label={(selected ? '移出 ' : '加入 ') + row.symbol}
        >
          {selected && <Check size={13} />}
        </button>
        <strong>{row.symbol}</strong>
      </td>
      <td>{compactNumber(row.tradeCount)}</td>
      <td>{money(row.volumeUsdt)}</td>
      <td className={valueClass(row.feeBeforePnlUsdt)}>
        {money(row.feeBeforePnlUsdt, true)}
      </td>
      <td className={valueClass(row.feeAfterPnlUsdt)}>
        {money(row.feeAfterPnlUsdt, true)}
      </td>
      <td className={valueClass(row.fundingPnlUsdt)}>
        {money(row.fundingPnlUsdt, true)}
      </td>
      <td className={row.interestCostUsdt > 0 ? 'is-negative' : ''}>
        {money(-row.interestCostUsdt, true)}
      </td>
      <td className={valueClass(row.totalPnlUsdt)}>
        <strong>{money(row.totalPnlUsdt, true)}</strong>
      </td>
    </tr>
  )
}

function PnlSymbolPool({
  title,
  tone,
  rows,
  selectedSymbols,
  onToggle,
  onToggleAll,
}: {
  title: string
  tone: 'positive' | 'negative'
  rows: SymbolPnlSummary[]
  selectedSymbols: ReadonlySet<string>
  onToggle: (symbol: string) => void
  onToggleAll: (symbols: string[]) => void
}) {
  const selectedCount = rows.filter((row) => selectedSymbols.has(row.symbol)).length
  const allSelected = rows.length > 0 && selectedCount === rows.length

  return (
    <section className={'pnl-symbol-pool pnl-symbol-pool--' + tone}>
      <label className="pnl-symbol-pool__header">
        <input
          type="checkbox"
          checked={allSelected}
          disabled={rows.length === 0}
          onChange={() => onToggleAll(rows.map((row) => row.symbol))}
        />
        <span>{title}</span>
        <small>{selectedCount} / {rows.length}</small>
      </label>
      <div className="pnl-symbol-pool__list">
        {rows.map((row) => (
          <label
            className={
              'pnl-symbol-option' +
              (selectedSymbols.has(row.symbol) ? ' is-selected' : '')
            }
            key={row.symbol}
          >
            <input
              type="checkbox"
              checked={selectedSymbols.has(row.symbol)}
              onChange={() => onToggle(row.symbol)}
            />
            <strong>{row.symbol}</strong>
            <span className={valueClass(row.totalPnlUsdt)}>
              {money(row.totalPnlUsdt, true)}
            </span>
          </label>
        ))}
        {rows.length === 0 && <span className="pnl-symbol-pool__empty">暂无币种</span>}
      </div>
    </section>
  )
}

export function PnlStrategyPage({ readOnly }: { readOnly: boolean }) {
  const { slug = '' } = useParams()
  const [strategy, setStrategy] = useState<Strategy | null>(null)
  const [pnl, setPnl] = useState<StrategyPnl | null>(null)
  const [snapshotHistory, setSnapshotHistory] = useState<StrategySnapshotSummary[]>([])
  const [initialSnapshot, setInitialSnapshotState] = useState<StrategySnapshotSummary | null>(null)
  const [selectedSnapshotTs, setSelectedSnapshotTs] = useState<number | null>(null)
  const [snapshotBusy, setSnapshotBusy] = useState(false)
  const [startInput, setStartInput] = useState('')
  const [endInput, setEndInput] = useState('')
  const [startMs, setStartMs] = useState<number | null>(null)
  const [endMs, setEndMs] = useState<number | null>(null)
  const [selectedSymbols, setSelectedSymbols] = useState<string[] | null>(null)
  const [symbolSelection, setSymbolSelection] =
    useState<PnlSymbolSelection>('all')
  const [visibleSeries, setVisibleSeries] = useState<PnlSeriesKey[]>([
    'totalPnlUsdt',
    'feeAfterPnlUsdt',
    'fundingPnlUsdt',
    'interestCostUsdt',
  ])
  const [chartMode, setChartMode] = useState<'portfolio' | 'symbols'>(
    'portfolio',
  )
  const [positionDisplayMode, setPositionDisplayMode] = useState<
    'positions' | 'exposure'
  >('positions')
  const [positionChartMode, setPositionChartMode] = useState<
    'portfolio' | 'symbols'
  >('symbols')
  const [positionUnit, setPositionUnit] = useState<PositionUnit>('qty')
  const [visiblePositionSeries, setVisiblePositionSeries] = useState<
    PositionSeriesKey[]
  >(['spotPositionUsdt', 'futuresPositionUsdt'])
  const [symbolSearch, setSymbolSearch] = useState('')
  const [strategyError, setStrategyError] = useState<string | null>(null)
  const [pnlError, setPnlError] = useState<string | null>(null)
  const [loadingPnl, setLoadingPnl] = useState(false)
  const [alignmentStatus, setAlignmentStatus] =
    useState<AlignmentStatus | null>(null)

  useEffect(() => {
    setStrategy(null)
    setPnl(null)
    setSnapshotHistory([])
    setInitialSnapshotState(null)
    setSelectedSnapshotTs(null)
    setStrategyError(null)
    Promise.all([
      getStrategy(slug),
      getStrategySnapshots(slug),
      getInitialSnapshot(slug),
    ])
      .then(([nextStrategy, snapshots, selectedSnapshot]) => {
        const now = Date.now()
        const effectiveStart = selectedSnapshot?.snapshotTsMs ?? nextStrategy.stMs
        setStrategy(nextStrategy)
        setSnapshotHistory(snapshots)
        setInitialSnapshotState(selectedSnapshot)
        setSelectedSnapshotTs(selectedSnapshot?.snapshotTsMs ?? snapshots[0]?.snapshotTsMs ?? null)
        const defaultStart = Math.max(
          effectiveStart,
          now - DEFAULT_RANGE_DAYS * DAY_MS,
        )
        setStartInput(toDatetimeLocal(defaultStart))
        setEndInput(toDatetimeLocal(now))
        setStartMs(defaultStart)
        setEndMs(now)
        setSelectedSymbols(null)
        setSymbolSelection('all')
      })
      .catch((reason: unknown) => {
        setStrategyError(reason instanceof Error ? reason.message : String(reason))
      })
  }, [slug])

  useEffect(() => {
    const controller = new AbortController()
    setAlignmentStatus(null)
    const refresh = () => {
      getAlignmentStatuses(controller.signal)
        .then((statuses) => {
          setAlignmentStatus(
            statuses.find((status) => status.strategySlug === slug) ?? null,
          )
        })
        .catch(() => undefined)
    }
    refresh()
    const timer = window.setInterval(refresh, 3_000)
    return () => {
      controller.abort()
      window.clearInterval(timer)
    }
  }, [slug])

  useEffect(() => {
    if (!strategy || startMs === null || endMs === null) return
    if (selectedSymbols?.length === 0) {
      setLoadingPnl(false)
      setPnlError(null)
      return
    }
    const controller = new AbortController()
    setLoadingPnl(true)
    setPnlError(null)
    getStrategyPnl(strategy.slug, {
      startMs,
      endMs,
      symbols: selectedSymbols ?? undefined,
      maxPoints: 3500,
      signal: controller.signal,
    })
      .then(setPnl)
      .catch((reason: unknown) => {
        if (reason instanceof DOMException && reason.name === 'AbortError') return
        setPnlError(reason instanceof Error ? reason.message : String(reason))
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoadingPnl(false)
      })
    return () => controller.abort()
  }, [strategy, startMs, endMs, selectedSymbols])

  const selectedSet = useMemo(
    () =>
      new Set(
        selectedSymbols ??
          pnl?.selectedSymbols ??
          pnl?.availableSymbols ??
          [],
      ),
    [pnl, selectedSymbols],
  )

  const filteredSymbols = useMemo(() => {
    if (!pnl) return []
    const search = symbolSearch.trim().toUpperCase()
    return search
      ? pnl.symbols.filter((row) => row.symbol.includes(search))
      : pnl.symbols
  }, [pnl, symbolSearch])

  const positiveSymbols = useMemo(
    () =>
      (pnl?.symbols ?? [])
        .filter((row) => row.totalPnlUsdt >= -PNL_EPSILON)
        .sort(
          (left, right) =>
            right.totalPnlUsdt - left.totalPnlUsdt ||
            left.symbol.localeCompare(right.symbol),
        ),
    [pnl],
  )
  const negativeSymbols = useMemo(
    () =>
      (pnl?.symbols ?? [])
        .filter((row) => row.totalPnlUsdt < -PNL_EPSILON)
        .sort(
          (left, right) =>
            left.totalPnlUsdt - right.totalPnlUsdt ||
            left.symbol.localeCompare(right.symbol),
        ),
    [pnl],
  )

  useEffect(() => {
    if (!pnl || symbolSelection === 'all' || symbolSelection === 'custom') return
    const rows = symbolSelection === 'positive' ? positiveSymbols : negativeSymbols
    const next = rows.map((row) => row.symbol).sort()
    const normalized =
      next.length === pnl.availableSymbols.length ? null : next
    setSelectedSymbols((current) => {
      if (current === null || normalized === null) {
        return current === normalized ? current : normalized
      }
      return current.length === normalized.length &&
        current.every((item, index) => item === normalized[index])
        ? current
        : normalized
    })
  }, [negativeSymbols, pnl, positiveSymbols, symbolSelection])

  const effectiveStartMs = initialSnapshot?.snapshotTsMs ?? strategy?.stMs ?? 0

  const activeRangeKey = rangeOptions.find((option) => {
    if (startMs === null || endMs === null) return false
    const expectedStart =
      option.days === null
        ? effectiveStartMs
        : Math.max(effectiveStartMs, endMs - option.days * DAY_MS)
    return Math.abs(startMs - expectedStart) < 60_000
  })?.key

  function applyRange() {
    if (!strategy) return
    const nextStart = fromDatetimeLocal(startInput)
    const nextEnd = fromDatetimeLocal(endInput)
    if (!Number.isFinite(nextStart) || !Number.isFinite(nextEnd)) {
      setPnlError('请选择有效时间')
      return
    }
    if (nextStart < effectiveStartMs || nextEnd < nextStart) {
      setPnlError('时间范围无效')
      return
    }
    setStartMs(nextStart)
    setEndMs(nextEnd)
    if (selectedSymbols?.length === 0) {
      setSymbolSelection('all')
      setSelectedSymbols(null)
    }
  }

  function selectRange(days: number | null) {
    if (!strategy) return
    const nextEnd = fromDatetimeLocal(endInput) || Date.now()
    const nextStart =
      days === null
        ? effectiveStartMs
        : Math.max(effectiveStartMs, nextEnd - days * DAY_MS)
    setStartInput(toDatetimeLocal(nextStart))
    setEndInput(toDatetimeLocal(nextEnd))
    setStartMs(nextStart)
    setEndMs(nextEnd)
    if (selectedSymbols?.length === 0) {
      setSymbolSelection('all')
      setSelectedSymbols(null)
    }
  }

  async function applyInitialSnapshot() {
    if (!strategy || selectedSnapshotTs === null) return
    setSnapshotBusy(true)
    setPnlError(null)
    try {
      const selected = await setInitialSnapshot(strategy.slug, selectedSnapshotTs)
      setInitialSnapshotState(selected)
      setStartInput(toDatetimeLocal(selected.snapshotTsMs))
      setStartMs(selected.snapshotTsMs)
      setSelectedSymbols(null)
      setSymbolSelection('all')
      setPnl(null)
    } catch (reason: unknown) {
      setPnlError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      setSnapshotBusy(false)
    }
  }

  async function removeInitialSnapshot() {
    if (!strategy) return
    setSnapshotBusy(true)
    setPnlError(null)
    try {
      await clearInitialSnapshot(strategy.slug)
      setInitialSnapshotState(null)
      setStartInput(toDatetimeLocal(strategy.stMs))
      setStartMs(strategy.stMs)
      setSelectedSymbols(null)
      setSymbolSelection('all')
      setPnl(null)
    } catch (reason: unknown) {
      setPnlError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      setSnapshotBusy(false)
    }
  }

  function toggleSymbol(symbol: string) {
    if (!pnl) return
    setSymbolSelection('custom')
    if (selectedSymbols === null) {
      const next = pnl.availableSymbols.filter((item) => item !== symbol)
      if (next.length) setSelectedSymbols(next)
      return
    }
    const exists = selectedSymbols.includes(symbol)
    const next = exists
      ? selectedSymbols.filter((item) => item !== symbol)
      : [...selectedSymbols, symbol].sort()
    setSelectedSymbols(
      next.length === pnl.availableSymbols.length ? null : next,
    )
  }

  function selectSymbolGroup(
    selection: Exclude<PnlSymbolSelection, 'custom'>,
  ) {
    if (!pnl) return
    setSymbolSelection(selection)
    if (selection === 'all') {
      setSelectedSymbols(null)
      return
    }
    const rows = selection === 'positive' ? positiveSymbols : negativeSymbols
    const next = rows.map((row) => row.symbol).sort()
    setSelectedSymbols(
      next.length === pnl.availableSymbols.length ? null : next,
    )
  }

  function toggleSymbolPool(symbols: string[]) {
    if (!pnl || symbols.length === 0) return
    setSymbolSelection('custom')
    const next = new Set(selectedSet)
    const allSelected = symbols.every((symbol) => next.has(symbol))
    for (const symbol of symbols) {
      if (allSelected) next.delete(symbol)
      else next.add(symbol)
    }
    const ordered = pnl.availableSymbols.filter((symbol) => next.has(symbol))
    setSelectedSymbols(
      ordered.length === pnl.availableSymbols.length ? null : ordered,
    )
  }

  function toggleSeries(key: PnlSeriesKey) {
    setVisibleSeries((current) => {
      if (current.includes(key)) {
        return current.length === 1
          ? current
          : current.filter((item) => item !== key)
      }
      return [...current, key]
    })
  }

  function togglePositionSeries(key: PositionSeriesKey) {
    setVisiblePositionSeries((current) => {
      if (current.includes(key)) {
        return current.length === 1
          ? current
          : current.filter((item) => item !== key)
      }
      return [...current, key]
    })
  }

  if (strategyError) {
    return (
      <main className="detail-shell">
        <Link className="back-link" to="/">
          <ArrowLeft size={17} />
          返回总览
        </Link>
        <div className="error-state">
          <CircleAlert size={19} />
          <div>
            <strong>盘子加载失败</strong>
            <span>{strategyError}</span>
          </div>
        </div>
      </main>
    )
  }

  if (!strategy) {
    return (
      <main className="detail-shell">
        <div className="detail-loading" />
      </main>
    )
  }

  const summary = pnl?.summary
  const isFuturesOnly =
    strategy.strategyKind === 'market_making' || strategy.strategyKind === 'cta'
  const analysisLink = strategySurfaceAnalysisLink(strategy.slug)
  const activePositionSeries: PositionSeriesKey[] =
    positionDisplayMode === 'exposure'
      ? ['exposureUsdt']
      : isFuturesOnly
        ? ['futuresPositionUsdt']
        : visiblePositionSeries.filter((key) => key !== 'exposureUsdt')
  const activePositionOptions = (
    isFuturesOnly ? marketMakingPositionSeriesOptions : positionSeriesOptions
  ).filter((option) => option.key !== 'exposureUsdt')

  return (
    <>
      <header className="detail-header">
        <div className="detail-header__inner">
          <div className="detail-title">
            <Link
              className="icon-button icon-button--back"
              to="/"
              title="返回总览"
            >
              <ArrowLeft size={18} />
            </Link>
            <span
              className={'exchange-mark exchange-mark--' + strategy.exchange}
              aria-hidden="true"
            >
              {exchangeBadge(strategy.exchange)}
            </span>
            <div>
              <p>{kindLabel(strategy.strategyKind)}</p>
              <h1>{strategy.displayName}</h1>
            </div>
          </div>
          <div className="detail-header-actions">
            {analysisLink && (
              <Link
                className="strategy-card__analysis"
                to={analysisLink.to}
                title="查看组合 FIFO 分析"
              >
                <FlaskConical size={14} />
                <span>{analysisLink.label}</span>
              </Link>
            )}
            {strategy.strategyKind !== 'cta' && (
              <a className="config-button" href={strategy.configUrl}>
                <Settings size={17} />
                配置
                <ExternalLink size={14} />
              </a>
            )}
          </div>
        </div>
      </header>

      <main className="detail-shell pnl-shell">
        <section className="pnl-toolbar" aria-label="PnL 查询范围">
          <div className="date-range">
            <CalendarRange size={18} />
            <label>
              <span>开始</span>
              <input
                type="datetime-local"
                value={startInput}
                min={toDatetimeLocal(effectiveStartMs)}
                max={endInput}
                onChange={(event) => setStartInput(event.target.value)}
              />
            </label>
            <span className="range-separator">至</span>
            <label>
              <span>结束</span>
              <input
                type="datetime-local"
                value={endInput}
                min={startInput}
                onChange={(event) => setEndInput(event.target.value)}
              />
            </label>
            <button className="refresh-button" type="button" onClick={applyRange}>
              <RefreshCw size={15} />
              查询
            </button>
          </div>
          <div className="segmented segmented--compact" aria-label="快捷时间范围">
            {rangeOptions.map((option) => (
              <button
                key={option.key}
                type="button"
                className={activeRangeKey === option.key ? 'is-active' : ''}
                onClick={() => selectRange(option.days)}
              >
                {option.key}
              </button>
            ))}
          </div>
        </section>

        <section className="initial-snapshot-bar" aria-label="初始快照">
          <div className="initial-snapshot-field">
            <Database size={16} />
            <label htmlFor="initial-snapshot-select">初始快照</label>
            <select
              id="initial-snapshot-select"
              value={selectedSnapshotTs ?? ''}
              disabled={readOnly || snapshotBusy || snapshotHistory.length === 0}
              onChange={(event) => setSelectedSnapshotTs(Number(event.target.value))}
            >
              {snapshotHistory.length === 0 && <option value="">暂无快照</option>}
              {snapshotHistory.map((snapshot) => (
                <option key={snapshot.snapshotTsMs} value={snapshot.snapshotTsMs}>
                  {new Date(snapshot.snapshotTsMs).toLocaleString()}
                </option>
              ))}
            </select>
            {!readOnly && (
              <>
                <button
                  type="button"
                  className="snapshot-apply-button"
                  disabled={
                    snapshotBusy ||
                    selectedSnapshotTs === null ||
                    selectedSnapshotTs === initialSnapshot?.snapshotTsMs
                  }
                  onClick={applyInitialSnapshot}
                >
                  设为初始
                </button>
                <button
                  type="button"
                  className="icon-button snapshot-clear-button"
                  disabled={snapshotBusy || initialSnapshot === null}
                  onClick={removeInitialSnapshot}
                  title="清除初始快照"
                  aria-label="清除初始快照"
                >
                  <Trash2 size={15} />
                </button>
              </>
            )}
          </div>
          <span>
            {initialSnapshot
              ? new Date(initialSnapshot.snapshotTsMs).toLocaleString()
              : '未启用'}
          </span>
        </section>

        {alignmentStatus && (
          <section
            className={
              'alignment-detail alignment-detail--' +
              alignmentTone(alignmentStatus)
            }
            aria-label="订单校对状态"
            title={alignmentTitle(alignmentStatus)}
          >
            <GitCompareArrows size={17} />
            <div className="alignment-detail__state">
              <span>订单校对</span>
              <strong>{alignmentLabel(alignmentStatus)}</strong>
            </div>
            <div className="alignment-detail__progress" aria-hidden="true">
              <i
                style={{ width: alignmentStatus.progressPercent + '%' }}
              />
            </div>
            <div className="alignment-detail__time">
              <span>已验证终点</span>
              <strong>{alignmentTime(alignmentStatus)}</strong>
            </div>
          </section>
        )}

        {pnlError && (
          <div className="error-state pnl-error">
            <CircleAlert size={19} />
            <div>
              <strong>PnL 加载失败</strong>
              <span>{pnlError}</span>
            </div>
          </div>
        )}

        <section className="pnl-metrics" aria-label="PnL 汇总">
          <div className="pnl-metric pnl-metric--primary">
            <span>Total PnL</span>
            <strong className={valueClass(summary?.totalPnlUsdt ?? 0)}>
              {summary ? money(summary.totalPnlUsdt, true) : '--'}
            </strong>
            <small>{summary ? money(summary.returnBpsOnVolume, true) : '--'} bps</small>
          </div>
          <div className="pnl-metric">
            <span>已实现 Fee 前</span>
            <strong className={valueClass(summary?.feeBeforePnlUsdt ?? 0)}>
              {summary ? money(summary.feeBeforePnlUsdt, true) : '--'}
            </strong>
            <small>{summary ? compactNumber(summary.tradeCount) : '--'} trades</small>
          </div>
          <div className="pnl-metric">
            <span>实际 Fee</span>
            <strong className={valueClass(-(summary?.tradingFeeUsdt ?? 0))}>
              {summary ? money(-summary.tradingFeeUsdt, true) : '--'}
            </strong>
            <small>USDT</small>
          </div>
          <div className="pnl-metric">
            <span>已实现 Fee 后</span>
            <strong className={valueClass(summary?.feeAfterPnlUsdt ?? 0)}>
              {summary ? money(summary.feeAfterPnlUsdt, true) : '--'}
            </strong>
            <small>USDT</small>
          </div>
          <div className="pnl-metric">
            <span>浮动盈亏</span>
            <strong className={valueClass(summary?.floatingPnlUsdt ?? 0)}>
              {summary ? money(summary.floatingPnlUsdt, true) : '--'}
            </strong>
            <small>USDT</small>
          </div>
          <div className="pnl-metric">
            <span>Funding</span>
            <strong className={valueClass(summary?.fundingPnlUsdt ?? 0)}>
              {summary ? money(summary.fundingPnlUsdt, true) : '--'}
            </strong>
            <small>{pnl ? compactNumber(pnl.source.loadedFundingRows) : '--'} rows</small>
          </div>
          <div className="pnl-metric">
            <span>Interest</span>
            <strong className={(summary?.interestCostUsdt ?? 0) > 0 ? 'is-negative' : ''}>
              {summary ? money(-summary.interestCostUsdt, true) : '--'}
            </strong>
            <small>{pnl ? compactNumber(pnl.source.loadedInterestRows) : '--'} rows</small>
          </div>
        </section>

        <section className="chart-panel pnl-chart-panel">
          <div className="chart-panel__header pnl-chart-header">
            <div>
              <p className="eyebrow">PNL TIMELINE</p>
              <h2>收益曲线</h2>
            </div>
            <div className="chart-controls">
              <div className="segmented segmented--compact" aria-label="收益曲线视图">
                <button
                  type="button"
                  className={chartMode === 'portfolio' ? 'is-active' : ''}
                  onClick={() => setChartMode('portfolio')}
                >
                  组合
                </button>
                <button
                  type="button"
                  className={chartMode === 'symbols' ? 'is-active' : ''}
                  onClick={() => setChartMode('symbols')}
                >
                  分币
                </button>
              </div>
              {chartMode === 'symbols' && (
                <span className="symbol-series-count">
                  {selectedSet.size} 条币种曲线
                </span>
              )}
            </div>
          </div>
          <div className="chart-body has-picker">
            <div className="chart-stage">
              {pnl && (
                <PnlChart
                  points={pnl.points}
                  symbolPoints={
                    selectedSymbols?.length === 0 ? [] : pnl.symbolPoints
                  }
                  visibleSeries={visibleSeries}
                  mode={chartMode}
                />
              )}
              {loadingPnl && (
                <div className="chart-loading">
                  <LoaderCircle size={20} />
                  <span>计算中</span>
                </div>
              )}
            </div>
            {chartMode === 'portfolio' ? (
              <aside className="symbol-curve-picker" aria-label="PnL 曲线选择">
                <div className="symbol-curve-picker__header">
                  <strong>PNL</strong>
                </div>
                <div className="symbol-curve-picker__list">
                  {seriesOptions.map((option) => (
                    <label key={option.key}>
                      <input
                        type="checkbox"
                        checked={visibleSeries.includes(option.key)}
                        onChange={() => toggleSeries(option.key)}
                      />
                      <span
                        className="series-swatch"
                        style={{ backgroundColor: option.color }}
                      />
                      <span>{option.label}</span>
                    </label>
                  ))}
                </div>
              </aside>
            ) : pnl ? (
              <aside className="pnl-symbol-selector" aria-label="分币曲线选择">
                <div className="pnl-symbol-selector__header">
                  <div>
                    <span>币种曲线</span>
                    <strong>{selectedSet.size} / {pnl.availableSymbols.length}</strong>
                  </div>
                  <div
                    className="segmented segmented--compact pnl-symbol-filter"
                    aria-label="按 Total PnL 筛选币种"
                  >
                    <button
                      type="button"
                      className={symbolSelection === 'all' ? 'is-active' : ''}
                      onClick={() => selectSymbolGroup('all')}
                    >
                      全部
                    </button>
                    <button
                      type="button"
                      className={symbolSelection === 'positive' ? 'is-active' : ''}
                      disabled={positiveSymbols.length === 0}
                      onClick={() => selectSymbolGroup('positive')}
                    >
                      盈利
                    </button>
                    <button
                      type="button"
                      className={symbolSelection === 'negative' ? 'is-active' : ''}
                      disabled={negativeSymbols.length === 0}
                      onClick={() => selectSymbolGroup('negative')}
                    >
                      亏损
                    </button>
                  </div>
                </div>
                <div className="pnl-symbol-pools">
                  <PnlSymbolPool
                    title="盈利 / 持平"
                    tone="positive"
                    rows={positiveSymbols}
                    selectedSymbols={selectedSet}
                    onToggle={toggleSymbol}
                    onToggleAll={toggleSymbolPool}
                  />
                  <PnlSymbolPool
                    title="亏损"
                    tone="negative"
                    rows={negativeSymbols}
                    selectedSymbols={selectedSet}
                    onToggle={toggleSymbol}
                    onToggleAll={toggleSymbolPool}
                  />
                </div>
              </aside>
            ) : null}
          </div>
          {pnl && (
            <div className="chart-foot">
              <span>
                <Database size={13} />
                {compactNumber(pnl.source.loadedTradeRows)} trades
              </span>
              <span>
                {compactNumber(
                  chartMode === 'portfolio'
                    ? pnl.source.returnedPoints
                    : pnl.source.returnedSymbolPoints,
                )}{' '}
                15min ticks
              </span>
              <span>{selectedSet.size} symbols</span>
              {pnl.source.initialSnapshotTsMs !== null && (
                <span>
                  snapshot {compactNumber(pnl.source.initialPositionCount)} symbols
                </span>
              )}
              {pnl.source.skippedInitialPositionCount > 0 && (
                <span>unpriced {pnl.source.skippedInitialPositionCount}</span>
              )}
              {pnl.source.sampled && <span>sampled</span>}
            </div>
          )}
        </section>

        {(strategy.strategyKind === 'funding_rate' ||
          strategy.strategyKind === 'intra_exchange' ||
          isFuturesOnly) && (
          <section className="chart-panel pnl-chart-panel">
            <div className="chart-panel__header pnl-chart-header">
              <div>
                <p className="eyebrow">POSITION / EXPOSURE</p>
                <h2>
                  {positionDisplayMode === 'exposure'
                    ? '净敞口'
                    : isFuturesOnly
                      ? '合约仓位'
                      : 'Spot / Swap 仓位'}
                </h2>
              </div>
              <div className="chart-controls">
                <div
                  className="segmented segmented--compact"
                  aria-label="仓位图计量单位"
                >
                  <button
                    type="button"
                    className={positionUnit === 'usdt' ? 'is-active' : ''}
                    onClick={() => setPositionUnit('usdt')}
                  >
                    USDT
                  </button>
                  <button
                    type="button"
                    className={positionUnit === 'qty' ? 'is-active' : ''}
                    onClick={() => {
                      setPositionUnit('qty')
                      setPositionChartMode('symbols')
                    }}
                  >
                    Qty
                  </button>
                </div>
                <div
                  className="segmented segmented--compact"
                  aria-label="仓位图显示指标"
                >
                  <button
                    type="button"
                    className={
                      positionDisplayMode === 'positions' ? 'is-active' : ''
                    }
                    onClick={() => setPositionDisplayMode('positions')}
                  >
                    仓位
                  </button>
                  <button
                    type="button"
                    className={
                      positionDisplayMode === 'exposure' ? 'is-active' : ''
                    }
                    onClick={() => setPositionDisplayMode('exposure')}
                  >
                    敞口
                  </button>
                </div>
                <div
                  className="segmented segmented--compact"
                  aria-label="仓位曲线视图"
                >
                  <button
                    type="button"
                    className={positionChartMode === 'portfolio' ? 'is-active' : ''}
                    disabled={positionUnit === 'qty'}
                    onClick={() => setPositionChartMode('portfolio')}
                  >
                    组合
                  </button>
                  <button
                    type="button"
                    className={positionChartMode === 'symbols' ? 'is-active' : ''}
                    onClick={() => setPositionChartMode('symbols')}
                  >
                    {positionDisplayMode === 'exposure' ? '币种组成' : '分币'}
                  </button>
                </div>
              </div>
            </div>
            <div className="chart-body has-picker">
              <div className="chart-stage">
                {pnl && (
                  <PositionChart
                    points={pnl.points}
                    symbolPoints={
                      selectedSymbols?.length === 0 ? [] : pnl.symbolPoints
                    }
                    visibleSeries={activePositionSeries}
                    mode={positionChartMode}
                    unit={positionUnit}
                  />
                )}
                {loadingPnl && (
                  <div className="chart-loading">
                    <LoaderCircle size={20} />
                    <span>计算中</span>
                  </div>
                )}
              </div>
              {positionChartMode === 'portfolio' ? (
                <aside className="symbol-curve-picker" aria-label="仓位曲线选择">
                  <div className="symbol-curve-picker__header">
                    <strong>
                      {positionDisplayMode === 'exposure' ? '敞口方向' : 'USDT 仓位'}
                    </strong>
                  </div>
                  <div className="symbol-curve-picker__list">
                    {positionDisplayMode === 'positions' ? (
                      activePositionOptions.map((option) => (
                        <label key={option.key}>
                          <input
                            type="checkbox"
                            checked={visiblePositionSeries.includes(option.key)}
                            disabled={isFuturesOnly}
                            onChange={() => togglePositionSeries(option.key)}
                          />
                          <span
                            className={
                              option.lineType === 'dashed'
                                ? 'signed-line-swatch is-dashed'
                                : 'signed-line-swatch'
                            }
                            aria-hidden="true"
                          >
                            <i />
                            <i />
                          </span>
                          <span>{option.label}</span>
                        </label>
                      ))
                    ) : (
                      <div className="position-sign-key">
                        <span><i className="is-positive" />正值</span>
                        <span><i className="is-negative" />负值</span>
                      </div>
                    )}
                  </div>
                </aside>
              ) : pnl ? (
                <aside className="symbol-curve-picker" aria-label="分币仓位选择">
                  <div className="symbol-curve-picker__header">
                    <strong>币种</strong>
                    <div className="symbol-curve-picker__actions">
                      <button
                        type="button"
                        onClick={() => selectSymbolGroup('all')}
                        disabled={selectedSymbols === null}
                      >
                        全选
                      </button>
                      <button
                        type="button"
                        onClick={() => {
                          setSymbolSelection('custom')
                          setSelectedSymbols([])
                        }}
                        disabled={selectedSymbols?.length === 0}
                      >
                        全不选
                      </button>
                    </div>
                  </div>
                  <div className="symbol-curve-picker__list">
                    {pnl.availableSymbols.map((symbol) => (
                      <label key={symbol}>
                        <input
                          type="checkbox"
                          checked={selectedSet.has(symbol)}
                          onChange={() => toggleSymbol(symbol)}
                        />
                        <span>{symbol}</span>
                      </label>
                    ))}
                  </div>
                </aside>
              ) : null}
            </div>
            {pnl && (
              <div className="chart-foot">
                <span>{positionUnit === 'usdt' ? 'USDT NOTIONAL' : 'BASE QTY'}</span>
                <span>
                  {positionDisplayMode === 'exposure'
                    ? 'EXPOSURE = Σ SIGNED VENUE POSITION'
                    : 'SIGNED VENUE POSITION'}
                </span>
                <span>{selectedSet.size} symbols</span>
              </div>
            )}
          </section>
        )}

        {summary &&
          (summary.unconvertedFeeCount > 0 ||
            summary.unconvertedInterestCount > 0) && (
            <div className="data-warning">
              <CircleAlert size={16} />
              未折算：fee {summary.unconvertedFeeCount} 条，interest{' '}
              {summary.unconvertedInterestCount} 条
            </div>
          )}

        <section className="symbol-panel">
          <div className="symbol-panel__header">
            <div>
              <p className="eyebrow">SYMBOL BREAKDOWN</p>
              <h2>分币收益</h2>
            </div>
            <div className="symbol-actions">
              <label className="search-input">
                <Search size={15} />
                <input
                  value={symbolSearch}
                  onChange={(event) => setSymbolSearch(event.target.value)}
                  placeholder="搜索币种"
                  aria-label="搜索币种"
                />
              </label>
              <button
                type="button"
                className="all-symbols-button"
                onClick={() => selectSymbolGroup('all')}
                disabled={selectedSymbols === null}
              >
                全部币种
              </button>
            </div>
          </div>
          <div className="symbol-table-wrap">
            <table className="symbol-table">
              <thead>
                <tr>
                  <th>Symbol</th>
                  <th>Trades</th>
                  <th>Volume</th>
                  <th>已实现 Fee 前</th>
                  <th>已实现 Fee 后</th>
                  <th>Funding</th>
                  <th>Interest</th>
                  <th>Total</th>
                </tr>
              </thead>
              <tbody>
                {filteredSymbols.map((row) => (
                  <SymbolRow
                    key={row.symbol}
                    row={row}
                    selected={selectedSet.has(row.symbol)}
                    onToggle={() => toggleSymbol(row.symbol)}
                  />
                ))}
              </tbody>
            </table>
          </div>
        </section>
      </main>
    </>
  )
}
