import {
  ArrowLeft,
  CheckCircle2,
  CircleAlert,
  Database,
  ExternalLink,
  Settings,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { Link, useParams } from 'react-router-dom'
import { getStrategy } from '../api'
import { NavChart } from '../components/NavChart'
import type { Strategy } from '../types'

const rangeOptions = ['1D', '7D', '30D']

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

export function StrategyPage() {
  const { slug = '' } = useParams()
  const [strategy, setStrategy] = useState<Strategy | null>(null)
  const [selectedRange, setSelectedRange] = useState('7D')
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    setStrategy(null)
    setError(null)
    getStrategy(slug)
      .then(setStrategy)
      .catch((reason: unknown) => {
        setError(reason instanceof Error ? reason.message : String(reason))
      })
  }, [slug])

  if (error) {
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
            <span>{error}</span>
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
              className={
                'exchange-mark exchange-mark--' + strategy.exchange
              }
              aria-hidden="true"
            >
              {strategy.exchange === 'binance' ? 'B' : 'G'}
            </span>
            <div>
              <p>
                {strategy.strategyKind === 'funding_rate'
                  ? '资金费套利'
                  : '所内套利'}
              </p>
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

      <main className="detail-shell">
        <section className="strategy-meta" aria-label="盘子状态">
          <div className="meta-item">
            {strategy.credentialsReady ? (
              <CheckCircle2 size={18} className="icon-ready" />
            ) : (
              <CircleAlert size={18} className="icon-warning" />
            )}
            <div>
              <span>凭证状态</span>
              <strong>
                {strategy.credentialsReady ? '就绪' : '需检查'}
              </strong>
            </div>
          </div>
          <div className="meta-item">
            <Database size={18} />
            <div>
              <span>账户模式</span>
              <strong>{modeLabel(strategy.accountMode)}</strong>
            </div>
          </div>
          <div className="meta-path">
            <span>ENV</span>
            <code>{strategy.envPath}</code>
          </div>
        </section>

        <section className="chart-panel">
          <div className="chart-panel__header">
            <div>
              <p className="eyebrow">NET ASSET VALUE</p>
              <h2>净值曲线</h2>
            </div>
            <div
              className="segmented segmented--compact"
              aria-label="净值时间范围"
            >
              {rangeOptions.map((range) => (
                <button
                  key={range}
                  type="button"
                  className={selectedRange === range ? 'is-active' : ''}
                  onClick={() => setSelectedRange(range)}
                >
                  {range}
                </button>
              ))}
            </div>
          </div>
          <NavChart name={strategy.slug} />
        </section>
      </main>
    </>
  )
}

