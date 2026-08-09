import {
  Activity,
  ArrowRight,
  Ban,
  CheckCircle2,
  CircleAlert,
  Clock3,
  Database,
  Gauge,
  GitCompareArrows,
  Percent,
  RadioTower,
  ShieldCheck,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import {
  getAccountRisks,
  getAlignmentStatuses,
  getHistorySyncStatuses,
  getStrategies,
} from '../api'
import {
  alignmentLabel,
  alignmentTime,
  alignmentTitle,
  alignmentTone,
} from '../alignment'
import type {
  AccountRisk,
  AlignmentStatus,
  HistorySyncStatus,
  Strategy,
} from '../types'

type Filter = 'all' | 'funding_rate' | 'intra_exchange' | 'market_making'

const filters: Array<{ value: Filter; label: string }> = [
  { value: 'all', label: '全部' },
  { value: 'funding_rate', label: '资金费' },
  { value: 'intra_exchange', label: '所内套利' },
  { value: 'market_making', label: '做市' },
]

const SYNC_INTERVAL_MS = 15 * 60 * 1_000
const syncTimeFormatter = new Intl.DateTimeFormat(undefined, {
  month: '2-digit',
  day: '2-digit',
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
  hour12: false,
})

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

function usesUniMmr(strategy: Strategy) {
  return (
    strategy.strategyKind !== 'market_making' &&
    strategy.accountMode !== 'usdm_futures'
  )
}

function exchangeMark(exchange: Strategy['exchange']) {
  switch (exchange) {
    case 'binance':
      return 'B'
    case 'bybit':
      return 'Y'
    case 'gate':
      return 'G'
    case 'bitget':
      return 'BG'
    case 'okx':
      return 'O'
  }
}

function formatUniMmr(risk: AccountRisk | undefined) {
  if (!risk) return '--'
  if (risk.maintenanceMarginUsd !== null && risk.maintenanceMarginUsd <= 0) {
    return '∞'
  }
  if (risk.uniMmr === null) return '--'
  if (risk.uniMmr >= 100) return risk.uniMmr.toFixed(1)
  return risk.uniMmr.toFixed(2)
}

function riskStatusLabel(risk: AccountRisk | undefined, host: string) {
  if (!risk) return host === 'local' ? '未配置 IPC' : '远端未接入'
  if (risk.status === 'unavailable') return 'Monitor 未连接'
  if (risk.status === 'waiting') return '等待风险快照'
  if (risk.status === 'stale') return '数据延迟'
  if (risk.maintenanceMarginUsd !== null && risk.maintenanceMarginUsd <= 0) {
    return '无保证金占用'
  }
  return '实时'
}

function syncIsHealthy(status: HistorySyncStatus | undefined, nowMs: number) {
  return (
    status?.scheduled === true &&
    status.lastFetchedAtMs !== null &&
    nowMs - status.lastFetchedAtMs < SYNC_INTERVAL_MS
  )
}

function syncTone(status: HistorySyncStatus | undefined, nowMs: number) {
  if (!status) return 'loading'
  if (!status.scheduled) return 'disabled'
  if (status.lastFetchedAtMs === null) return 'waiting'
  return syncIsHealthy(status, nowMs) ? 'healthy' : 'stale'
}

function syncStatusLabel(status: HistorySyncStatus | undefined, nowMs: number) {
  if (!status) return '读取中'
  if (!status.scheduled) return '未启用'
  if (status.lastFetchedAtMs === null) return '尚未成功'
  if (syncIsHealthy(status, nowMs)) return '正常'
  const ageMinutes = Math.floor((nowMs - status.lastFetchedAtMs) / 60_000)
  return `延迟 ${Math.max(0, ageMinutes)}m`
}

function syncTime(status: HistorySyncStatus | undefined) {
  if (!status?.scheduled) return '--'
  if (status.lastFetchedAtMs === null) return '无成功记录'
  return syncTimeFormatter.format(status.lastFetchedAtMs)
}

function syncStatusTitle(status: HistorySyncStatus | undefined) {
  if (!status) return '正在读取拉取状态'
  if (!status.scheduled) return '该策略未纳入 15 分钟定时拉取'
  return status.datasets
    .map((dataset) => {
      const time =
        dataset.fetchedAtMs === null
          ? '无成功记录'
          : syncTimeFormatter.format(dataset.fetchedAtMs)
      return `${dataset.dataset}: ${time}`
    })
    .join('\n')
}

export function IndexPage() {
  const [strategies, setStrategies] = useState<Strategy[]>([])
  const [accountRisks, setAccountRisks] = useState<AccountRisk[]>([])
  const [syncStatuses, setSyncStatuses] = useState<HistorySyncStatus[]>([])
  const [alignmentStatuses, setAlignmentStatuses] = useState<AlignmentStatus[]>([])
  const [nowMs, setNowMs] = useState(() => Date.now())
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

  useEffect(() => {
    const controller = new AbortController()
    const refresh = () => {
      getAlignmentStatuses(controller.signal)
        .then(setAlignmentStatuses)
        .catch(() => undefined)
    }
    refresh()
    const timer = window.setInterval(refresh, 3_000)
    return () => {
      controller.abort()
      window.clearInterval(timer)
    }
  }, [])

  useEffect(() => {
    const controller = new AbortController()
    const refresh = () => {
      getAccountRisks(controller.signal)
        .then(setAccountRisks)
        .catch(() => undefined)
    }
    refresh()
    const timer = window.setInterval(refresh, 3_000)
    return () => {
      controller.abort()
      window.clearInterval(timer)
    }
  }, [])

  useEffect(() => {
    const controller = new AbortController()
    const refresh = () => {
      setNowMs(Date.now())
      getHistorySyncStatuses(controller.signal)
        .then(setSyncStatuses)
        .catch(() => undefined)
    }
    refresh()
    const timer = window.setInterval(refresh, 30_000)
    return () => {
      controller.abort()
      window.clearInterval(timer)
    }
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
  const risksBySlug = useMemo(
    () => new Map(accountRisks.map((risk) => [risk.strategySlug, risk])),
    [accountRisks],
  )
  const syncBySlug = useMemo(
    () => new Map(syncStatuses.map((status) => [status.strategySlug, status])),
    [syncStatuses],
  )
  const alignmentBySlug = useMemo(
    () =>
      new Map(
        alignmentStatuses.map((status) => [status.strategySlug, status]),
      ),
    [alignmentStatuses],
  )
  const connectedRiskCount = accountRisks.filter(
    (risk) => risk.connected,
  ).length
  const scheduledSyncs = syncStatuses.filter((status) => status.scheduled)
  const healthySyncCount = scheduledSyncs.filter((status) =>
    syncIsHealthy(status, nowMs),
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
          <div className="header-actions">
            <Link
              className="header-nav-link"
              to="/market-data"
              aria-label="行情网络"
              title="行情网络"
            >
              <RadioTower size={16} />
              <span>行情网络</span>
            </Link>
            <Link
              className="header-nav-link"
              to="/intra-matching"
              aria-label="订单匹配"
              title="订单匹配"
            >
              <GitCompareArrows size={16} />
              <span>订单匹配</span>
            </Link>
            <Link
              className="header-nav-link"
              to="/fee-rates"
              aria-label="手续费"
              title="手续费"
            >
              <Percent size={16} />
              <span>手续费</span>
            </Link>
            <Link
              className="header-nav-link"
              to="/fr-position-limits"
              aria-label="FR 限仓"
              title="FR 限仓"
            >
              <Gauge size={16} />
              <span>FR 限仓</span>
            </Link>
            <div className="system-state">
              <span className="status-dot status-dot--ready" />
              服务在线
            </div>
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
            <div className="summary-item">
              <ShieldCheck size={17} />
              <div>
                <span>UniMMR IPC</span>
                <strong>{connectedRiskCount} / {accountRisks.length}</strong>
              </div>
            </div>
            <div className="summary-item">
              <Clock3 size={17} />
              <div>
                <span>15min 拉取正常</span>
                <strong>{healthySyncCount} / {scheduledSyncs.length}</strong>
              </div>
            </div>
          </div>
        </section>

        <section className="retired-notice" aria-label="已停用盘子">
          <Ban size={16} />
          <strong>binance nova01 / nova02</strong>
          <span>已停用，不再拉取</span>
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
            {visibleStrategies.map((strategy) => {
              const risk = risksBySlug.get(strategy.slug)
              const syncStatus = syncBySlug.get(strategy.slug)
              const alignmentStatus = alignmentBySlug.get(strategy.slug)
              const riskTone =
                risk?.status === 'live'
                  ? (risk.riskLevel ?? 'live')
                  : (risk?.status ?? 'missing')
              return (
                <Link
                  className={
                    'strategy-card strategy-card--' + strategy.exchange
                  }
                  to={'/strategies/' + strategy.slug}
                  key={strategy.slug}
                >
                  <div className="strategy-card__top">
                    <span className="exchange-mark" aria-hidden="true">
                      {exchangeMark(strategy.exchange)}
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
                    {usesUniMmr(strategy) && (
                      <div className={'account-risk account-risk--' + riskTone}>
                        <div>
                          <span>UniMMR</span>
                          <strong>{formatUniMmr(risk)}</strong>
                        </div>
                        <small>{riskStatusLabel(risk, strategy.host)}</small>
                      </div>
                    )}
                    <div
                      className={
                        'history-sync history-sync--' +
                        syncTone(syncStatus, nowMs)
                      }
                      title={syncStatusTitle(syncStatus)}
                    >
                      <div>
                        <span>最近完整拉取</span>
                        <strong>{syncTime(syncStatus)}</strong>
                      </div>
                      <small>
                        <span className="status-dot" />
                        {syncStatusLabel(syncStatus, nowMs)}
                      </small>
                    </div>
                    {alignmentStatus && (
                      <div
                        className={
                          'alignment-check alignment-check--' +
                          alignmentTone(alignmentStatus)
                        }
                        title={alignmentTitle(alignmentStatus)}
                      >
                        <GitCompareArrows size={15} />
                        <div>
                          <span>订单校对</span>
                          <strong>{alignmentTime(alignmentStatus)}</strong>
                          {alignmentStatus.state === 'running' && (
                            <i
                              aria-hidden="true"
                              style={{
                                width: alignmentStatus.progressPercent + '%',
                              }}
                            />
                          )}
                        </div>
                        <small>
                          <span className="status-dot" />
                          {alignmentLabel(alignmentStatus)}
                        </small>
                      </div>
                    )}
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
              )
            })}
          </div>
        )}
      </main>
    </>
  )
}
