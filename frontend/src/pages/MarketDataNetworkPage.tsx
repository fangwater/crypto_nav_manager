import {
  ArrowLeft,
  CheckCircle2,
  CircleAlert,
  Network,
  RadioTower,
  RefreshCw,
  RotateCcw,
  TriangleAlert,
  WifiOff,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import {
  getMarketDataSnapshot,
  type MarketDataSnapshot,
  type MarketDataTarget,
} from '../marketDataApi'

type FeedLevel = 'ok' | 'warn' | 'critical' | 'unknown'

const timeFormatter = new Intl.DateTimeFormat(undefined, {
  month: '2-digit',
  day: '2-digit',
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
  hour12: false,
})

function valueOrZero(value: number | null) {
  return value ?? 0
}

function feedLevel(target: MarketDataTarget): FeedLevel {
  const network = target.network
  if (
    !target.process ||
    target.process_candidates !== 1 ||
    network.established_count === 0 ||
    (network.rx_idle_secs ?? 0) >= 120 ||
    valueOrZero(network.retransmits) >= 10 ||
    valueOrZero(network.socket_drops) >= 10 ||
    valueOrZero(network.reconnects) >= 10 ||
    network.recv_queue_bytes >= 1_048_576
  ) {
    return 'critical'
  }
  if (network.rx_bytes === null) return 'unknown'
  if (
    (network.rx_idle_secs ?? 0) >= 30 ||
    valueOrZero(network.retransmits) > 0 ||
    valueOrZero(network.socket_drops) > 0 ||
    valueOrZero(network.reconnects) >= 2 ||
    valueOrZero(network.disconnects) > 0 ||
    network.recv_queue_bytes >= 65_536
  ) {
    return 'warn'
  }
  return 'ok'
}

function feedLabel(target: MarketDataTarget, level: FeedLevel) {
  if (!target.process) return '进程缺失'
  if (target.process_candidates !== 1) return '进程重复'
  if (target.network.established_count === 0) return '行情断开'
  if ((target.network.rx_idle_secs ?? 0) >= 120) return '行情中断'
  if (valueOrZero(target.network.reconnects) >= 2) return '频繁重连'
  if (valueOrZero(target.network.disconnects) > 0) return '连接切换'
  if (
    valueOrZero(target.network.retransmits) > 0 ||
    valueOrZero(target.network.socket_drops) > 0
  ) {
    return '网络抖动'
  }
  if (level === 'unknown') return '采样中'
  if (target.network.rx_bytes === 0) return '等待行情'
  return '持续接收'
}

function formatBytes(bytes: number | null) {
  if (bytes === null) return '--'
  if (bytes < 1_024) return bytes + ' B'
  if (bytes < 1_048_576) return (bytes / 1_024).toFixed(1) + ' KB'
  return (bytes / 1_048_576).toFixed(1) + ' MB'
}

function formatSeconds(seconds: number | null) {
  if (seconds === null) return '--'
  if (seconds < 1) return '< 1 秒'
  return Math.round(seconds) + ' 秒'
}

function feedReason(target: MarketDataTarget) {
  const reason = target.reasons.find(
    (item) =>
      !item.startsWith('CPU affinity') &&
      !item.startsWith('current CPU') &&
      !item.startsWith('process, affinity'),
  )
  if (!reason) return null
  if (reason === 'target process is missing') return '行情进程不存在'
  if (reason.includes('matching processes found')) return '检测到重复行情进程'
  if (reason.startsWith('RX has remained zero')) return '行情持续没有新数据'
  if (reason === 'no established TCP socket') return 'TCP 行情连接已断开'
  if (reason.includes('receive queue')) return '接收队列正在积压'
  if (reason.startsWith('TCP retransmissions')) return 'TCP 重传达到告警阈值'
  if (reason.startsWith('socket drops')) return 'Socket 丢包达到告警阈值'
  if (reason.startsWith('reconnects')) return '重连频率达到告警阈值'
  if (reason === 'no completed sampling window') return '等待首个采样窗口'
  return reason
}

export function MarketDataNetworkPage() {
  const [snapshot, setSnapshot] = useState<MarketDataSnapshot | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [refreshKey, setRefreshKey] = useState(0)

  useEffect(() => {
    let active = true
    let controller = new AbortController()
    const refresh = () => {
      controller.abort()
      const requestController = new AbortController()
      controller = requestController
      getMarketDataSnapshot(requestController.signal)
        .then((next) => {
          if (!active) return
          setSnapshot(next)
          setError(null)
        })
        .catch((reason: unknown) => {
          if (!active || requestController.signal.aborted) return
          setError(reason instanceof Error ? reason.message : String(reason))
        })
    }
    refresh()
    const timer = window.setInterval(refresh, 5_000)
    return () => {
      active = false
      controller.abort()
      window.clearInterval(timer)
    }
  }, [refreshKey])

  const rows = useMemo(
    () =>
      snapshot?.targets.map((target) => ({
        target,
        level: feedLevel(target),
      })) ?? [],
    [snapshot],
  )
  const healthyCount = rows.filter((row) => row.level === 'ok').length
  const reconnectCount = rows.reduce(
    (total, row) => total + valueOrZero(row.target.network.reconnects),
    0,
  )
  const disconnectedCount = rows.filter(
    ({ target }) =>
      !target.process ||
      target.network.established_count === 0 ||
      (target.network.rx_idle_secs ?? 0) >= 120,
  ).length
  const hasFeedProblem = rows.some(
    (row) => row.level !== 'ok',
  )
  const stale = snapshot
    ? Date.now() - snapshot.timestamp_unix_ms > 30_000
    : false
  const windowSecs = snapshot?.window_secs
  const allClear =
    rows.length > 0 &&
    healthyCount === rows.length &&
    snapshot?.system.status === 'OK'

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
            <span className="market-network-title-mark" aria-hidden="true">
              <RadioTower size={19} />
            </span>
            <div>
              <p>MARKET DATA</p>
              <h1>行情网络</h1>
            </div>
          </div>
          <div className="market-network-header-state">
            <div className="system-state">
              <span
                className={
                  'status-dot ' +
                  (error || stale
                    ? 'status-dot--danger'
                    : hasFeedProblem
                      ? 'status-dot--warning'
                      : 'status-dot--ready')
                }
              />
              {error
                ? '监控连接异常'
                : stale
                  ? '数据已过期'
                  : snapshot
                    ? timeFormatter.format(snapshot.timestamp_unix_ms)
                    : '连接中'}
            </div>
            <button
              className="icon-button"
              type="button"
              onClick={() => setRefreshKey((value) => value + 1)}
              aria-label="刷新行情网络状态"
              title="刷新"
            >
              <RefreshCw size={16} />
            </button>
          </div>
        </div>
      </header>

      <main className="detail-shell market-network-shell">
        {error && !snapshot && (
          <div className="error-state">
            <CircleAlert size={19} />
            <div>
              <strong>行情网络监控不可用</strong>
              <span>{error}</span>
            </div>
          </div>
        )}

        <section className="market-network-overview">
          <div className="section-heading">
            <div>
              <p className="eyebrow">LIVE NETWORK</p>
              <h2>行情链路状态</h2>
            </div>
            {snapshot && (
              <span className="market-network-window">
                {snapshot.host.hostname} · 最近{' '}
                {formatSeconds(snapshot.window_secs)}窗口
              </span>
            )}
          </div>

          <div className="summary-strip market-network-summary">
            <div className="summary-item">
              <CheckCircle2 size={17} />
              <div>
                <span>健康通道</span>
                <strong>{snapshot ? healthyCount + ' / ' + rows.length : '--'}</strong>
              </div>
            </div>
            <div className="summary-item">
              <RotateCcw size={17} />
              <div>
                <span>窗口重连</span>
                <strong>{snapshot ? reconnectCount : '--'}</strong>
              </div>
            </div>
            <div className="summary-item">
              <WifiOff size={17} />
              <div>
                <span>行情断开</span>
                <strong>{snapshot ? disconnectedCount : '--'}</strong>
              </div>
            </div>
            <div
              className={
                'summary-item ' +
                (snapshot?.system.status === 'OK'
                  ? ''
                  : 'summary-item--warning')
              }
            >
              <Network size={17} />
              <div>
                <span>整机网络</span>
                <strong>
                  {snapshot
                    ? snapshot.system.status === 'OK'
                      ? '正常'
                      : snapshot.system.status === 'UNKNOWN'
                        ? '采样中'
                        : '告警'
                    : '--'}
                </strong>
              </div>
            </div>
          </div>
        </section>

        {snapshot && allClear && !stale && (
          <div className="market-network-all-clear">
            <CheckCircle2 size={16} />
            所有行情通道持续接收，当前窗口未见断流或频繁重连。
          </div>
        )}

        <section className="market-network-feeds">
          <div className="market-network-section-heading">
            <h2>行情通道</h2>
            <span>自动刷新</span>
          </div>
          <div className="market-feed-list">
            {rows.map(({ target, level }) => {
              const network = target.network
              const eventCount =
                valueOrZero(network.retransmits) +
                valueOrZero(network.socket_drops)
              const reason = feedReason(target)
              return (
                <div className="market-feed-row" key={target.name}>
                  <div className="market-feed-identity">
                    <strong>{target.name}</strong>
                    <span>{target.venue}</span>
                    <small>
                      {target.process
                        ? 'PID ' + target.process.pid
                        : '无运行进程'}
                    </small>
                  </div>

                  <div className={'market-feed-status market-feed-status--' + level}>
                    <span className="status-dot" />
                    <div>
                      <strong>{feedLabel(target, level)}</strong>
                      <small>{reason ?? '网络状态正常'}</small>
                    </div>
                  </div>

                  <div className="market-feed-reading">
                    <span>行情接收</span>
                    <strong>
                      {formatBytes(network.rx_bytes)}
                      {windowSecs
                        ? ' / ' + Math.round(windowSecs) + 's'
                        : ''}
                    </strong>
                    <small>
                      {network.rx_bytes === 0
                        ? '静默 ' + formatSeconds(network.rx_idle_secs)
                        : '窗口内持续有数据'}
                    </small>
                  </div>

                  <div className="market-feed-reading">
                    <span>TCP 连接</span>
                    <strong>
                      {network.established_count} / {network.socket_count}
                    </strong>
                    <small>
                      {valueOrZero(network.reconnects) > 0 ||
                      valueOrZero(network.disconnects) > 0
                        ? '重连 ' +
                          valueOrZero(network.reconnects) +
                          ' · 断开 ' +
                          valueOrZero(network.disconnects)
                        : '连接稳定'}
                    </small>
                  </div>

                  <div className="market-feed-reading market-feed-events">
                    <span>网络异常</span>
                    <strong className={eventCount > 0 ? 'is-warning' : ''}>
                      {eventCount > 0 ? eventCount : '无'}
                    </strong>
                    <small>
                      {eventCount > 0
                        ? '重传 ' +
                          valueOrZero(network.retransmits) +
                          ' · 丢包 ' +
                          valueOrZero(network.socket_drops)
                        : '未见重传或丢包'}
                    </small>
                  </div>
                </div>
              )
            })}
            {!snapshot && !error && (
              <div className="market-network-loading">
                <RadioTower size={18} />
                正在建立首个采样窗口
              </div>
            )}
          </div>
        </section>

        {snapshot && snapshot.system.status !== 'OK' && (
          <div className="market-network-host-warning">
            <TriangleAlert size={16} />
            <div>
              <strong>整机网络存在告警</strong>
              <span>{snapshot.system.reasons.join('；')}</span>
            </div>
          </div>
        )}
      </main>
    </>
  )
}
