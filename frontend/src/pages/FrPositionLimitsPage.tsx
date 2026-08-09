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
  if (environment.sourceCounts.nearLimitRows > 0) return '接近限仓'
  if (environment.warnings.length > 0) return '数据降级'
  return '正常'
}

function EnvironmentTable({
  environment,
  onlyAlerts,
}: {
  environment: FrLimitEnvironment
  onlyAlerts: boolean
}) {
  const rows = onlyAlerts
    ? environment.rows.filter((row) => row.nearLimit)
    : environment.rows

  return (
    <article className="fr-environment">
      <header className="fr-environment__header">
        <div className="fr-environment__identity">
          <span
            className={
              'status-dot status-dot--' +
              (environment.status === 'error'
                ? 'danger'
                : environment.sourceCounts.nearLimitRows > 0
                  ? 'warning'
                  : 'ready')
            }
          />
          <div>
            <h3>{environment.strategySlug}</h3>
            <p>
              Snapshot{' '}
              {environment.snapshotTsMs
                ? timeFormatter.format(environment.snapshotTsMs)
                : '不可用'}
            </p>
          </div>
        </div>
        <div className="fr-environment__states">
          <span
            className={
              'fr-state-badge fr-state-badge--' + environment.status
            }
          >
            {environmentStatus(environment)}
          </span>
          <span
            className={
              'fr-param-state ' +
              (environment.paramsLive ? 'is-live' : 'is-fallback')
            }
          >
            {environment.paramsLive ? 'Pre-Trade 实时参数' : '原始限仓口径'}
          </span>
          <span>
            {environment.sourceCounts.displayedRows} symbols
          </span>
        </div>
      </header>

      {environment.error && (
        <div className="fr-source-error">
          <CircleAlert size={15} />
          <span>{environment.error}</span>
        </div>
      )}

      {environment.warnings.length > 0 && (
        <details className="fr-source-warnings">
          <summary>数据提示 {environment.warnings.length}</summary>
          <ul>
            {environment.warnings.map((warning) => (
              <li key={warning}>{warning}</li>
            ))}
          </ul>
        </details>
      )}

      {!environment.error && rows.length === 0 && (
        <div className="fr-empty">
          <CheckCircle2 size={17} />
          <span>{onlyAlerts ? '当前没有接近限仓的币种' : '当前没有有效合约仓位'}</span>
        </div>
      )}

      {rows.length > 0 && (
        <div className="fr-limit-table-wrap">
          <table className="fr-limit-table">
            <thead>
              <tr>
                <th>Symbol</th>
                <th>方向</th>
                <th>占用率</th>
                <th>REST 合约仓位</th>
                <th>Guard Cap</th>
                <th>交易所限仓</th>
                <th>剩余额度</th>
                <th>Snapshot Futures</th>
                <th>REST - Snapshot</th>
                <th>Buffer</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((row) => (
                <tr
                  className={row.nearLimit ? 'is-near-limit' : ''}
                  key={row.symbol}
                >
                  <th>
                    <strong>{row.symbol}</strong>
                    <small>
                      {row.error ??
                        (row.trackedInSnapshot ? row.asset : '仅 REST')}
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
                    {row.leverage !== null && (
                      <small>{row.leverage}x</small>
                    )}
                  </td>
                  <td className="fr-number">
                    <strong>{formatUsd(row.remainingUsdt)}</strong>
                  </td>
                  <td className="fr-number">
                    <strong>{formatUsd(row.snapshotFuturesUsdt)}</strong>
                  </td>
                  <td className="fr-number">
                    <strong>{formatUsd(row.snapshotRestDeltaUsdt)}</strong>
                  </td>
                  <td className="fr-number">
                    <strong>{formatUsd(row.guardBufferUsdt)}</strong>
                    {row.pendingLimitOrders !== null &&
                      row.amountU !== null && (
                        <small>
                          {row.pendingLimitOrders} × {formatUsd(row.amountU)}
                        </small>
                      )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </article>
  )
}

export function FrPositionLimitsPage() {
  const [overview, setOverview] = useState<FrPositionLimitOverview | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [onlyAlerts, setOnlyAlerts] = useState(false)

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
          if (!active || (reason instanceof DOMException && reason.name === 'AbortError')) {
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

  const allRows =
    overview?.environments.flatMap((environment) => environment.rows) ?? []
  const nearLimitRows = allRows.filter((row) => row.nearLimit).length
  const sourceIssueCount =
    overview?.environments.filter(
      (environment) =>
        environment.error !== null || environment.warnings.length > 0,
    ).length ?? 0
  const headerTone =
    error && !overview
      ? 'danger'
      : nearLimitRows > 0 || sourceIssueCount > 0
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
              <p className="eyebrow">ACCOUNT-WIDE LIMITS</p>
              <h2>实时限仓状态</h2>
            </div>
            <label className="fr-alert-toggle">
              <input
                type="checkbox"
                checked={onlyAlerts}
                onChange={(event) => setOnlyAlerts(event.target.checked)}
              />
              <span aria-hidden="true" />
              只看告警
            </label>
          </div>
          <div className="summary-strip fr-limit-summary">
            <div className="summary-item">
              <Layers3 size={17} />
              <div>
                <span>监控盘子</span>
                <strong>{overview?.environments.length ?? '--'}</strong>
              </div>
            </div>
            <div className="summary-item">
              <Gauge size={17} />
              <div>
                <span>有效仓位</span>
                <strong>{overview ? allRows.length : '--'}</strong>
              </div>
            </div>
            <div className="summary-item summary-item--danger">
              <AlertTriangle size={17} />
              <div>
                <span>接近限仓</span>
                <strong>{overview ? nearLimitRows : '--'}</strong>
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
          const exchangeRows = environments.flatMap(
            (environment) => environment.rows,
          )
          const exchangeAlerts = exchangeRows.filter(
            (row) => row.nearLimit,
          ).length
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
                  <span>{environments.length} 盘</span>
                  <strong className={exchangeAlerts > 0 ? 'has-alerts' : ''}>
                    {exchangeAlerts} 告警
                  </strong>
                </div>
              </header>
              {environments.map((environment) => (
                <EnvironmentTable
                  environment={environment}
                  onlyAlerts={onlyAlerts}
                  key={environment.strategySlug}
                />
              ))}
              {overview && environments.length === 0 && (
                <div className="fr-source-error">
                  <CircleAlert size={15} />
                  <span>{meta.name} 监控配置缺失</span>
                </div>
              )}
            </section>
          )
        })}
      </main>
    </>
  )
}
