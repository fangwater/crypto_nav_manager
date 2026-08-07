import {
  Activity,
  AlertTriangle,
  ArrowLeft,
  CheckCircle2,
  CircleAlert,
  Cpu,
  Server,
  ShieldAlert,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import { getOpsOverview } from '../api'
import type {
  OpsComponent,
  OpsComponentHealth,
  OpsComponentRole,
  OpsEnvironment,
  OpsOverview,
  OpsTradingBlock,
} from '../types'

const roleOrder: OpsComponentRole[] = [
  'trade_signal',
  'pre_trade',
  'account_monitor',
  'trade_engine',
  'persist_manager',
  'viz_server',
]

const roleLabels: Record<OpsComponentRole, string> = {
  trade_signal: 'Trade Signal',
  pre_trade: 'Pre-Trade',
  account_monitor: 'Account Monitor',
  trade_engine: 'Trade Engine',
  persist_manager: 'Persist',
  viz_server: 'Viz',
}

const timeFormatter = new Intl.DateTimeFormat(undefined, {
  month: '2-digit',
  day: '2-digit',
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
  hour12: false,
})

const numberFormatter = new Intl.NumberFormat(undefined, {
  maximumFractionDigits: 8,
})

const usdFormatter = new Intl.NumberFormat(undefined, {
  style: 'currency',
  currency: 'USD',
  maximumFractionDigits: 2,
})

function healthLabel(status: OpsComponentHealth) {
  switch (status) {
    case 'online':
      return '正常'
    case 'warning':
      return '告警'
    case 'offline':
      return '离线'
    case 'duplicate':
      return '重复进程'
    case 'zombie':
      return '僵尸进程'
  }
}

function environmentLabel(status: OpsEnvironment['status']) {
  if (status === 'healthy') return '正常'
  if (status === 'critical') return '严重'
  return '告警'
}

function legLabel(leg: OpsTradingBlock['blockedLeg']) {
  if (leg === 'margin') return 'Margin 阻断'
  if (leg === 'futures') return 'Futures 阻断'
  return '未知交易腿'
}

function sideLabel(side: string) {
  const normalized = side.toLowerCase()
  if (normalized === 'buy') return '买入'
  if (normalized === 'sell') return '卖出'
  return side
}

function positionValue(value: number) {
  if (Math.abs(value) < 1e-12) return '0'
  return numberFormatter.format(value)
}

interface BlockRow {
  environment: OpsEnvironment
  block: OpsTradingBlock
}

interface AlertRow {
  environment: OpsEnvironment
  component: OpsComponent
  sample: OpsComponent['alerts']['samples'][number]
}

export function OpsMonitorPage() {
  const [overview, setOverview] = useState<OpsOverview | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let active = true
    const refresh = () => {
      const controller = new AbortController()
      getOpsOverview(controller.signal)
        .then((next) => {
          if (!active) return
          setOverview(next)
          setError(null)
        })
        .catch((reason: unknown) => {
          if (!active) return
          setError(reason instanceof Error ? reason.message : String(reason))
        })
      return controller
    }
    let controller = refresh()
    const timer = window.setInterval(() => {
      controller.abort()
      controller = refresh()
    }, 5_000)
    return () => {
      active = false
      controller.abort()
      window.clearInterval(timer)
    }
  }, [])

  const blockRows = useMemo<BlockRow[]>(
    () =>
      overview?.environments.flatMap((environment) =>
        environment.tradingBlocks.map((block) => ({ environment, block })),
      ) ?? [],
    [overview],
  )

  const alertRows = useMemo<AlertRow[]>(
    () =>
      (overview?.environments.flatMap((environment) =>
        environment.components.flatMap((component) =>
          component.alerts.samples.map((sample) => ({
            environment,
            component,
            sample,
          })),
        ),
      ) ?? [])
        .sort((a, b) => b.sample.atMs - a.sample.atMs)
        .slice(0, 20),
    [overview],
  )

  const componentCount =
    overview?.environments.reduce(
      (count, environment) => count + environment.components.length,
      0,
    ) ?? 0
  const onlineCount =
    overview?.environments.reduce(
      (count, environment) =>
        count +
        environment.components.filter(
          (component) =>
            component.instances > 0 && component.status !== 'zombie',
        ).length,
      0,
    ) ?? 0
  const warningEnvironments =
    overview?.environments.filter((environment) => environment.status === 'warning')
      .length ?? 0
  const criticalEnvironments =
    overview?.environments.filter((environment) => environment.status === 'critical')
      .length ?? 0

  return (
    <>
      <header className="detail-header">
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
            <span className="ops-title-mark" aria-hidden="true">
              <ShieldAlert size={19} />
            </span>
            <div>
              <p>OPERATIONS</p>
              <h1>安全监控</h1>
            </div>
          </div>
          <div className="system-state">
            <span
              className={
                'status-dot ' +
                (error ? 'status-dot--danger' : 'status-dot--ready')
              }
            />
            {error
              ? '监控后端不可用'
              : overview
                ? timeFormatter.format(overview.generatedAtMs)
                : '连接中'}
          </div>
        </div>
      </header>

      <main className="detail-shell ops-shell">
        {error && !overview && (
          <div className="error-state">
            <CircleAlert size={19} />
            <div>
              <strong>安全监控加载失败</strong>
              <span>{error}</span>
            </div>
          </div>
        )}

        <section className="ops-overview">
          <div className="section-heading">
            <div>
              <p className="eyebrow">LIVE STATUS</p>
              <h2>运行状态</h2>
            </div>
          </div>
          <div className="summary-strip ops-summary">
            <div className="summary-item">
              <Server size={17} />
              <div>
                <span>监控盘子</span>
                <strong>{overview?.environments.length ?? '--'}</strong>
              </div>
            </div>
            <div className="summary-item">
              <Cpu size={17} />
              <div>
                <span>在线进程</span>
                <strong>
                  {overview ? onlineCount + ' / ' + componentCount : '--'}
                </strong>
              </div>
            </div>
            <div className="summary-item summary-item--warning">
              <AlertTriangle size={17} />
              <div>
                <span>告警 / 严重</span>
                <strong>
                  {overview
                    ? warningEnvironments + ' / ' + criticalEnvironments
                    : '--'}
                </strong>
              </div>
            </div>
            <div className="summary-item summary-item--danger">
              <ShieldAlert size={17} />
              <div>
                <span>交易阻断</span>
                <strong>{overview ? blockRows.length : '--'}</strong>
              </div>
            </div>
          </div>
        </section>

        <section className="ops-section">
          <div className="ops-section__heading">
            <div>
              <p className="eyebrow">TRADING BLOCKS</p>
              <h2>交易阻断</h2>
            </div>
            <span className="ops-section__count">{blockRows.length}</span>
          </div>

          {overview && blockRows.length === 0 && (
            <div className="ops-empty">
              <CheckCircle2 size={18} />
              <span>当前没有币种级交易阻断</span>
            </div>
          )}

          {blockRows.length > 0 && (
            <div className="trading-block-list">
              {blockRows.map(({ environment, block }) => {
                const position = block.currentPosition
                return (
                  <article
                    className="trading-block-row"
                    key={
                      environment.strategySlug +
                      ':' +
                      block.symbol +
                      ':' +
                      block.blockedLeg
                    }
                  >
                    <div className="trading-block__identity">
                      <span>{environment.strategySlug}</span>
                      <strong>{block.symbol}</strong>
                      <small>
                        {block.venue} · {sideLabel(block.side)}
                      </small>
                    </div>
                    <div className="trading-block__error">
                      <span
                        className={
                          'leg-badge leg-badge--' + block.blockedLeg
                        }
                      >
                        {legLabel(block.blockedLeg)}
                      </span>
                      <strong>
                        {block.httpStatus ?? '--'} / {block.errorCode ?? '--'}
                      </strong>
                      <code>{block.errorLabel}</code>
                      <small>
                        {block.count} 次 · {timeFormatter.format(block.lastSeenAtMs)}
                      </small>
                    </div>
                    <div className="position-readout">
                      {block.positionStatus === 'live' && position ? (
                        <>
                          <div
                            className={
                              block.blockedLeg === 'margin'
                                ? 'is-blocked-leg'
                                : ''
                            }
                          >
                            <span>Margin</span>
                            <strong>{positionValue(position.marginQty)}</strong>
                            <small>{usdFormatter.format(position.marginUsd)}</small>
                          </div>
                          <div
                            className={
                              block.blockedLeg === 'futures'
                                ? 'is-blocked-leg'
                                : ''
                            }
                          >
                            <span>Futures</span>
                            <strong>{positionValue(position.futuresQty)}</strong>
                            <small>{usdFormatter.format(position.futuresUsd)}</small>
                          </div>
                          <div>
                            <span>Net</span>
                            <strong>{positionValue(position.netQty)}</strong>
                            <small>{usdFormatter.format(position.netUsd)}</small>
                          </div>
                        </>
                      ) : (
                        <div className="position-readout__error">
                          <CircleAlert size={15} />
                          <span>{block.positionError ?? '仓位读取失败'}</span>
                        </div>
                      )}
                    </div>
                  </article>
                )
              })}
            </div>
          )}
        </section>

        <section className="ops-section">
          <div className="ops-section__heading">
            <div>
              <p className="eyebrow">PROCESSES</p>
              <h2>进程矩阵</h2>
            </div>
          </div>
          <div className="process-matrix-wrap">
            <table className="process-matrix">
              <thead>
                <tr>
                  <th>盘子</th>
                  {roleOrder.map((role) => (
                    <th key={role}>{roleLabels[role]}</th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {overview?.environments.map((environment) => {
                  const byRole = new Map(
                    environment.components.map((component) => [
                      component.role,
                      component,
                    ]),
                  )
                  return (
                    <tr key={environment.strategySlug}>
                      <th>
                        <strong>{environment.strategySlug}</strong>
                        <small
                          className={'environment-health--' + environment.status}
                        >
                          {environmentLabel(environment.status)}
                        </small>
                      </th>
                      {roleOrder.map((role) => {
                        const component = byRole.get(role)
                        return (
                          <td key={role}>
                            {component ? (
                              <div
                                className={
                                  'process-state process-state--' + component.status
                                }
                                title={component.managerName}
                              >
                                <span className="status-dot" />
                                <strong>{healthLabel(component.status)}</strong>
                                <small>
                                  {component.pid ? 'PID ' + component.pid : '--'}
                                  {component.alerts.warningCount +
                                    component.alerts.errorCount >
                                    0 &&
                                    ' · ' +
                                      (component.alerts.warningCount +
                                        component.alerts.errorCount)}
                                </small>
                              </div>
                            ) : (
                              '--'
                            )}
                          </td>
                        )
                      })}
                    </tr>
                  )
                })}
              </tbody>
            </table>
          </div>
        </section>

        <section className="ops-section">
          <div className="ops-section__heading">
            <div>
              <p className="eyebrow">RECENT ALERTS</p>
              <h2>最近告警</h2>
            </div>
          </div>
          <div className="alert-feed">
            {alertRows.map(({ environment, component, sample }) => (
              <div
                className={'alert-feed__row alert-feed__row--' + sample.severity}
                key={
                  environment.strategySlug +
                  component.role +
                  sample.atMs +
                  sample.message
                }
              >
                <span>{timeFormatter.format(sample.atMs)}</span>
                <strong>{environment.strategySlug}</strong>
                <code>{roleLabels[component.role]}</code>
                <p>{sample.message}</p>
                <small>{sample.count > 1 ? '×' + sample.count : ''}</small>
              </div>
            ))}
            {overview && alertRows.length === 0 && (
              <div className="ops-empty">
                <Activity size={17} />
                <span>当前告警窗口为空</span>
              </div>
            )}
          </div>
        </section>
      </main>
    </>
  )
}
