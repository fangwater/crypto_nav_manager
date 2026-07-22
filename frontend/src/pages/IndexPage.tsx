import {
  Activity,
  ArrowRight,
  CheckCircle2,
  CircleAlert,
  Database,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import { getStrategies } from '../api'
import type { Strategy } from '../types'

type Filter = 'all' | 'funding_rate' | 'intra_exchange' | 'market_making'

const filters: Array<{ value: Filter; label: string }> = [
  { value: 'all', label: '全部' },
  { value: 'funding_rate', label: '资金费' },
  { value: 'intra_exchange', label: '所内套利' },
  { value: 'market_making', label: '做市' },
]

function modeLabel(mode: string) {
  switch (mode) {
    case 'portfolio_margin':
      return 'Portfolio Margin'
    case 'usdm_futures':
      return 'USD-M Futures'
    case 'unified':
      return 'Unified Account'
    default:
      return mode
  }
}

function kindLabel(kind: Strategy['strategyKind']) {
  if (kind === 'funding_rate') return '资金费套利'
  if (kind === 'market_making') return '做市'
  return '所内套利'
}

export function IndexPage() {
  const [strategies, setStrategies] = useState<Strategy[]>([])
  const [filter, setFilter] = useState<Filter>('all')
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)

  useEffect(() => {
    getStrategies()
      .then(setStrategies)
      .catch((reason: unknown) => {
        setError(reason instanceof Error ? reason.message : String(reason))
      })
      .finally(() => setLoading(false))
  }, [])

  const visibleStrategies = useMemo(
    () =>
      filter === 'all'
        ? strategies
        : strategies.filter((strategy) => strategy.strategyKind === filter),
    [filter, strategies],
  )
  const readyCount = strategies.filter(
    (strategy) => strategy.credentialsReady,
  ).length
  const binanceCount = strategies.filter(
    (strategy) => strategy.exchange === 'binance',
  ).length
  const gateCount = strategies.filter(
    (strategy) => strategy.exchange === 'gate',
  ).length

  return (
    <>
      <header className="app-header">
        <div className="app-header__inner">
          <div className="brand">
            <span className="brand__mark" aria-hidden="true">
              <Activity size={19} strokeWidth={2} />
            </span>
            <div>
              <h1>Crypto NAV</h1>
              <p>净值管理系统</p>
            </div>
          </div>
          <div className="system-state">
            <span className="status-dot status-dot--ready" />
            服务在线
          </div>
        </div>
      </header>

      <main className="page-shell">
        <section className="overview">
          <div className="section-heading">
            <div>
              <p className="eyebrow">PORTFOLIOS</p>
              <h2>盘子总览</h2>
            </div>
            <div className="segmented" aria-label="盘子类型筛选">
              {filters.map((item) => (
                <button
                  key={item.value}
                  type="button"
                  className={filter === item.value ? 'is-active' : ''}
                  onClick={() => setFilter(item.value)}
                >
                  {item.label}
                </button>
              ))}
            </div>
          </div>

          <div className="summary-strip">
            <div className="summary-item">
              <Database size={17} />
              <div>
                <span>已接入</span>
                <strong>{strategies.length || 7}</strong>
              </div>
            </div>
            <div className="summary-item">
              <CheckCircle2 size={17} />
              <div>
                <span>凭证就绪</span>
                <strong>{readyCount}</strong>
              </div>
            </div>
            <div className="summary-item summary-item--venues">
              <span className="venue-count venue-count--binance">B</span>
              <div>
                <span>Binance / Gate</span>
                <strong>
                  {binanceCount} / {gateCount}
                </strong>
              </div>
            </div>
          </div>
        </section>

        {loading && (
          <div className="loading-grid" aria-label="正在加载盘子">
            {Array.from({ length: 7 }, (_, index) => (
              <div
                className="strategy-card strategy-card--loading"
                key={index}
              />
            ))}
          </div>
        )}

        {error && (
          <div className="error-state">
            <CircleAlert size={19} />
            <div>
              <strong>盘子列表加载失败</strong>
              <span>{error}</span>
            </div>
          </div>
        )}

        {!loading && !error && (
          <div className="strategy-grid">
            {visibleStrategies.map((strategy) => (
              <Link
                className={
                  'strategy-card strategy-card--' + strategy.exchange
                }
                to={'/strategies/' + strategy.slug}
                key={strategy.slug}
              >
                <div className="strategy-card__top">
                  <span className="exchange-mark" aria-hidden="true">
                    {strategy.exchange === 'binance' ? 'B' : 'G'}
                  </span>
                  <span
                    className={
                      strategy.credentialsReady
                        ? 'credential-state credential-state--ready'
                        : 'credential-state credential-state--warning'
                    }
                  >
                    <span className="status-dot" />
                    {strategy.credentialsReady ? '凭证就绪' : '检查 env'}
                  </span>
                </div>
                <div className="strategy-card__body">
                  <span className="strategy-kind">
                    {kindLabel(strategy.strategyKind)}
                  </span>
                  <h3>{strategy.displayName}</h3>
                  <p>{modeLabel(strategy.accountMode)}</p>
                </div>
                <div className="strategy-card__footer">
                  <code title={strategy.envPath}>{strategy.envPath}</code>
                  <span
                    className="icon-button"
                    title="进入盘子"
                    aria-label="进入盘子"
                  >
                    <ArrowRight size={17} />
                  </span>
                </div>
              </Link>
            ))}
          </div>
        )}
      </main>
    </>
  )
}

