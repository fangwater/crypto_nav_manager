import {
  AlertTriangle,
  ArrowLeft,
  CheckCircle2,
  CircleAlert,
  Gauge,
  Layers3,
  ShieldAlert,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import { getFrPositionLimits } from '../api'
import type {
  FrLimitEnvironment,
  FrLimitExchange,
  FrLimitRow,
  FrPositionLimitOverview,
} from '../types'

const timeFormatter = new Intl.DateTimeFormat(undefined, {
  month: '2-digit',
  day: '2-digit',
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
  hour12: false,
})

const usdFormatter = new Intl.NumberFormat('en-US', {
  style: 'currency',
  currency: 'USD',
  maximumFractionDigits: 0,
})

const percentFormatter = new Intl.NumberFormat('en-US', {
  minimumFractionDigits: 1,
  maximumFractionDigits: 1,
})

const exchangeOrder: FrLimitExchange[] = ['binance', 'gate']

const exchangeLabels: Record<
  FrLimitExchange,
  { name: string; market: string; mark: string }
> = {
  binance: {
    name: 'Binance',
    market: 'Portfolio Margin · UM Futures',
    mark: 'B',
  },
  gate: {
    name: 'Gate',
    market: 'Unified Account · USDT Futures',
    mark: 'G',
  },
}

function formatUsd(value: number | null) {
  return value === null ? '--' : usdFormatter.format(value)
}

function formatRatio(value: number | null) {
  return value === null ? '--' : percentFormatter.format(value * 100) + '%'
}

function sideLabel(side: FrLimitRow['side']) {
  if (side === 'short') return '空'
  if (side === 'long') return '多'
  return '平'
}

function environmentStatus(environment: FrLimitEnvironment) {
  if (environment.status === 'error') return '数据异常'
  if (!environment.paramsLive) return '参数降级'
  if (environment.warnings.length > 0) return '数据降级'
  if (environment.sourceCounts.nearLimitRows > 0) return '接近限仓'
  return '正常'
}

interface FrLimitTrigger {
  environment: FrLimitEnvironment
  row: FrLimitRow
}

interface FrSymbolAlert {
  exchange: FrLimitExchange
  symbol: string
  asset: string
  maxUsageRatio: number
  triggers: FrLimitTrigger[]
}

function strategyLabel(slug: string) {
  const match = slug.match(/_fr_arb(\d+)$/)
  return match ? `FR${match[1]}` : slug
}

function ExchangeSourceIssues({
  environments,
}: {
  environments: FrLimitEnvironment[]
}) {
  const issues = environments.filter(
    (environment) =>
      environment.error !== null ||
      environment.warnings.length > 0 ||
      !environment.paramsLive,
  )

  if (issues.length === 0) return null

  return (
    <details className="fr-exchange-issues">
      <summary>
        <CircleAlert size={15} />
        <span>数据源提示 {issues.length} 盘</span>
      </summary>
      <div className="fr-exchange-issues__list">
        {issues.map((environment) => (
          <div key={environment.strategySlug}>
            <strong>{environment.strategySlug}</strong>
            <span>{environmentStatus(environment)}</span>
            {environment.error && <p>{environment.error}</p>}
            {environment.warnings.map((warning) => (
              <p key={warning}>{warning}</p>
            ))}
          </div>
        ))}
      </div>
    </details>
  )
}

function SymbolAlert({ alert }: { alert: FrSymbolAlert }) {
  const strategyNames = alert.triggers.map(({ environment }) =>
    strategyLabel(environment.strategySlug),
  )

  return (
    <article className="fr-symbol-alert">
      <header className="fr-symbol-alert__header">
        <div className="fr-symbol-alert__identity">
          <span className="fr-alert-indicator" aria-hidden="true">
            <AlertTriangle size={15} />
          </span>
          <div>
            <h3>{alert.symbol}</h3>
            <p>
              {alert.asset} · {alert.triggers.length} 个盘触发
            </p>
          </div>
        </div>
        <div className="fr-symbol-alert__summary">
          <div className="fr-trigger-strategies" aria-label="触发盘子">
            {strategyNames.map((name) => (
              <span key={name}>{name}</span>
            ))}
          </div>
          <div className="fr-max-usage">
            <span>最高占用率</span>
            <strong>{formatRatio(alert.maxUsageRatio)}</strong>
          </div>
        </div>
      </header>

      <div className="fr-limit-table-wrap">
        <table className="fr-limit-table fr-trigger-table">
          <thead>
            <tr>
              <th>触发盘</th>
              <th>方向</th>
              <th>占用率</th>
              <th>REST 合约仓位</th>
              <th>Guard Cap</th>
              <th>交易所限仓</th>
              <th>剩余额度</th>
              <th>Buffer</th>
              <th>Snapshot Futures</th>
              <th>REST - Snapshot</th>
            </tr>
          </thead>
          <tbody>
            {alert.triggers.map(({ environment, row }) => (
              <tr className="is-near-limit" key={environment.strategySlug}>
                <th>
                  <strong>{strategyLabel(environment.strategySlug)}</strong>
                  <small title={environment.strategySlug}>
                    {environment.strategySlug}
                  </small>
                </th>
                <td>
                  <span className={'fr-side fr-side--' + row.side}>
                    {sideLabel(row.side)}
                  </span>
                </td>
                <td>
                  <div className="fr-usage">
                    <strong>{formatRatio(row.usageRatio)}</strong>
                    <span aria-hidden="true">
                      <i
                        style={{
                          width:
                            Math.min(
                              100,
                              Math.max(0, (row.usageRatio ?? 0) * 100),
                            ) + '%',
                        }}
                      />
                    </span>
                  </div>
                </td>
                <td className="fr-number">
                  <strong>{formatUsd(row.positionNotionalUsdt)}</strong>
                  <small>{row.positionSource}</small>
                </td>
                <td className="fr-number">
                  <strong>{formatUsd(row.guardCapUsdt)}</strong>
                </td>
                <td className="fr-number">
                  <strong>{formatUsd(row.exchangeLimitUsdt)}</strong>
                  {row.leverage !== null && <small>{row.leverage}x</small>}
                </td>
                <td className="fr-number">
                  <strong>{formatUsd(row.remainingUsdt)}</strong>
                </td>
                <td className="fr-number">
                  <strong>{formatUsd(row.guardBufferUsdt)}</strong>
                  {row.pendingLimitOrders !== null && row.amountU !== null && (
                    <small>
                      {row.pendingLimitOrders} × {formatUsd(row.amountU)}
                    </small>
                  )}
                </td>
                <td className="fr-number">
                  <strong>{formatUsd(row.snapshotFuturesUsdt)}</strong>
                </td>
                <td className="fr-number">
                  <strong>{formatUsd(row.snapshotRestDeltaUsdt)}</strong>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </article>
  )
}

export function FrPositionLimitsPage() {
  const [overview, setOverview] = useState<FrPositionLimitOverview | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let active = true
    let controller: AbortController | null = null
    const refresh = () => {
      controller?.abort()
      controller = new AbortController()
      getFrPositionLimits(controller.signal)
        .then((next) => {
          if (!active) return
          setOverview(next)
          setError(null)
        })
        .catch((reason: unknown) => {
          if (
            !active ||
            (reason instanceof DOMException && reason.name === 'AbortError')
          ) {
            return
          }
          setError(reason instanceof Error ? reason.message : String(reason))
        })
    }
    refresh()
    const timer = window.setInterval(refresh, 30_000)
    return () => {
      active = false
      controller?.abort()
      window.clearInterval(timer)
    }
  }, [])

  const environmentsByExchange = useMemo(
    () =>
      new Map(
        exchangeOrder.map((exchange) => [
          exchange,
          overview?.environments.filter(
            (environment) => environment.exchange === exchange,
          ) ?? [],
        ]),
      ),
    [overview],
  )

  const alertsByExchange = useMemo(() => {
    const next = new Map<FrLimitExchange, FrSymbolAlert[]>()

    for (const exchange of exchangeOrder) {
      const bySymbol = new Map<string, FrSymbolAlert>()
      const environments =
        overview?.environments.filter(
          (environment) => environment.exchange === exchange,
        ) ?? []

      for (const environment of environments) {
        for (const row of environment.rows) {
          if (!row.nearLimit) continue

          const existing = bySymbol.get(row.symbol)
          if (existing) {
            existing.triggers.push({ environment, row })
            existing.maxUsageRatio = Math.max(
              existing.maxUsageRatio,
              row.usageRatio ?? 0,
            )
          } else {
            bySymbol.set(row.symbol, {
              exchange,
              symbol: row.symbol,
              asset: row.asset,
              maxUsageRatio: row.usageRatio ?? 0,
              triggers: [{ environment, row }],
            })
          }
        }
      }

      const alerts = [...bySymbol.values()]
      for (const alert of alerts) {
        alert.triggers.sort((left, right) => {
          const usageDifference =
            (right.row.usageRatio ?? 0) - (left.row.usageRatio ?? 0)
          return (
            usageDifference ||
            left.environment.strategySlug.localeCompare(
              right.environment.strategySlug,
            )
          )
        })
      }
      alerts.sort(
        (left, right) =>
          right.maxUsageRatio - left.maxUsageRatio ||
          left.symbol.localeCompare(right.symbol),
      )
      next.set(exchange, alerts)
    }

    return next
  }, [overview])

  const symbolAlertCount = exchangeOrder.reduce(
    (total, exchange) => total + (alertsByExchange.get(exchange)?.length ?? 0),
    0,
  )
  const triggerCount = exchangeOrder.reduce(
    (total, exchange) =>
      total +
      (alertsByExchange.get(exchange)?.reduce(
        (exchangeTotal, alert) => exchangeTotal + alert.triggers.length,
        0,
      ) ?? 0),
    0,
  )
  const sourceIssueCount =
    overview?.environments.filter(
      (environment) =>
        environment.error !== null ||
        environment.warnings.length > 0 ||
        !environment.paramsLive,
    ).length ?? 0
  const headerTone =
    error && !overview
      ? 'danger'
      : symbolAlertCount > 0 || sourceIssueCount > 0
        ? 'warning'
        : 'ready'

  return (
    <>
      <header className="detail-header fr-limit-header">
        <div className="detail-header__inner">
          <div className="detail-title">
            <Link
              className="icon-button icon-button--back"
              to="/"
              aria-label="返回盘子总览"
              title="返回盘子总览"
            >
              <ArrowLeft size={17} />
            </Link>
            <span className="fr-limit-title-mark" aria-hidden="true">
              <Gauge size={19} />
            </span>
            <div>
              <p>FUTURES RISK LIMITS</p>
              <h1>FR 合约限仓</h1>
            </div>
          </div>
          <div className="system-state">
            <span className={'status-dot status-dot--' + headerTone} />
            {error && !overview
              ? '限仓后端不可用'
              : overview
                ? timeFormatter.format(overview.generatedAtMs)
                : '连接中'}
          </div>
        </div>
      </header>

      <main className="detail-shell fr-limit-shell">
        {error && (
          <div className="error-state">
            <CircleAlert size={19} />
            <div>
              <strong>限仓数据刷新失败</strong>
              <span>{error}</span>
            </div>
          </div>
        )}

        <section className="fr-limit-overview">
          <div className="section-heading">
            <div>
              <p className="eyebrow">EXCHANGE + SYMBOL ALERTS</p>
              <h2>当前限仓告警</h2>
            </div>
          </div>
          <div className="summary-strip fr-limit-summary">
            <div className="summary-item">
              <Layers3 size={17} />
              <div>
                <span>监控盘子</span>
                <strong>{overview?.environments.length ?? '--'}</strong>
              </div>
            </div>
            <div className="summary-item summary-item--danger">
              <Gauge size={17} />
              <div>
                <span>告警币种</span>
                <strong>{overview ? symbolAlertCount : '--'}</strong>
              </div>
            </div>
            <div className="summary-item summary-item--danger">
              <AlertTriangle size={17} />
              <div>
                <span>触发盘次</span>
                <strong>{overview ? triggerCount : '--'}</strong>
              </div>
            </div>
            <div className="summary-item">
              <ShieldAlert size={17} />
              <div>
                <span>告警阈值</span>
                <strong>
                  {overview
                    ? percentFormatter.format(
                        overview.alertThresholdRatio * 100,
                      ) + '%'
                    : '--'}
                </strong>
              </div>
            </div>
          </div>
        </section>

        {exchangeOrder.map((exchange) => {
          const meta = exchangeLabels[exchange]
          const environments = environmentsByExchange.get(exchange) ?? []
          const alerts = alertsByExchange.get(exchange) ?? []
          const exchangeTriggers = alerts.reduce(
            (total, alert) => total + alert.triggers.length,
            0,
          )

          return (
            <section
              className={'fr-exchange-section fr-exchange-section--' + exchange}
              key={exchange}
            >
              <header className="fr-exchange-heading">
                <span className="fr-exchange-mark" aria-hidden="true">
                  {meta.mark}
                </span>
                <div>
                  <p className="eyebrow">{meta.market}</p>
                  <h2>{meta.name}</h2>
                </div>
                <div className="fr-exchange-counts">
                  <span>{alerts.length} 告警币种</span>
                  <strong className={alerts.length > 0 ? 'has-alerts' : ''}>
                    {exchangeTriggers} 盘触发
                  </strong>
                </div>
              </header>

              <ExchangeSourceIssues environments={environments} />

              {overview && environments.length === 0 && (
                <div className="fr-source-error fr-exchange-message">
                  <CircleAlert size={15} />
                  <span>{meta.name} 监控配置缺失</span>
                </div>
              )}

              {overview &&
                environments.length > 0 &&
                alerts.length === 0 && (
                  <div className="fr-empty fr-exchange-message">
                    <CheckCircle2 size={17} />
                    <span>当前没有限仓告警</span>
                  </div>
                )}

              {alerts.length > 0 && (
                <div className="fr-symbol-alerts">
                  {alerts.map((alert) => (
                    <SymbolAlert
                      alert={alert}
                      key={exchange + ':' + alert.symbol}
                    />
                  ))}
                </div>
              )}
            </section>
          )
        })}
      </main>
    </>
  )
}
