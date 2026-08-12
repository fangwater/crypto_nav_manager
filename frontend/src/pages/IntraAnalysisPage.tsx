import {
  ArrowLeft,
  ArrowRight,
  CalendarRange,
  CircleAlert,
  Database,
  FlaskConical,
  GitCompareArrows,
  Layers3,
  LoaderCircle,
  RefreshCw,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link, useParams } from 'react-router-dom'
import { getIntraAnalysis, getStrategy } from '../api'
import {
  IntraFifoChart,
  type IntraFifoChartMode,
} from '../components/IntraFifoChart'
import {
  intraAnalysisMetricOptions,
  intraSymbolColor,
} from '../intraAnalysisSeries'
import type {
  IntraAnalysis,
  IntraAnalysisSeriesKey,
  IntraArbDirection,
  IntraSymbolAnalysis,
  Strategy,
} from '../types'

const rangeOptions = [
  { key: 'ALL', days: null },
  { key: '1D', days: 1 },
  { key: '7D', days: 7 },
  { key: '30D', days: 30 },
] as const

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

function optionalBps(value: number | null) {
  return value === null ? '--' : bps(value, true)
}

function optionalMoney(value: number | null) {
  return value === null ? '--' : money(value, true)
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

function directionLabel(direction: IntraArbDirection) {
  return direction === 'positive' ? '正套' : '反套'
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

function SymbolRow({ row }: { row: IntraSymbolAnalysis }) {
  return (
    <tr>
      <td><strong>{row.symbol}</strong></td>
      <td>
        <strong>{compactNumber(row.mtCount)}</strong>
        <small>{row.positiveMtCount} / {row.reverseMtCount}</small>
      </td>
      <td>
        <strong>{compactNumber(row.closedMatchCount)}</strong>
        <small>{percentage(row.winRate)} win</small>
      </td>
      <td>{money(row.matchedNotionalUsdt)}</td>
      <td className={valueClass(row.realizedPnlUsdt)}>
        <strong>{money(row.realizedPnlUsdt, true)}</strong>
        <small>{bps(row.returnBps, true)} bps</small>
      </td>
      <td className={valueClass(row.marketPnlUsdt)}>
        <strong>{money(row.marketPnlUsdt, true)}</strong>
        <small>{bps(row.marketReturnBps, true)} bps</small>
      </td>
      <td className={valueClass(row.executionPnlUsdt)}>
        <strong>{money(row.executionPnlUsdt, true)}</strong>
        <small>{bps(row.executionReturnBps, true)} bps</small>
      </td>
      <td className={valueClass(row.executionCaptureUsdt)}>
        <strong>{money(row.executionCaptureUsdt, true)}</strong>
        <small>{bps(row.executionCaptureReturnBps, true)} bps</small>
      </td>
      <td>
        <strong>{percentage(row.executionMtPremiumCoverage)}</strong>
        <small>{compactNumber(row.executionMtCount)} MT legs</small>
      </td>
      <td>
        <strong>{money(row.positiveOpenNotionalUsdt)}</strong>
        <small>
          {quantity(row.positiveOpenQuantity)} @ {bps(row.positiveAverageBasisBps, true)} bps
        </small>
      </td>
      <td>
        <strong>{money(row.reverseOpenNotionalUsdt)}</strong>
        <small>
          {quantity(row.reverseOpenQuantity)} @ {bps(row.reverseAverageBasisBps, true)} bps
        </small>
      </td>
    </tr>
  )
}

export function IntraAnalysisPage() {
  const { slug = '' } = useParams()
  const [strategy, setStrategy] = useState<Strategy | null>(null)
  const [analysis, setAnalysis] = useState<IntraAnalysis | null>(null)
  const [startInput, setStartInput] = useState('')
  const [endInput, setEndInput] = useState('')
  const [startMs, setStartMs] = useState<number | null>(null)
  const [endMs, setEndMs] = useState<number | null>(null)
  const [symbol, setSymbol] = useState('')
  const [chartMode, setChartMode] = useState<IntraFifoChartMode>('portfolio')
  const [chartMetric, setChartMetric] =
    useState<IntraAnalysisSeriesKey>('realizedPnlUsdt')
  const [chartSymbolSelection, setChartSymbolSelection] =
    useState<ChartSymbolSelection>('all')
  const [selectedChartSymbols, setSelectedChartSymbols] = useState<string[]>([])
  const [pageError, setPageError] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)

  useEffect(() => {
    let active = true
    setStrategy(null)
    setAnalysis(null)
    setSymbol('')
    setPageError(null)
    setLoading(true)
    getStrategy(slug)
      .then((nextStrategy) => {
        if (!active) return
        const now = Date.now()
        setStrategy(nextStrategy)
        setStartInput(toDatetimeLocal(nextStrategy.stMs))
        setEndInput(toDatetimeLocal(now))
        setStartMs(nextStrategy.stMs)
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
    getIntraAnalysis(strategy.slug, {
      startMs,
      endMs,
      symbols: symbol ? [symbol] : undefined,
      maxPoints: 3500,
      maxMatches: 200,
      signal: controller.signal,
    })
      .then(setAnalysis)
      .catch((reason: unknown) => {
        if (reason instanceof DOMException && reason.name === 'AbortError') return
        setPageError(reason instanceof Error ? reason.message : String(reason))
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false)
      })
    return () => controller.abort()
  }, [strategy, startMs, endMs, symbol])

  const visibleSymbols = useMemo(() => {
    if (!analysis) return []
    return symbol
      ? analysis.symbols.filter((row) => row.symbol === symbol)
      : analysis.symbols
  }, [analysis, symbol])

  const chartSymbolRows = useMemo<ChartSymbolRow[]>(() => {
    if (!analysis) return []
    return analysis.symbolPoints.map((series, index) => ({
      symbol: series.symbol,
      value: series.points.at(-1)?.[chartMetric] ?? 0,
      color: intraSymbolColor(index),
    }))
  }, [analysis, chartMetric])

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
        : Math.max(strategy.stMs, nextEnd - days * 86_400_000)
    setStartInput(toDatetimeLocal(nextStart))
    setEndInput(toDatetimeLocal(nextEnd))
    setStartMs(nextStart)
    setEndMs(nextEnd)
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

  if (!strategy && !pageError) {
    return (
      <main className="detail-shell">
        <div className="detail-loading" />
      </main>
    )
  }

  if (!strategy) {
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
          </div>
        </div>
      </main>
    )
  }

  const summary = analysis?.summary
  const chartTotal = summary
    ? chartMode === 'portfolio'
      ? summary.realizedPnlUsdt
      : visibleChartSymbolPoints.reduce(
          (total, series) => total + (series.points.at(-1)?.[chartMetric] ?? 0),
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
              <p>组合 FIFO 研究</p>
              <h1>{strategy.displayName}</h1>
            </div>
          </div>
          <div className="analysis-header-tags">
            <span title="独立研究口径，不进入正式 NAV">
              <FlaskConical size={14} /> Research
            </span>
            <span title="不包含手续费、Funding 和利息">Gross only</span>
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
            <label className="analysis-symbol-select">
              <span>币种</span>
              <select
                value={symbol}
                onChange={(event) => setSymbol(event.target.value)}
                disabled={!analysis}
              >
                <option value="">全部币种</option>
                {analysis?.availableSymbols.map((item) => (
                  <option key={item} value={item}>{item}</option>
                ))}
              </select>
            </label>
            <div className="segmented segmented--compact" aria-label="快捷时间范围">
              {rangeOptions.map((option) => (
                <button
                  key={option.key}
                  type="button"
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
            <span>正套</span>
            <strong>Spot Buy / Futures Sell</strong>
          </div>
          <div>
            <GitCompareArrows size={16} />
            <span>反套</span>
            <strong>Spot Sell / Futures Buy</strong>
          </div>
          <div>
            <Database size={16} />
            <span>基准</span>
            <strong>
              Main FKey Hedge · {strategy.exchange === 'bybit' ? 'Bybit' : 'Binance'} Premium 1m close
            </strong>
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

        <section className="analysis-metrics" aria-label="组合 FIFO 汇总">
          <div className="analysis-metric analysis-metric--primary">
            <span>FIFO 毛收益</span>
            <strong className={valueClass(summary?.realizedPnlUsdt ?? 0)}>
              {summary ? money(summary.realizedPnlUsdt, true) : '--'}
            </strong>
            <small>{summary ? bps(summary.returnBps, true) : '--'} bps</small>
          </div>
          <div className="analysis-metric">
            <span>选币基差收益</span>
            <strong className={valueClass(summary?.marketPnlUsdt ?? 0)}>
              {summary ? money(summary.marketPnlUsdt, true) : '--'}
            </strong>
            <small>{summary ? bps(summary.marketReturnBps, true) : '--'} bps</small>
          </div>
          <div className="analysis-metric">
            <span>闭环两腿执行</span>
            <strong className={valueClass(summary?.executionPnlUsdt ?? 0)}>
              {summary ? money(summary.executionPnlUsdt, true) : '--'}
            </strong>
            <small>{summary ? bps(summary.executionReturnBps, true) : '--'} bps</small>
          </div>
          <div className="analysis-metric">
            <span>窗口 MT 执行捕捉</span>
            <strong className={valueClass(summary?.executionCaptureUsdt ?? 0)}>
              {summary ? money(summary.executionCaptureUsdt, true) : '--'}
            </strong>
            <small>
              {summary
                ? `正 ${money(summary.positiveExecutionCaptureUsdt, true)} / ` +
                  `反 ${money(summary.reverseExecutionCaptureUsdt, true)}`
                : '--'}
            </small>
          </div>
          <div className="analysis-metric">
            <span>Premium 覆盖</span>
            <strong>{summary ? percentage(summary.executionMtPremiumCoverage) : '--'}</strong>
            <small>{summary ? compactNumber(summary.executionMtCount) : '--'} MT legs</small>
          </div>
          <div className="analysis-metric">
            <span>FIFO 闭环</span>
            <strong>{summary ? compactNumber(summary.closedMatchCount) : '--'}</strong>
            <small>{summary ? percentage(summary.winRate) : '--'} win rate</small>
          </div>
        </section>

        <section className="chart-panel analysis-chart-panel">
          <div className="chart-panel__header">
            <div>
              <p className="eyebrow">TRADE DECOMPOSITION</p>
              <h2>{chartMode === 'portfolio' ? '累计收益根因' : '分币累计收益'}</h2>
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
              {chartMode === 'symbol' && (
                <label className="analysis-symbol-select analysis-series-select">
                  <span>指标</span>
                  <select
                    value={chartMetric}
                    onChange={(event) =>
                      setChartMetric(event.target.value as IntraAnalysisSeriesKey)
                    }
                  >
                    {intraAnalysisMetricOptions.map((option) => (
                      <option key={option.key} value={option.key}>{option.label}</option>
                    ))}
                  </select>
                </label>
              )}
              <span className="analysis-chart-total">
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
                  metric={chartMetric}
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
                    aria-label="按当前指标收益筛选币种"
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
              <span>{compactNumber(analysis.source.windowMtRows)} hedged MT</span>
              <span>
                {compactNumber(
                  chartMode === 'portfolio'
                    ? analysis.source.returnedPoints
                    : visibleChartPointCount,
                )} points
              </span>
              <span>
                {chartMode === 'portfolio'
                  ? analysis.selectedSymbols.length
                  : selectedChartSymbols.length} symbols
              </span>
              <span>{percentage(analysis.summary.premiumCoverage)} premium</span>
              {analysis.source.sampled && <span>sampled</span>}
              <span>fee / funding excluded</span>
            </div>
          )}
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
            <table className="analysis-table">
              <thead>
                <tr>
                  <th>Symbol</th>
                  <th>MT 正 / 反</th>
                  <th>FIFO / 胜率</th>
                  <th>闭环本金</th>
                  <th>实际成交</th>
                  <th>选币基差</th>
                  <th>闭环两腿执行</th>
                  <th>窗口 MT 执行</th>
                  <th>MT k 覆盖</th>
                  <th>未闭合正套</th>
                  <th>未闭合反套</th>
                </tr>
              </thead>
              <tbody>
                {visibleSymbols.map((row) => <SymbolRow key={row.symbol} row={row} />)}
                {!loading && visibleSymbols.length === 0 && (
                  <tr><td className="analysis-empty" colSpan={11}>暂无闭环数据</td></tr>
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
                  <th>开仓方向</th>
                  <th>实际基差 bps</th>
                  <th>市场 k bps</th>
                  <th>执行边际 bps</th>
                  <th>数量</th>
                  <th>持有</th>
                  <th>选币基差</th>
                  <th>开仓腿执行</th>
                  <th>平仓腿执行</th>
                  <th>实际成交</th>
                </tr>
              </thead>
              <tbody>
                {analysis?.matches.map((row) => (
                  <tr key={`${row.closeFkey}-${row.openFkey}`}>
                    <td>{new Date(row.closedAtMs).toLocaleString()}</td>
                    <td><strong>{row.symbol}</strong></td>
                    <td>
                      <span className={'analysis-direction analysis-direction--' + row.openDirection}>
                        {directionLabel(row.openDirection)}
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
                    <td
                      title={`${strategy.exchange === 'bybit' ? 'Bybit' : 'Binance'} premium-index 1m close`}
                    >
                      <span className="analysis-basis-move">
                        {optionalBps(row.entryPremiumBps)}
                        <ArrowRight size={12} />
                        {optionalBps(row.exitPremiumBps)}
                      </span>
                    </td>
                    <td title="实际成交基差减去同分钟 premium-index close">
                      <span className="analysis-basis-move">
                        {optionalBps(row.entryExecutionEdgeBps)}
                        <ArrowRight size={12} />
                        {optionalBps(row.exitExecutionEdgeBps)}
                      </span>
                    </td>
                    <td>{quantity(row.quantity)}</td>
                    <td>{duration(row.holdingMs)}</td>
                    <td className={valueClass(row.marketPnlUsdt ?? 0)}>
                      <strong>{optionalMoney(row.marketPnlUsdt)}</strong>
                    </td>
                    <td className={valueClass(row.entryExecutionPnlUsdt ?? 0)}>
                      <strong>{optionalMoney(row.entryExecutionPnlUsdt)}</strong>
                    </td>
                    <td className={valueClass(row.exitExecutionPnlUsdt ?? 0)}>
                      <strong>{optionalMoney(row.exitExecutionPnlUsdt)}</strong>
                    </td>
                    <td className={valueClass(row.pnlUsdt)}>
                      <strong>{money(row.pnlUsdt, true)}</strong>
                      <small>{bps(row.returnBps, true)} bps</small>
                    </td>
                  </tr>
                ))}
                {!loading && analysis?.matches.length === 0 && (
                  <tr><td className="analysis-empty" colSpan={12}>当前范围没有 FIFO 闭环</td></tr>
                )}
              </tbody>
            </table>
          </div>
        </section>
      </main>
    </>
  )
}
