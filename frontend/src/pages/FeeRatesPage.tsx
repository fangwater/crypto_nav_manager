import {
  Activity,
  ArrowLeft,
  ChevronDown,
  ChevronUp,
  CircleAlert,
  Clock3,
  Percent,
  RefreshCw,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import { getFeeRates, syncFeeRates } from '../api'
import type { AccountFeeRates, Strategy, TradingFeeRate } from '../types'

type ExchangeFilter = 'all' | Strategy['exchange']

const exchanges: Array<{ value: ExchangeFilter; label: string }> = [
  { value: 'all', label: '全部' },
  { value: 'binance', label: 'Binance' },
  { value: 'bybit', label: 'Bybit' },
  { value: 'gate', label: 'Gate' },
  { value: 'bitget', label: 'Bitget' },
  { value: 'okx', label: 'OKX' },
]

const bybitDefaultSymbols = ['BTC', 'ETH', 'SOL', 'XRP', 'BNB', 'DOGE']

function compactInstrument(instrument: string) {
  return instrument.toUpperCase().replace(/[^A-Z0-9]/g, '')
}

function bybitDefaultSymbolIndex(instrument: string) {
  const compact = compactInstrument(instrument)
  return bybitDefaultSymbols.findIndex(
    (symbol) => compact === symbol || compact === symbol + 'USDT',
  )
}

function marketOrder(market: string) {
  if (market === 'spot') return 0
  if (market === 'linear') return 1
  return 2
}

function exchangeBadge(exchange: Strategy['exchange']) {
  if (exchange === 'binance') return 'BN'
  if (exchange === 'bybit') return 'BY'
  if (exchange === 'gate') return 'GT'
  if (exchange === 'bitget') return 'BG'
  return 'OK'
}

function kindLabel(kind: Strategy['strategyKind']) {
  if (kind === 'funding_rate') return '资金费套利'
  if (kind === 'market_making') return '做市'
  return '所内套利'
}

function modeLabel(mode: string) {
  if (mode === 'portfolio_margin') return 'Portfolio Margin'
  if (mode === 'usdm_futures') return 'USD-M Futures'
  if (mode === 'unified') return 'Unified Account'
  return mode
}

function marketLabel(market: string) {
  const labels: Record<string, string> = {
    spot: 'Spot',
    margin: 'Margin',
    linear: 'Linear',
    swap: 'Swap',
    usdt_futures: 'USDT Futures',
  }
  return labels[market] ?? market
}

function feeDisplay(value: string) {
  const rate = Number(value)
  const bps = Number.isFinite(rate) ? rate * 10_000 : 0
  if (Math.abs(bps) < 0.00005) {
    return { label: '0.00 bps', detail: '免手续费', className: 'is-zero' }
  }
  return {
    label:
      Math.abs(bps).toLocaleString('en-US', {
        minimumFractionDigits: 2,
        maximumFractionDigits: 4,
      }) + ' bps',
    detail: bps < 0 ? '返佣' : '成本',
    className: bps < 0 ? 'is-rebate' : 'is-cost',
  }
}

function formatTime(ms: number) {
  return new Intl.DateTimeFormat('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  }).format(new Date(ms))
}

function latestTime(rates: TradingFeeRate[]) {
  return rates.reduce(
    (latest, rate) => Math.max(latest, rate.fetchedAtMs),
    0,
  )
}

function FeeValue({ value }: { value: string }) {
  const fee = feeDisplay(value)
  return (
    <div className={'fee-value ' + fee.className}>
      <strong>{fee.label}</strong>
      <span>{fee.detail}</span>
    </div>
  )
}

interface AccountRatesProps {
  account: AccountFeeRates
  syncing: boolean
  syncError?: string
  onSync: (slug: string) => void
}

function AccountRates({
  account,
  syncing,
  syncError,
  onSync,
}: AccountRatesProps) {
  const [expanded, setExpanded] = useState(false)
  const updatedAt = latestTime(account.rates)
  const { defaultRates, otherRates } = useMemo(() => {
    if (account.exchange !== 'bybit') {
      return { defaultRates: account.rates, otherRates: [] }
    }
    const defaults: TradingFeeRate[] = []
    const others: TradingFeeRate[] = []
    for (const rate of account.rates) {
      if (bybitDefaultSymbolIndex(rate.instrument) >= 0) defaults.push(rate)
      else others.push(rate)
    }
    defaults.sort((left, right) => {
      const symbolDifference =
        bybitDefaultSymbolIndex(left.instrument) -
        bybitDefaultSymbolIndex(right.instrument)
      return (
        symbolDifference ||
        marketOrder(left.market) - marketOrder(right.market)
      )
    })
    return { defaultRates: defaults, otherRates: others }
  }, [account.exchange, account.rates])
  const visibleRates = expanded
    ? defaultRates.concat(otherRates)
    : defaultRates
  const otherInstrumentCount = new Set(
    otherRates.map((rate) => rate.instrument),
  ).size
  return (
    <article className="fee-account">
      <header className="fee-account__header">
        <div className="fee-account__identity">
          <span
            className={'exchange-mark exchange-mark--' + account.exchange}
            aria-hidden="true"
          >
            {exchangeBadge(account.exchange)}
          </span>
          <div>
            <Link to={'/strategies/' + account.slug}>
              {account.displayName}
            </Link>
            <p>
              {kindLabel(account.strategyKind)} ·{' '}
              {modeLabel(account.accountMode)}
            </p>
          </div>
        </div>
        <div className="fee-account__status">
          <div className={'fee-sync-state' + (updatedAt ? '' : ' is-empty')}>
            <Clock3 size={14} />
            {updatedAt ? formatTime(updatedAt) : '待同步'}
          </div>
          <button
            className="fee-account-sync"
            type="button"
            aria-label={'同步 ' + account.displayName}
            title="同步此账户"
            disabled={syncing}
            onClick={() => onSync(account.slug)}
          >
            <RefreshCw size={15} className={syncing ? 'is-spinning' : ''} />
          </button>
        </div>
      </header>

      {syncError && (
        <div className="fee-account-sync-error">
          <CircleAlert size={14} />
          <span>{syncError}</span>
        </div>
      )}

      {account.rates.length ? (
        <>
          <div className="fee-table-wrap">
            <table className="fee-table">
              <thead>
                <tr>
                  <th>市场</th>
                  <th>币种</th>
                  <th>Maker</th>
                  <th>Taker</th>
                  <th>等级 / 分组</th>
                </tr>
              </thead>
              <tbody>
                {visibleRates.map((rate) => (
                  <tr
                    key={
                      rate.market +
                      ':' +
                      rate.instrument +
                      ':' +
                      (rate.feeGroup ?? '')
                    }
                  >
                    <td>
                      <span className="market-tag">
                        {marketLabel(rate.market)}
                      </span>
                    </td>
                    <td>
                      <strong>{rate.instrument}</strong>
                    </td>
                    <td>
                      <FeeValue value={rate.makerRate} />
                    </td>
                    <td>
                      <FeeValue value={rate.takerRate} />
                    </td>
                    <td className="fee-tier">
                      {[rate.feeTier, rate.feeGroup]
                        .filter(Boolean)
                        .join(' / ') || '—'}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          {otherRates.length > 0 && (
            <div className="fee-table-actions">
              <button
                className="fee-table-expand"
                type="button"
                aria-expanded={expanded}
                onClick={() => setExpanded((current) => !current)}
              >
                {expanded ? (
                  <ChevronUp size={15} />
                ) : (
                  <ChevronDown size={15} />
                )}
                {expanded
                  ? '收起其他币种'
                  : `展开其他币种 (${otherInstrumentCount})`}
              </button>
            </div>
          )}
        </>
      ) : (
        <div className="fee-empty">
          <Percent size={18} />
          <span>尚无手续费快照</span>
        </div>
      )}
    </article>
  )
}

export function FeeRatesPage() {
  const [accounts, setAccounts] = useState<AccountFeeRates[]>([])
  const [filter, setFilter] = useState<ExchangeFilter>('all')
  const [loading, setLoading] = useState(true)
  const [refreshing, setRefreshing] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [syncingSlugs, setSyncingSlugs] = useState<Set<string>>(
    new Set(),
  )
  const [syncErrors, setSyncErrors] = useState<Record<string, string>>({})

  function load(refresh = false) {
    const controller = new AbortController()
    if (refresh) setRefreshing(true)
    else setLoading(true)
    setError(null)
    getFeeRates(controller.signal)
      .then(setAccounts)
      .catch((reason: unknown) => {
        if (
          reason instanceof DOMException &&
          reason.name === 'AbortError'
        ) {
          return
        }
        setError(
          reason instanceof Error ? reason.message : String(reason),
        )
      })
      .finally(() => {
        setLoading(false)
        setRefreshing(false)
      })
    return controller
  }

  useEffect(() => {
    const controller = load()
    return () => controller.abort()
  }, [])

  async function syncAccount(slug: string) {
    setSyncingSlugs((current) => new Set(current).add(slug))
    setSyncErrors((current) => {
      const next = { ...current }
      delete next[slug]
      return next
    })
    try {
      await syncFeeRates(slug)
      setAccounts(await getFeeRates())
    } catch (reason: unknown) {
      const message = reason instanceof Error ? reason.message : String(reason)
      setSyncErrors((current) => ({ ...current, [slug]: message }))
    } finally {
      setSyncingSlugs((current) => {
        const next = new Set(current)
        next.delete(slug)
        return next
      })
    }
  }

  const visibleAccounts = useMemo(
    () =>
      filter === 'all'
        ? accounts
        : accounts.filter((account) => account.exchange === filter),
    [accounts, filter],
  )
  const covered = accounts.filter(
    (account) => account.rates.length > 0,
  ).length
  const latest = Math.max(
    0,
    ...accounts.flatMap((account) =>
      account.rates.map((rate) => rate.fetchedAtMs),
    ),
  )

  return (
    <>
      <header className="app-header">
        <div className="app-header__inner">
          <Link className="brand brand--link" to="/">
            <span className="brand__mark" aria-hidden="true">
              <Activity size={19} strokeWidth={2} />
            </span>
            <div>
              <h1>Crypto NAV</h1>
              <p>账户手续费</p>
            </div>
          </Link>
          <Link className="header-nav-link" to="/">
            <ArrowLeft size={16} />
            盘子总览
          </Link>
        </div>
      </header>

      <main className="page-shell fee-page">
        <section className="fee-overview">
          <div className="section-heading">
            <div>
              <p className="eyebrow">TRADING FEE RATES</p>
              <h2>账户手续费</h2>
            </div>
            <button
              className="refresh-button"
              type="button"
              onClick={() => load(true)}
              disabled={refreshing}
            >
              <RefreshCw
                size={15}
                className={refreshing ? 'is-spinning' : ''}
              />
              刷新
            </button>
          </div>

          <div className="fee-summary">
            <div>
              <span>账户</span>
              <strong>{accounts.length}</strong>
            </div>
            <div>
              <span>已有快照</span>
              <strong>
                {covered} / {accounts.length}
              </strong>
            </div>
            <div>
              <span>最近同步</span>
              <strong>{latest ? formatTime(latest) : '—'}</strong>
            </div>
          </div>

          <div className="fee-filters" aria-label="交易所筛选">
            {exchanges.map((exchange) => (
              <button
                type="button"
                key={exchange.value}
                className={
                  filter === exchange.value ? 'is-active' : ''
                }
                onClick={() => setFilter(exchange.value)}
              >
                {exchange.label}
              </button>
            ))}
          </div>
        </section>

        {loading && (
          <div className="fee-loading" aria-label="正在加载手续费" />
        )}
        {error && (
          <div className="error-state">
            <CircleAlert size={19} />
            <div>
              <strong>手续费加载失败</strong>
              <span>{error}</span>
            </div>
          </div>
        )}
        {!loading && !error && (
          <section className="fee-account-list" aria-live="polite">
            {visibleAccounts.map((account) => (
              <AccountRates
                account={account}
                key={account.slug}
                syncing={syncingSlugs.has(account.slug)}
                syncError={syncErrors[account.slug]}
                onSync={syncAccount}
              />
            ))}
          </section>
        )}
      </main>
    </>
  )
}
