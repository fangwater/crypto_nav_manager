import {
  ArrowLeft,
  CalendarRange,
  Check,
  CircleAlert,
  Database,
  ExternalLink,
  LoaderCircle,
  RefreshCw,
  Search,
  Settings,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link, useParams } from 'react-router-dom'
import { getStrategy, getStrategyPnl } from '../api'
import { PnlChart } from '../components/PnlChart'
import { PositionChart } from '../components/PositionChart'
import type {
  PnlSeriesKey,
  PositionSeriesKey,
  Strategy,
  StrategyPnl,
  SymbolPnlSummary,
} from '../types'

const rangeOptions = [
  { key: 'ALL', days: null },
  { key: '1D', days: 1 },
  { key: '7D', days: 7 },
  { key: '30D', days: 30 },
] as const

const seriesOptions: Array<{
  key: PnlSeriesKey
  label: string
  color: string
}> = [
  { key: 'totalPnlUsdt', label: 'Total', color: '#176b5b' },
  { key: 'feeBeforePnlUsdt', label: 'Fee 前', color: '#2563a7' },
  { key: 'feeAfterPnlUsdt', label: 'Fee 后', color: '#7357a3' },
  { key: 'fundingPnlUsdt', label: 'Funding', color: '#b7791f' },
  { key: 'interestCostUsdt', label: 'Interest', color: '#c2413b' },
]

const positionSeriesOptions: Array<{
  key: PositionSeriesKey
  label: string
  color: string
}> = [
  { key: 'spotPositionUsdt', label: 'Spot 仓位', color: '#2563a7' },
  { key: 'futuresPositionUsdt', label: 'Futures 仓位', color: '#b7791f' },
  { key: 'exposureUsdt', label: '敞口', color: '#c2413b' },
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
  if (kind === 'cta') return 'CTA'
  if (kind === 'market_making') return '做市'
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

export function PnlStrategyPage() {
  const { slug = '' } = useParams()
  const [strategy, setStrategy] = useState<Strategy | null>(null)
  const [pnl, setPnl] = useState<StrategyPnl | null>(null)
  const [startInput, setStartInput] = useState('')
  const [endInput, setEndInput] = useState('')
  const [startMs, setStartMs] = useState<number | null>(null)
  const [endMs, setEndMs] = useState<number | null>(null)
  const [selectedSymbols, setSelectedSymbols] = useState<string[] | null>(null)
  const [visibleSeries, setVisibleSeries] = useState<PnlSeriesKey[]>([
    'totalPnlUsdt',
    'feeAfterPnlUsdt',
    'fundingPnlUsdt',
    'interestCostUsdt',
  ])
  const [chartMode, setChartMode] = useState<'portfolio' | 'symbols'>(
    'portfolio',
  )
  const [visiblePositionSeries, setVisiblePositionSeries] = useState<
    PositionSeriesKey[]
  >(['spotPositionUsdt', 'futuresPositionUsdt', 'exposureUsdt'])
  const [symbolSearch, setSymbolSearch] = useState('')
  const [strategyError, setStrategyError] = useState<string | null>(null)
  const [pnlError, setPnlError] = useState<string | null>(null)
  const [loadingPnl, setLoadingPnl] = useState(false)

  useEffect(() => {
    setStrategy(null)
    setPnl(null)
    setStrategyError(null)
    getStrategy(slug)
      .then((nextStrategy) => {
        const now = Date.now()
        setStrategy(nextStrategy)
        setStartInput(toDatetimeLocal(nextStrategy.stMs))
        setEndInput(toDatetimeLocal(now))
        setStartMs(nextStrategy.stMs)
        setEndMs(now)
        setSelectedSymbols(null)
      })
      .catch((reason: unknown) => {
        setStrategyError(reason instanceof Error ? reason.message : String(reason))
      })
  }, [slug])

  useEffect(() => {
    if (!strategy || startMs === null || endMs === null) return
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

  function applyRange() {
    if (!strategy) return
    const nextStart = fromDatetimeLocal(startInput)
    const nextEnd = fromDatetimeLocal(endInput)
    if (!Number.isFinite(nextStart) || !Number.isFinite(nextEnd)) {
      setPnlError('请选择有效时间')
      return
    }
    if (nextStart < strategy.stMs || nextEnd < nextStart) {
      setPnlError('时间范围无效')
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

  function toggleSymbol(symbol: string) {
    if (!pnl) return
    if (selectedSymbols === null) {
      const next = pnl.availableSymbols.filter((item) => item !== symbol)
      if (next.length) setSelectedSymbols(next)
      return
    }
    const exists = selectedSymbols.includes(symbol)
    const next = exists
      ? selectedSymbols.filter((item) => item !== symbol)
      : [...selectedSymbols, symbol].sort()
    if (!next.length) return
    setSelectedSymbols(
      next.length === pnl.availableSymbols.length ? null : next,
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
  const isMarketMaking = strategy.strategyKind === 'market_making'
  const activePositionSeries = isMarketMaking
    ? visiblePositionSeries.filter((key) => key !== 'spotPositionUsdt')
    : visiblePositionSeries
  const activePositionOptions = isMarketMaking
    ? marketMakingPositionSeriesOptions
    : positionSeriesOptions

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
          <a className="config-button" href={strategy.configUrl}>
            <Settings size={17} />
            配置
            <ExternalLink size={14} />
          </a>
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
        </section>

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
            <span>Fee 前</span>
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
            <span>Fee 后</span>
            <strong className={valueClass(summary?.feeAfterPnlUsdt ?? 0)}>
              {summary ? money(summary.feeAfterPnlUsdt, true) : '--'}
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
                  {pnl?.selectedSymbols.length ?? 0} 条币种曲线
                </span>
              )}
            </div>
          </div>
          <div className="chart-body has-picker">
            <div className="chart-stage">
              {pnl && (
                <PnlChart
                  points={pnl.points}
                  symbolPoints={pnl.symbolPoints}
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
              <aside className="symbol-curve-picker" aria-label="分币曲线选择">
                <div className="symbol-curve-picker__header">
                  <strong>币种</strong>
                  <button
                    type="button"
                    onClick={() => setSelectedSymbols(null)}
                    disabled={selectedSymbols === null}
                  >
                    全选
                  </button>
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
                points
              </span>
              <span>{pnl.selectedSymbols.length} symbols</span>
              {pnl.source.sampled && <span>sampled</span>}
            </div>
          )}
        </section>

        {(strategy.strategyKind === 'funding_rate' ||
          strategy.strategyKind === 'intra_exchange' ||
          isMarketMaking) && (
          <section className="chart-panel pnl-chart-panel">
            <div className="chart-panel__header pnl-chart-header">
              <div>
                <p className="eyebrow">POSITION / EXPOSURE</p>
                <h2>{isMarketMaking ? '合约仓位与敞口' : '仓位与敞口'}</h2>
              </div>
              <div className="chart-controls">
                <div
                  className="segmented segmented--compact"
                  aria-label="仓位曲线视图"
                >
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
              </div>
            </div>
            <div className="chart-body has-picker">
              <div className="chart-stage">
                {pnl && (
                  <PositionChart
                    points={pnl.points}
                    symbolPoints={pnl.symbolPoints}
                    visibleSeries={activePositionSeries}
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
                <aside className="symbol-curve-picker" aria-label="仓位曲线选择">
                  <div className="symbol-curve-picker__header">
                    <strong>U 仓位</strong>
                  </div>
                  <div className="symbol-curve-picker__list">
                    {activePositionOptions.map((option) => (
                      <label key={option.key}>
                        <input
                          type="checkbox"
                          checked={visiblePositionSeries.includes(option.key)}
                          onChange={() => togglePositionSeries(option.key)}
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
                <aside className="symbol-curve-picker" aria-label="分币仓位选择">
                  <div className="symbol-curve-picker__header">
                    <strong>币种</strong>
                    <button
                      type="button"
                      onClick={() => setSelectedSymbols(null)}
                      disabled={selectedSymbols === null}
                    >
                      全选
                    </button>
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
                <span>USDT NOTIONAL</span>
                <span>
                  {isMarketMaking
                    ? 'EXPOSURE = ABS(FUTURES POSITION)'
                    : 'EXPOSURE = SPOT + FUTURES'}
                </span>
                <span>{pnl.selectedSymbols.length} symbols</span>
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
                onClick={() => setSelectedSymbols(null)}
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
                  <th>Fee 前</th>
                  <th>Fee 后</th>
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
