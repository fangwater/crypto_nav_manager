import {
  ArrowLeft,
  ArrowRight,
  CalendarRange,
  CircleAlert,
  Database,
  FlaskConical,
  Layers3,
  LoaderCircle,
  RefreshCw,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link, useParams } from 'react-router-dom'
import { getIntraAnalysis, getIntraHourlyLatency, getStrategy } from '../api'
import { AnalysisMetricHelper } from '../components/AnalysisMetricHelper'
import {
  IntraFifoChart,
  type IntraFifoChartMode,
} from '../components/IntraFifoChart'
import { IntraLatencyChart } from '../components/IntraLatencyChart'
import { analysisMetricHelpForStrategy } from '../analysisMetricHelp'
import { intraAnalysisHref, intraAnalysisIncludesClosedCarry, suggestIntraAnalysisSlug } from '../analysisNav'
import {
  intraFeeModeConfig,
  intraFeeModeOptions,
  intraSymbolColor,
} from '../intraAnalysisSeries'
import {
  LATENCY_CHART_FAMILIES,
  LATENCY_CHART_LINES,
  LATENCY_LINE_FILTERS,
  defaultLatencyLineKeys,
  latencyLineFilterFromKeys,
  latencyLineKeysForFilter,
  toggleLatencyFamilyKeys,
  toggleLatencyLineKey,
  type HourlyLatencySeries,
  type LatencyFamilyKey,
} from '../analysisLatencyChart'
import type {
  IntraAnalysis,
  IntraAnalysisSummary,
  IntraArbDirection,
  IntraClosedMatch,
  IntraDirectionSlice,
  IntraFeeMode,
  IntraSymbolAnalysis,
  Strategy,
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

type ChartSymbolSelection = 'all' | 'positive' | 'negative' | 'custom'

interface ChartSymbolRow {
  symbol: string
  value: number
  color: string
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

function quantity(value: number) {
  return value.toLocaleString('en-US', {
    minimumFractionDigits: 0,
    maximumFractionDigits: 8,
  })
}

function compactNumber(value: number) {
  return value.toLocaleString('en-US', { maximumFractionDigits: 0 })
}

function bps(value: number, signed = false) {
  const formatted = Math.abs(value).toLocaleString('en-US', {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  })
  if (signed && value !== 0) return (value > 0 ? '+' : '-') + formatted
  return (value < 0 ? '-' : '') + formatted
}

function percentage(value: number) {
  return (value * 100).toLocaleString('en-US', {
    minimumFractionDigits: 1,
    maximumFractionDigits: 1,
  }) + '%'
}

function duration(ms: number) {
  if (ms < 1_000) return compactNumber(ms) + ' ms'
  if (ms < 60_000) return (ms / 1_000).toFixed(1) + ' s'
  if (ms < 3_600_000) return (ms / 60_000).toFixed(1) + ' min'
  if (ms < 86_400_000) return (ms / 3_600_000).toFixed(1) + ' h'
  return (ms / 86_400_000).toFixed(1) + ' d'
}

function valueClass(value: number) {
  if (value > 1e-9) return 'is-positive'
  if (value < -1e-9) return 'is-negative'
  return ''
}

interface FeeResult {
  grossPnlUsdt: number
  grossReturnBps: number
  tradingFeeUsdt: number
  feeAfterPnlUsdt: number
  feeAfterReturnBps: number
  referenceTradingFeeUsdt: number
  referenceFeeAfterPnlUsdt: number
  referenceFeeAfterReturnBps: number
}

function feeModePnl(row: FeeResult, mode: IntraFeeMode) {
  if (mode === 'actual') return row.feeAfterPnlUsdt
  if (mode === 'reference') return row.referenceFeeAfterPnlUsdt
  return row.grossPnlUsdt
}

function feeModeReturnBps(row: FeeResult, mode: IntraFeeMode) {
  if (mode === 'actual') return row.feeAfterReturnBps
  if (mode === 'reference') return row.referenceFeeAfterReturnBps
  return row.grossReturnBps
}

function feeModeImpact(row: FeeResult, mode: IntraFeeMode) {
  if (mode === 'actual') return -row.tradingFeeUsdt
  if (mode === 'reference') return -row.referenceTradingFeeUsdt
  return 0
}

function feeModeImpactLabel(mode: IntraFeeMode) {
  if (mode === 'actual') return '实际 Fee 影响'
  if (mode === 'reference') return '参考 Fee 影响'
  return 'Fee 影响'
}

interface CaptureBuckets {
  aboveShare: number
  belowShare: number
  uncapturedShare: number
  aboveNotional: number
  belowNotional: number
  uncapturedNotional: number
}

function captureBuckets(slice: IntraDirectionSlice, mode: IntraFeeMode): CaptureBuckets {
  if (mode === 'actual') {
    return {
      aboveShare: slice.capturedAboveFeeShare,
      belowShare: slice.capturedBelowFeeShare,
      uncapturedShare: slice.uncapturedShare,
      aboveNotional: slice.capturedAboveFeeNotionalUsdt,
      belowNotional: slice.capturedBelowFeeNotionalUsdt,
      uncapturedNotional: slice.uncapturedNotionalUsdt,
    }
  }
  if (mode === 'reference') {
    return {
      aboveShare: slice.capturedAboveReferenceFeeShare,
      belowShare: slice.capturedBelowReferenceFeeShare,
      uncapturedShare: slice.uncapturedShare,
      aboveNotional: slice.capturedAboveReferenceFeeNotionalUsdt,
      belowNotional: slice.capturedBelowReferenceFeeNotionalUsdt,
      uncapturedNotional: slice.uncapturedNotionalUsdt,
    }
  }
  return {
    aboveShare: slice.capturedPositiveShare,
    belowShare: 0,
    uncapturedShare: slice.uncapturedShare,
    aboveNotional: slice.capturedPositiveNotionalUsdt,
    belowNotional: 0,
    uncapturedNotional: slice.uncapturedNotionalUsdt,
  }
}

function combinedCapture(summary: IntraAnalysisSummary, mode: IntraFeeMode): CaptureBuckets {
  const positive = captureBuckets(summary.positive, mode)
  const reverse = captureBuckets(summary.reverse, mode)
  const notional = summary.matchedNotionalUsdt
  const aboveNotional = positive.aboveNotional + reverse.aboveNotional
  const belowNotional = positive.belowNotional + reverse.belowNotional
  const uncapturedNotional = positive.uncapturedNotional + reverse.uncapturedNotional
  if (Math.abs(notional) <= PNL_EPSILON) {
    return {
      aboveShare: 0,
      belowShare: 0,
      uncapturedShare: 0,
      aboveNotional,
      belowNotional,
      uncapturedNotional,
    }
  }
  return {
    aboveShare: aboveNotional / notional,
    belowShare: belowNotional / notional,
    uncapturedShare: uncapturedNotional / notional,
    aboveNotional,
    belowNotional,
    uncapturedNotional,
  }
}

function matchFeeModePnl(row: IntraClosedMatch, mode: IntraFeeMode) {
  if (mode === 'actual') return row.feeAfterPnlUsdt
  if (mode === 'reference') return row.referenceFeeAfterPnlUsdt
  return row.grossPnlUsdt
}

function matchFeeModeImpact(row: IntraClosedMatch, mode: IntraFeeMode) {
  if (mode === 'actual') return -row.tradingFeeUsdt
  if (mode === 'reference') return -row.referenceTradingFeeUsdt
  return 0
}

function matchFeeModeReturnBps(row: IntraClosedMatch, mode: IntraFeeMode) {
  const matchedNotional = row.quantity * (row.openSpotPrice + row.closeSpotPrice) / 2
  return matchedNotional === 0 ? 0 : matchFeeModePnl(row, mode) / matchedNotional * 10_000
}

function pairLabel(direction: IntraArbDirection) {
  return direction === 'positive' ? '正 → 反' : '反 → 正'
}

function pendingDirectionLabel(direction: IntraArbDirection) {
  return direction === 'positive' ? '正套待平' : '反套待平'
}

function unpairedNotional(summary: IntraAnalysisSummary) {
  return summary.positiveOpenNotionalUsdt + summary.reverseOpenNotionalUsdt
}

function unpairedQuantity(summary: IntraAnalysisSummary) {
  return summary.positiveOpenQuantity + summary.reverseOpenQuantity
}

function hasWindowActivity(row: IntraAnalysisSummary) {
  return (
    row.closedMatchCount > 0 ||
    unpairedNotional(row) > PNL_EPSILON ||
    unpairedQuantity(row) > PNL_EPSILON
  )
}

function exchangeBadge(exchange: Strategy['exchange']) {
  return exchange === 'bybit' ? 'BY' : 'BN'
}

interface SymbolPoolProps {
  title: string
  tone: 'positive' | 'negative'
  rows: ChartSymbolRow[]
  selectedSymbols: Set<string>
  onToggle: (symbol: string) => void
  onToggleAll: (symbols: string[]) => void
}

function SymbolPool({
  title,
  tone,
  rows,
  selectedSymbols,
  onToggle,
  onToggleAll,
}: SymbolPoolProps) {
  const selectedCount = rows.filter((row) => selectedSymbols.has(row.symbol)).length
  const allSelected = rows.length > 0 && selectedCount === rows.length

  return (
    <section className={'analysis-symbol-pool analysis-symbol-pool--' + tone}>
      <label className="analysis-symbol-pool__header">
        <input
          type="checkbox"
          checked={allSelected}
          disabled={rows.length === 0}
          onChange={() => onToggleAll(rows.map((row) => row.symbol))}
        />
        <span>{title}</span>
        <small>{selectedCount} / {rows.length}</small>
      </label>
      <div className="analysis-symbol-pool__list">
        {rows.map((row) => (
          <label
            className={
              'analysis-symbol-option' +
              (selectedSymbols.has(row.symbol) ? ' is-selected' : '')
            }
            key={row.symbol}
          >
            <input
              type="checkbox"
              checked={selectedSymbols.has(row.symbol)}
              onChange={() => onToggle(row.symbol)}
            />
            <span
              className="analysis-symbol-option__swatch"
              style={{ backgroundColor: row.color }}
              aria-hidden="true"
            />
            <strong>{row.symbol}</strong>
            <span className={valueClass(row.value)}>{money(row.value, true)}</span>
          </label>
        ))}
        {rows.length === 0 && <span className="analysis-symbol-pool__empty">暂无币种</span>}
      </div>
    </section>
  )
}

function LatencyFamily({
  family,
  name,
  selectedKeys,
  onToggleLine,
  onToggleFamily,
}: {
  family: LatencyFamilyKey
  name: string
  selectedKeys: ReadonlySet<string>
  onToggleLine: (key: string) => void
  onToggleFamily: (family: LatencyFamilyKey) => void
}) {
  const rows = LATENCY_CHART_LINES.filter((line) => line.family === family)
  const selectedCount = rows.filter((line) => selectedKeys.has(line.key)).length
  const allSelected = rows.length > 0 && selectedCount === rows.length

  return (
    <section className="analysis-symbol-pool analysis-latency-family">
      <label className="analysis-symbol-pool__header">
        <input
          type="checkbox"
          checked={allSelected}
          onChange={() => onToggleFamily(family)}
        />
        <span>{name}</span>
        <small>{selectedCount} / {rows.length}</small>
      </label>
      <div className="analysis-symbol-pool__list">
        {rows.map((line) => (
          <label
            className={
              'analysis-symbol-option' +
              (selectedKeys.has(line.key) ? ' is-selected' : '')
            }
            key={line.key}
          >
            <input
              type="checkbox"
              checked={selectedKeys.has(line.key)}
              onChange={() => onToggleLine(line.key)}
            />
            <span
              className={
                'analysis-symbol-option__swatch' +
                (line.dashed ? ' analysis-symbol-option__swatch--dashed' : '')
              }
              style={{
                backgroundColor: line.color,
                borderTopColor: line.color,
              }}
              aria-hidden="true"
            />
            <strong>{line.quantileLabel}</strong>
          </label>
        ))}
      </div>
    </section>
  )
}

function SymbolRow({
  row,
  feeMode,
  includeClosedCarry,
}: {
  row: IntraSymbolAnalysis
  feeMode: IntraFeeMode
  includeClosedCarry: boolean
}) {
  const pnl = feeModePnl(row, feeMode)
  const feeImpact = feeModeImpact(row, feeMode)
  return (
    <tr>
      <td><strong>{row.symbol}</strong></td>
      <td>
        <strong>{compactNumber(row.closedMatchCount)}</strong>
        <small>{percentage(row.winRate)} win</small>
      </td>
      <td>{money(row.matchedNotionalUsdt)}</td>
      <td className={valueClass(pnl)}>
        <strong>{money(pnl, true)}</strong>
        <small>{bps(feeModeReturnBps(row, feeMode), true)} bps</small>
      </td>
      <td className={valueClass(feeImpact)}>
        <strong>{money(feeImpact, true)}</strong>
        <small>
          {feeMode === 'actual'
            ? `${percentage(row.actualFeeCoverage)} covered`
            : feeMode === 'reference'
              ? `${bps(row.referenceFeeBps)} bps`
              : '未计 Fee'}
        </small>
      </td>
      {includeClosedCarry && (
        <>
          <td className={valueClass(row.fundingPnlUsdt)}>
            <strong>{money(row.fundingPnlUsdt, true)}</strong>
            <small>{bps(row.fundingReturnBps, true)} bps</small>
          </td>
          <td className={valueClass(-row.interestCostUsdt)}>
            <strong>{money(-row.interestCostUsdt, true)}</strong>
            <small>{bps(-row.interestCostReturnBps, true)} bps</small>
          </td>
        </>
      )}
      <td>
        <strong>{percentage(combinedCapture(row, feeMode).aboveShare)}</strong>
        <small>
          不够 {percentage(combinedCapture(row, feeMode).belowShare)} · 没兑现{' '}
          {percentage(combinedCapture(row, feeMode).uncapturedShare)}
        </small>
      </td>
    </tr>
  )
}

export function IntraAnalysisPage() {
  const { slug = '' } = useParams()
  const includeClosedCarry = intraAnalysisIncludesClosedCarry(slug)
  const [strategy, setStrategy] = useState<Strategy | null>(null)
  const [analysis, setAnalysis] = useState<IntraAnalysis | null>(null)
  const [latencySeries, setLatencySeries] = useState<HourlyLatencySeries | null>(null)
  const [startInput, setStartInput] = useState('')
  const [endInput, setEndInput] = useState('')
  const [startMs, setStartMs] = useState<number | null>(null)
  const [endMs, setEndMs] = useState<number | null>(null)
  const [referenceFeeInput, setReferenceFeeInput] = useState('1')
  const [referenceFeeBps, setReferenceFeeBps] = useState(1)
  const [symbol, setSymbol] = useState('')
  const [feeMode, setFeeMode] = useState<IntraFeeMode>('actual')
  const [chartMode, setChartMode] = useState<IntraFifoChartMode>('portfolio')
  const [chartSymbolSelection, setChartSymbolSelection] =
    useState<ChartSymbolSelection>('all')
  const [selectedChartSymbols, setSelectedChartSymbols] = useState<string[]>([])
  const [selectedLatencyKeys, setSelectedLatencyKeys] = useState<string[]>(
    defaultLatencyLineKeys,
  )
  const [pageError, setPageError] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)

  useEffect(() => {
    let active = true
    setStrategy(null)
    setAnalysis(null)
    setLatencySeries(null)
    setSymbol('')
    setFeeMode('actual')
    setReferenceFeeInput('1')
    setReferenceFeeBps(1)
    setPageError(null)
    setLoading(true)
    getStrategy(slug)
      .then((nextStrategy) => {
        if (!active) return
        const now = Date.now()
        const defaultStart = Math.max(
          nextStrategy.stMs,
          now - DEFAULT_RANGE_DAYS * DAY_MS,
        )
        setStrategy(nextStrategy)
        setStartInput(toDatetimeLocal(defaultStart))
        setEndInput(toDatetimeLocal(now))
        setStartMs(defaultStart)
        setEndMs(now)
      })
      .catch((reason: unknown) => {
        if (!active) return
        setPageError(reason instanceof Error ? reason.message : String(reason))
        setLoading(false)
      })
    return () => {
      active = false
    }
  }, [slug])

  useEffect(() => {
    if (!strategy || startMs === null || endMs === null) return
    const controller = new AbortController()
    setLoading(true)
    setPageError(null)
    Promise.all([
      getIntraAnalysis(strategy.slug, {
        startMs,
        endMs,
        symbols: symbol ? [symbol] : undefined,
        referenceFeeBps,
        maxPoints: 3500,
        maxMatches: 200,
        signal: controller.signal,
      }),
      getIntraHourlyLatency(strategy.slug, {
        startMs,
        endMs,
        signal: controller.signal,
      }),
    ])
      .then(([nextAnalysis, nextLatency]) => {
        setAnalysis(nextAnalysis)
        setLatencySeries(nextLatency)
      })
      .catch((reason: unknown) => {
        if (reason instanceof DOMException && reason.name === 'AbortError') return
        setPageError(reason instanceof Error ? reason.message : String(reason))
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false)
      })
    return () => controller.abort()
  }, [strategy, startMs, endMs, symbol, referenceFeeBps])

  const availableClosedSymbols = useMemo(
    () =>
      analysis?.symbols
        .filter((row) => hasWindowActivity(row))
        .map((row) => row.symbol) ?? [],
    [analysis],
  )

  const feeModeConfig = intraFeeModeConfig(feeMode)

  const visibleSymbols = useMemo(() => {
    if (!analysis) return []
    const scoped = symbol
      ? analysis.symbols.filter((row) => row.symbol === symbol)
      : analysis.symbols
    return scoped
      .filter((row) => hasWindowActivity(row))
      .sort((left, right) =>
        feeModePnl(right, feeMode) - feeModePnl(left, feeMode) ||
        left.symbol.localeCompare(right.symbol),
      )
  }, [analysis, feeMode, symbol])

  const chartSymbolRows = useMemo<ChartSymbolRow[]>(() => {
    if (!analysis) return []
    const closedSymbols = new Set(
      analysis.symbols
        .filter((row) => row.closedMatchCount > 0)
        .map((row) => row.symbol),
    )
    return analysis.symbolPoints
      .filter((series) => closedSymbols.has(series.symbol))
      .map((series, index) => ({
        symbol: series.symbol,
        value: series.points.at(-1)?.[feeModeConfig.metric] ?? 0,
        color: intraSymbolColor(index),
      }))
  }, [analysis, feeModeConfig.metric])

  const positiveChartSymbols = useMemo(
    () =>
      chartSymbolRows
        .filter((row) => row.value >= -PNL_EPSILON)
        .sort((left, right) => right.value - left.value),
    [chartSymbolRows],
  )
  const negativeChartSymbols = useMemo(
    () =>
      chartSymbolRows
        .filter((row) => row.value < -PNL_EPSILON)
        .sort((left, right) => left.value - right.value),
    [chartSymbolRows],
  )

  useEffect(() => {
    const availableSymbols = new Set(chartSymbolRows.map((row) => row.symbol))
    setSelectedChartSymbols((current) => {
      let next: string[]
      if (chartSymbolSelection === 'all') {
        next = chartSymbolRows.map((row) => row.symbol)
      } else if (chartSymbolSelection === 'positive') {
        next = chartSymbolRows
          .filter((row) => row.value > PNL_EPSILON)
          .map((row) => row.symbol)
      } else if (chartSymbolSelection === 'negative') {
        next = chartSymbolRows
          .filter((row) => row.value < -PNL_EPSILON)
          .map((row) => row.symbol)
      } else {
        next = current.filter((item) => availableSymbols.has(item))
      }
      return next.length === current.length &&
        next.every((item, index) => item === current[index])
        ? current
        : next
    })
  }, [chartSymbolRows, chartSymbolSelection])

  const selectedChartSymbolSet = useMemo(
    () => new Set(selectedChartSymbols),
    [selectedChartSymbols],
  )
  const visibleChartSymbolPoints = useMemo(
    () =>
      analysis?.symbolPoints.filter((series) =>
        selectedChartSymbolSet.has(series.symbol),
      ) ?? [],
    [analysis, selectedChartSymbolSet],
  )
  const chartSymbolColors = useMemo(
    () =>
      Object.fromEntries(
        chartSymbolRows.map((row) => [row.symbol, row.color]),
      ),
    [chartSymbolRows],
  )
  const selectedLatencyKeySet = useMemo(
    () => new Set(selectedLatencyKeys),
    [selectedLatencyKeys],
  )
  const latencyLineFilter = latencyLineFilterFromKeys(selectedLatencyKeys)
  const activeRangeKey = rangeOptions.find((option) => {
    if (!strategy || startMs === null || endMs === null) return false
    const expectedStart =
      option.days === null
        ? strategy.stMs
        : Math.max(strategy.stMs, endMs - option.days * DAY_MS)
    return Math.abs(startMs - expectedStart) < 60_000
  })?.key

  function applyRange() {
    if (!strategy) return
    const nextStart = fromDatetimeLocal(startInput)
    const nextEnd = fromDatetimeLocal(endInput)
    if (
      !Number.isFinite(nextStart) ||
      !Number.isFinite(nextEnd) ||
      nextStart < strategy.stMs ||
      nextEnd < nextStart
    ) {
      setPageError('时间范围无效')
      return
    }
    setStartMs(nextStart)
    setEndMs(nextEnd)
  }

  function selectRange(days: number | null) {
    if (!strategy) return
    const nextEnd = fromDatetimeLocal(endInput) || Date.now()
    const nextStart =
      days === null
        ? strategy.stMs
        : Math.max(strategy.stMs, nextEnd - days * DAY_MS)
    setStartInput(toDatetimeLocal(nextStart))
    setEndInput(toDatetimeLocal(nextEnd))
    setStartMs(nextStart)
    setEndMs(nextEnd)
  }

  function applyReferenceFee() {
    const nextReferenceFeeBps = Number(referenceFeeInput)
    if (
      referenceFeeInput.trim() === '' ||
      !Number.isFinite(nextReferenceFeeBps) ||
      Math.abs(nextReferenceFeeBps) > 100
    ) {
      setPageError('参考 Fee 必须在 -100 至 100 bps 之间')
      return
    }
    setPageError(null)
    setReferenceFeeBps(nextReferenceFeeBps)
    setFeeMode('reference')
  }

  function selectChartSymbols(selection: Exclude<ChartSymbolSelection, 'custom'>) {
    setChartSymbolSelection(selection)
  }

  function toggleChartSymbol(target: string) {
    setChartSymbolSelection('custom')
    setSelectedChartSymbols((current) => {
      const selected = new Set(current)
      if (selected.has(target)) selected.delete(target)
      else selected.add(target)
      return chartSymbolRows
        .map((row) => row.symbol)
        .filter((item) => selected.has(item))
    })
  }

  function toggleChartSymbolPool(targets: string[]) {
    setChartSymbolSelection('custom')
    setSelectedChartSymbols((current) => {
      const selected = new Set(current)
      const allSelected = targets.length > 0 && targets.every((item) => selected.has(item))
      for (const target of targets) {
        if (allSelected) selected.delete(target)
        else selected.add(target)
      }
      return chartSymbolRows
        .map((row) => row.symbol)
        .filter((item) => selected.has(item))
    })
  }

  function selectLatencyLines(filter: (typeof LATENCY_LINE_FILTERS)[number]['key']) {
    setSelectedLatencyKeys(latencyLineKeysForFilter(filter))
  }

  function toggleLatencyLine(key: string) {
    setSelectedLatencyKeys((current) => toggleLatencyLineKey(current, key))
  }

  function toggleLatencyFamily(family: LatencyFamilyKey) {
    setSelectedLatencyKeys((current) => toggleLatencyFamilyKeys(current, family))
  }

  if (!strategy && !pageError) {
    return (
      <main className="detail-shell">
        <div className="detail-loading" />
      </main>
    )
  }

  if (!strategy) {
    const suggestedSlug = suggestIntraAnalysisSlug(slug)
    return (
      <main className="detail-shell">
        <Link className="back-link" to="/">
          <ArrowLeft size={17} />
          返回总览
        </Link>
        <div className="error-state">
          <CircleAlert size={19} />
          <div>
            <strong>研究数据加载失败</strong>
            <span>{pageError}</span>
            {suggestedSlug ? (
              <span>
                你是不是想打开{' '}
                <Link to={intraAnalysisHref(suggestedSlug)}>{suggestedSlug}</Link>
                ？
              </span>
            ) : null}
          </div>
        </div>
      </main>
    )
  }

  const summary = analysis?.summary
  const capture = summary ? combinedCapture(summary, feeMode) : null
  const visiblePendingLots = (analysis?.pendingLots ?? []).filter(
    (lot) => !symbol || lot.symbol === symbol,
  )
  const chartTotal = summary
    ? chartMode === 'portfolio'
      ? feeModePnl(summary, feeMode)
      : visibleChartSymbolPoints.reduce(
          (total, series) =>
            total + (series.points.at(-1)?.[feeModeConfig.metric] ?? 0),
          0,
        )
    : null
  const visibleChartPointCount = visibleChartSymbolPoints.reduce(
    (total, series) => total + series.points.length,
    0,
  )
  return (
    <>
      <header className="detail-header">
        <div className="detail-header__inner">
          <div className="detail-title">
            <Link
              className="icon-button icon-button--back"
              to={'/strategies/' + encodeURIComponent(strategy.slug)}
              title="返回策略净值"
              aria-label="返回策略净值"
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
              <p>正反配对 FIFO</p>
              <h1>{strategy.displayName}</h1>
            </div>
          </div>
          <div className="analysis-header-tags">
            <AnalysisMetricHelper items={analysisMetricHelpForStrategy(slug)} />
            <span title="独立研究口径，不进入正式 NAV">
              <FlaskConical size={14} /> Research
            </span>
            <span
              title={
                includeClosedCarry
                  ? '只有开、平都在选定区间内的正反配对才计入；区间内单边进待配对。闭环数量上还摊 Funding 与 Interest'
                  : '只有开、平都在选定区间内的正反配对才计入；区间内单边进待配对，不计算收益'
              }
            >
              开平都在区间内 · 单边待配对
            </span>
          </div>
        </div>
      </header>

      <main className="detail-shell analysis-shell">
        <section className="pnl-toolbar analysis-toolbar" aria-label="组合 FIFO 查询范围">
          <div className="date-range">
            <CalendarRange size={18} />
            <label>
              <span>开始</span>
              <input
                type="datetime-local"
                value={startInput}
                min={toDatetimeLocal(strategy.stMs)}
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
          <div className="analysis-toolbar__right">
            <div className="analysis-reference-fee">
              <label>
                <span>参考 Fee (bps)</span>
                <input
                  type="number"
                  min="-100"
                  max="100"
                  step="0.01"
                  value={referenceFeeInput}
                  onChange={(event) => setReferenceFeeInput(event.target.value)}
                  onKeyDown={(event) => {
                    if (event.key === 'Enter') applyReferenceFee()
                  }}
                />
              </label>
              <button
                type="button"
                onClick={applyReferenceFee}
                disabled={
                  loading ||
                  referenceFeeInput.trim() === '' ||
                  Number(referenceFeeInput) === referenceFeeBps
                }
              >
                应用
              </button>
            </div>
            <label className="analysis-symbol-select">
              <span>币种</span>
              <select
                value={symbol}
                onChange={(event) => setSymbol(event.target.value)}
                disabled={!analysis}
              >
                <option value="">全部币种</option>
                {availableClosedSymbols.map((item) => (
                  <option key={item} value={item}>{item}</option>
                ))}
              </select>
            </label>
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
          </div>
        </section>

        <section className="analysis-scope" aria-label="组合统计口径">
          <div>
            <Layers3 size={16} />
            <span>记账</span>
            <strong>开、平都在选定区间才计入；单边进待配对</strong>
          </div>
          <div>
            <Database size={16} />
            <span>口径</span>
            <strong>
              {includeClosedCarry
                ? '交易价差 + Funding - Interest - Fee'
                : 'FIFO 成交价差 - Fee'}
            </strong>
          </div>
          <div>
            <Database size={16} />
            <span>基准</span>
            <strong>Main FKey Hedge · 成交基差 (fut − spot)</strong>
          </div>
        </section>

        {pageError && (
          <div className="error-state pnl-error">
            <CircleAlert size={19} />
            <div>
              <strong>组合分析加载失败</strong>
              <span>{pageError}</span>
            </div>
          </div>
        )}

        <section className="analysis-fee-bar" aria-label="收益 Fee 口径">
          <div>
            <span>收益口径</span>
            <strong>{feeModeConfig.label}</strong>
          </div>
          <div className="segmented segmented--compact analysis-fee-mode">
            {intraFeeModeOptions.map((option) => (
              <button
                key={option.key}
                className={feeMode === option.key ? 'is-active' : ''}
                type="button"
                aria-pressed={feeMode === option.key}
                onClick={() => setFeeMode(option.key)}
              >
                {option.label}
              </button>
            ))}
          </div>
          <span>
            {includeClosedCarry
              ? '只有开、平都在选定区间内的配对才计入；Funding 与 Interest 随闭环数量释放；区间内单边进待配对'
              : '只有开、平都在选定区间内的配对才计入；跨区间或未配对的单边挂在待配对，不计算收益'}
          </span>
        </section>

        <section className="analysis-metrics" aria-label="组合 FIFO 汇总">
          <div className="analysis-metric analysis-metric--primary">
            <span>{feeModeConfig.label}收益</span>
            <strong className={valueClass(summary ? feeModePnl(summary, feeMode) : 0)}>
              {summary ? money(feeModePnl(summary, feeMode), true) : '--'}
            </strong>
            <small>
              {summary ? bps(feeModeReturnBps(summary, feeMode), true) : '--'} bps
            </small>
          </div>
          <div className="analysis-metric">
            <span>{feeModeImpactLabel(feeMode)}</span>
            <strong className={valueClass(summary ? feeModeImpact(summary, feeMode) : 0)}>
              {summary ? money(feeModeImpact(summary, feeMode), true) : '--'}
            </strong>
            <small>
              {summary
                ? feeMode === 'actual'
                  ? `${percentage(summary.actualFeeCoverage)} actual fee coverage`
                  : feeMode === 'reference'
                    ? `${bps(summary.referenceFeeBps)} bps reference`
                    : 'Fee excluded'
                : '--'}
            </small>
          </div>
          {includeClosedCarry && (
            <>
              <div className="analysis-metric">
                <span>闭环 Funding</span>
                <strong className={valueClass(summary?.fundingPnlUsdt ?? 0)}>
                  {summary ? money(summary.fundingPnlUsdt, true) : '--'}
                </strong>
                <small>{summary ? bps(summary.fundingReturnBps, true) : '--'} bps</small>
              </div>
              <div className="analysis-metric">
                <span>闭环 Interest</span>
                <strong className={valueClass(-(summary?.interestCostUsdt ?? 0))}>
                  {summary ? money(-summary.interestCostUsdt, true) : '--'}
                </strong>
                <small>
                  {summary ? bps(-summary.interestCostReturnBps, true) : '--'} bps
                </small>
              </div>
            </>
          )}
          <div className="analysis-metric">
            <span>{feeMode === 'gross' ? '兑现占比' : '过费兑现'}</span>
            <strong>{capture ? percentage(capture.aboveShare) : '--'}</strong>
            <small>
              {capture
                ? feeMode === 'gross'
                  ? `${percentage(capture.uncapturedShare)} 没兑现`
                  : `不够 ${percentage(capture.belowShare)} · 没兑现 ${percentage(capture.uncapturedShare)}`
                : '--'}
            </small>
          </div>
          <div className="analysis-metric">
            <span>FIFO 闭环</span>
            <strong>{summary ? compactNumber(summary.closedMatchCount) : '--'}</strong>
            <small>{summary ? percentage(summary.winRate) : '--'} win rate</small>
          </div>
          <div className="analysis-metric">
            <span>待配对</span>
            <strong>{summary ? money(unpairedNotional(summary)) : '--'}</strong>
            <small>
              {summary
                ? `${quantity(unpairedQuantity(summary))} 剩余 · 不计入收益`
                : '--'}
            </small>
          </div>
        </section>

        <section
          className="chart-panel analysis-chart-panel"
          data-chart-id="fifo-closed-pnl"
        >
          <div className="chart-panel__header">
            <div>
              <p className="eyebrow">FIFO FILL SPREAD</p>
              <h2>{chartMode === 'portfolio' ? '累计成交价差' : '分币累计成交价差'}</h2>
            </div>
            <div className="analysis-chart-controls">
              <div className="segmented segmented--compact analysis-chart-mode" aria-label="图表视图">
                <button
                  className={chartMode === 'portfolio' ? 'is-active' : ''}
                  type="button"
                  aria-pressed={chartMode === 'portfolio'}
                  onClick={() => setChartMode('portfolio')}
                >
                  组合
                </button>
                <button
                  className={chartMode === 'symbol' ? 'is-active' : ''}
                  type="button"
                  aria-pressed={chartMode === 'symbol'}
                  onClick={() => setChartMode('symbol')}
                >
                  分币
                </button>
              </div>
              <span className="analysis-chart-total">
                {feeModeConfig.label}{' '}
                {chartTotal === null ? '--' : money(chartTotal, true)} USDT
              </span>
            </div>
          </div>
          <div
            className={
              'analysis-chart-body' +
              (chartMode === 'symbol' ? ' analysis-chart-body--symbol' : '')
            }
          >
            <div className="analysis-chart-stage">
              {analysis && (
                <IntraFifoChart
                  points={analysis.points}
                  symbolPoints={
                    chartMode === 'symbol'
                      ? visibleChartSymbolPoints
                      : analysis.symbolPoints
                  }
                  symbolColors={chartSymbolColors}
                  mode={chartMode}
                  feeMode={feeMode}
                  includeClosedCarry={includeClosedCarry}
                />
              )}
              {chartMode === 'symbol' &&
                !loading &&
                visibleChartSymbolPoints.length === 0 && (
                  <div className="chart-loading analysis-chart-empty">
                    <span>请从右侧勾选币种</span>
                  </div>
                )}
              {loading && (
                <div className="chart-loading">
                  <LoaderCircle size={20} />
                  <span>计算中</span>
                </div>
              )}
            </div>
            {chartMode === 'symbol' && (
              <aside className="analysis-symbol-selector" aria-label="分币曲线选择">
                <div className="analysis-symbol-selector__header">
                  <div>
                    <span>币种曲线</span>
                    <strong>
                      {selectedChartSymbols.length} / {chartSymbolRows.length}
                    </strong>
                  </div>
                  <div
                    className="segmented segmented--compact analysis-symbol-filter"
                    aria-label={`按${feeModeConfig.label}收益筛选币种`}
                  >
                    <button
                      className={chartSymbolSelection === 'all' ? 'is-active' : ''}
                      type="button"
                      onClick={() => selectChartSymbols('all')}
                    >
                      全部
                    </button>
                    <button
                      className={chartSymbolSelection === 'positive' ? 'is-active' : ''}
                      type="button"
                      onClick={() => selectChartSymbols('positive')}
                    >
                      正收益
                    </button>
                    <button
                      className={chartSymbolSelection === 'negative' ? 'is-active' : ''}
                      type="button"
                      onClick={() => selectChartSymbols('negative')}
                    >
                      负收益
                    </button>
                  </div>
                </div>
                <div className="analysis-symbol-pools">
                  <SymbolPool
                    title="正收益 / 持平"
                    tone="positive"
                    rows={positiveChartSymbols}
                    selectedSymbols={selectedChartSymbolSet}
                    onToggle={toggleChartSymbol}
                    onToggleAll={toggleChartSymbolPool}
                  />
                  <SymbolPool
                    title="负收益"
                    tone="negative"
                    rows={negativeChartSymbols}
                    selectedSymbols={selectedChartSymbolSet}
                    onToggle={toggleChartSymbol}
                    onToggleAll={toggleChartSymbolPool}
                  />
                </div>
              </aside>
            )}
          </div>
          {analysis && (
            <div className="chart-foot">
              <span>{compactNumber(analysis.summary.closedMatchCount)} 开平都在区间内的闭环</span>
              <span>
                {money(unpairedNotional(analysis.summary))} 待配对不计入
              </span>
              <span>
                {compactNumber(
                  chartMode === 'portfolio'
                    ? analysis.source.returnedPoints
                    : visibleChartPointCount,
                )} points
              </span>
              <span>
                {chartMode === 'portfolio'
                  ? chartSymbolRows.length
                  : selectedChartSymbols.length} symbols
              </span>
              <span>
                {percentage(combinedCapture(analysis.summary, feeMode).aboveShare)}{' '}
                {feeMode === 'gross' ? '兑现' : '过费兑现'}
              </span>
              <span>
                {percentage(analysis.summary.actualFeeCoverage)} actual fee coverage ·{' '}
                {compactNumber(analysis.source.windowFeeTradeRows)} fills
              </span>
              {analysis.source.sampled && <span>sampled</span>}
              <span>closed four-leg fee allocation</span>
              <span>{bps(analysis.summary.referenceFeeBps)} bps reference fee</span>
              {includeClosedCarry && (
                <>
                  <span>
                    {compactNumber(analysis.source.allocatedFundingRows)} /{' '}
                    {compactNumber(analysis.source.windowFundingRows)} funding events allocated
                  </span>
                  <span>funding on open lots, released by FIFO closed quantity</span>
                  <span>
                    {compactNumber(analysis.source.allocatedInterestRows)} /{' '}
                    {compactNumber(analysis.source.windowInterestRows)} interest events allocated
                  </span>
                  <span>{compactNumber(analysis.source.convertedInterestRows)} interest events converted</span>
                  <span>interest on open spot borrowing, released by FIFO closed quantity</span>
                </>
              )}
            </div>
          )}
        </section>

        <section
          className="chart-panel analysis-chart-panel analysis-latency-panel"
          data-chart-id="hourly-latency"
        >
          <div className="chart-panel__header">
            <div>
              <p className="eyebrow">HOURLY ORDER LATENCY</p>
              <h2>小时订单时延</h2>
            </div>
            <span className="analysis-chart-total">
              {selectedLatencyKeys.length} / {LATENCY_CHART_LINES.length} series ·{' '}
              {latencySeries?.points.length ?? 0} hours
            </span>
          </div>
          <div className="analysis-chart-body analysis-chart-body--symbol">
            <div className="analysis-chart-stage">
              {selectedLatencyKeys.length > 0 ? (
                <IntraLatencyChart
                  series={latencySeries}
                  selectedKeys={selectedLatencyKeys}
                />
              ) : (
                <div className="chart-loading analysis-chart-empty">
                  <span>请从右侧勾选时延序列</span>
                </div>
              )}
              {loading && (
                <div className="chart-loading">
                  <LoaderCircle size={20} />
                  <span>计算中</span>
                </div>
              )}
            </div>
            <aside className="analysis-symbol-selector" aria-label="小时时延序列选择">
              <div className="analysis-symbol-selector__header">
                <div>
                  <span>时延曲线</span>
                  <strong>
                    {selectedLatencyKeys.length} / {LATENCY_CHART_LINES.length}
                  </strong>
                </div>
                <div
                  className="segmented segmented--compact analysis-symbol-filter"
                  aria-label="按时延分位筛选"
                >
                  {LATENCY_LINE_FILTERS.map((option) => (
                    <button
                      key={option.key}
                      className={latencyLineFilter === option.key ? 'is-active' : ''}
                      type="button"
                      onClick={() => selectLatencyLines(option.key)}
                    >
                      {option.label}
                    </button>
                  ))}
                </div>
              </div>
              <div className="analysis-symbol-pools analysis-latency-pools">
                {LATENCY_CHART_FAMILIES.map((family) => (
                  <LatencyFamily
                    key={family.family}
                    family={family.family}
                    name={family.name}
                    selectedKeys={selectedLatencyKeySet}
                    onToggleLine={toggleLatencyLine}
                    onToggleFamily={toggleLatencyFamily}
                  />
                ))}
              </div>
            </aside>
          </div>
          <div className="chart-foot">
            <span>默认只显示 p50，避免 8 条线叠在一起</span>
            <span>p90 为虚线</span>
            <span>按触发腿拆分，平局不计入</span>
            <span>正常路径 ≤ 100 ms</span>
          </div>
        </section>

        <section className="analysis-table-panel">
          <div className="analysis-table-header">
            <div>
              <p className="eyebrow">SYMBOL BREAKDOWN</p>
              <h2>分币组合收益</h2>
            </div>
            <span>{visibleSymbols.length} symbols</span>
          </div>
          <div className="analysis-table-wrap">
            <table className="analysis-table analysis-symbol-table">
              <thead>
                <tr>
                  <th>Symbol</th>
                  <th>FIFO / 胜率</th>
                  <th>闭环本金</th>
                  <th>{feeModeConfig.label}收益</th>
                  <th>Fee 影响</th>
                  {includeClosedCarry && (
                    <>
                      <th>闭环 Funding</th>
                      <th>闭环 Interest</th>
                    </>
                  )}
                  <th>过费兑现</th>
                </tr>
              </thead>
              <tbody>
                {visibleSymbols.map((row) => (
                  <SymbolRow
                    key={row.symbol}
                    row={row}
                    feeMode={feeMode}
                    includeClosedCarry={includeClosedCarry}
                  />
                ))}
                {!loading && visibleSymbols.length === 0 && (
                  <tr>
                    <td className="analysis-empty" colSpan={includeClosedCarry ? 8 : 6}>
                      暂无闭环或待配对数据
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
        </section>

        <section className="analysis-table-panel">
          <div className="analysis-table-header">
            <div>
              <p className="eyebrow">PENDING UNPAIRED LOTS</p>
              <h2>待配对</h2>
            </div>
            <span>
              {visiblePendingLots.length}
              {!symbol
                && analysis
                && analysis.source.returnedPendingLots > visiblePendingLots.length
                ? ` / ${analysis.source.returnedPendingLots}`
                : ''}{' '}
              lots · 不计入收益
            </span>
          </div>
          <div className="analysis-table-wrap">
            <table className="analysis-table analysis-pending-table">
              <thead>
                <tr>
                  <th>进入时间</th>
                  <th>Symbol</th>
                  <th>方向</th>
                  <th>成交基差 bps</th>
                  <th>数量</th>
                  <th>名义</th>
                </tr>
              </thead>
              <tbody>
                {visiblePendingLots.map((lot) => (
                  <tr key={`${lot.symbol}-${lot.fkey}`}>
                    <td>{new Date(lot.openedAtMs).toLocaleString()}</td>
                    <td><strong>{lot.symbol}</strong></td>
                    <td>
                      <span className={'analysis-direction analysis-direction--' + lot.direction}>
                        {pendingDirectionLabel(lot.direction)}
                      </span>
                    </td>
                    <td
                      title={`spot ${lot.spotPrice} / futures ${lot.futuresPrice}`}
                    >
                      {bps(lot.basisBps, true)}
                    </td>
                    <td>{quantity(lot.quantity)}</td>
                    <td>{money(lot.notionalUsdt)}</td>
                  </tr>
                ))}
                {!loading && visiblePendingLots.length === 0 && (
                  <tr>
                    <td className="analysis-empty" colSpan={6}>
                      当前范围没有待配对单边
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
        </section>

        <section className="analysis-table-panel">
          <div className="analysis-table-header">
            <div>
              <p className="eyebrow">RECENT FIFO CLOSES</p>
              <h2>最近闭环明细</h2>
            </div>
            <span>latest {analysis?.source.returnedMatches ?? 0}</span>
          </div>
          <div className="analysis-table-wrap">
            <table className="analysis-table analysis-match-table">
              <thead>
                <tr>
                  <th>闭环时间</th>
                  <th>Symbol</th>
                  <th>配对</th>
                  <th>成交基差 bps</th>
                  <th>数量</th>
                  <th>持有</th>
                  {includeClosedCarry && (
                    <>
                      <th>闭环 Funding</th>
                      <th>闭环 Interest</th>
                    </>
                  )}
                  <th>Fee 影响</th>
                  <th>{feeModeConfig.label}收益</th>
                </tr>
              </thead>
              <tbody>
                {analysis?.matches.map((row) => (
                  <tr key={`${row.closeFkey}-${row.openFkey}`}>
                    <td>{new Date(row.closedAtMs).toLocaleString()}</td>
                    <td><strong>{row.symbol}</strong></td>
                    <td>
                      <span className={'analysis-direction analysis-direction--' + row.openDirection}>
                        {pairLabel(row.openDirection)}
                      </span>
                    </td>
                    <td
                      title={
                        `open ${row.openSpotPrice} / ${row.openFuturesPrice}; ` +
                        `close ${row.closeSpotPrice} / ${row.closeFuturesPrice}`
                      }
                    >
                      <span className="analysis-basis-move">
                        {bps(row.entryBasisBps, true)}
                        <ArrowRight size={12} />
                        {bps(row.exitBasisBps, true)}
                      </span>
                    </td>
                    <td>{quantity(row.quantity)}</td>
                    <td>{duration(row.holdingMs)}</td>
                    {includeClosedCarry && (
                      <>
                        <td className={valueClass(row.fundingPnlUsdt)}>
                          <strong>{money(row.fundingPnlUsdt, true)}</strong>
                        </td>
                        <td className={valueClass(-row.interestCostUsdt)}>
                          <strong>{money(-row.interestCostUsdt, true)}</strong>
                        </td>
                      </>
                    )}
                    <td className={valueClass(matchFeeModeImpact(row, feeMode))}>
                      <strong>{money(matchFeeModeImpact(row, feeMode), true)}</strong>
                    </td>
                    <td className={valueClass(matchFeeModePnl(row, feeMode))}>
                      <strong>{money(matchFeeModePnl(row, feeMode), true)}</strong>
                      <small>{bps(matchFeeModeReturnBps(row, feeMode), true)} bps</small>
                    </td>
                  </tr>
                ))}
                {!loading && analysis?.matches.length === 0 && (
                  <tr>
                    <td className="analysis-empty" colSpan={includeClosedCarry ? 10 : 8}>
                      当前范围没有 FIFO 闭环
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
        </section>
      </main>
    </>
  )
}
