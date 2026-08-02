import {
  Activity,
  ArrowLeft,
  CheckCircle2,
  CircleAlert,
  Clock3,
  Database,
  GitCompareArrows,
  RefreshCw,
  TimerReset,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import {
  getAlignmentStatuses,
  getIntraMatchingSummaries,
} from '../api'
import type {
  AlignmentStatus,
  IntraMatchingSummary,
} from '../types'

const strategyOrder = [
  'binance-intra-arb01',
  'bybit-intra-arb01',
  'bybit-intra-arb02',
]

function formatTimeMs(ms: number | null) {
  if (!ms || ms <= 0) return '—'
  return new Intl.DateTimeFormat('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
    hour12: false,
  }).format(new Date(ms))
}

function formatTimeUs(us: number) {
  return formatTimeMs(us > 0 ? Math.trunc(us / 1_000) : null)
}

function formatNumber(value: number, maximumFractionDigits = 2) {
  return value.toLocaleString('en-US', { maximumFractionDigits })
}

function exchangeLabel(exchange: IntraMatchingSummary['exchange']) {
  return exchange === 'binance' ? 'Binance' : 'Bybit'
}

function alignmentTone(status?: AlignmentStatus) {
  if (!status) return 'waiting'
  if (status?.state === 'succeeded' && (status.mismatchCount ?? 0) === 0) {
    return 'ok'
  }
  if (status?.state === 'running' || status?.state === 'waiting') {
    return 'waiting'
  }
  return 'danger'
}

function alignmentLabel(status?: AlignmentStatus) {
  if (!status) return '等待校验'
  if (status.state === 'succeeded' && (status.mismatchCount ?? 0) === 0) {
    return '成交已对齐'
  }
  if (status.state === 'running') return '正在校验'
  if (status.state === 'waiting') return '等待校验'
  if (status.state === 'mismatch') {
    return formatNumber(status.mismatchCount ?? 0, 0) + ' 条差异'
  }
  return '校验失败'
}

function writeTone(
  summary: IntraMatchingSummary,
  alignment?: AlignmentStatus,
) {
  if (summary.sourceReadThroughUs <= 0 || summary.updatedAtMs <= 0) {
    return 'waiting'
  }
  const releaseFloor =
    summary.sourceReadThroughUs - summary.reorderWindowUs - 1_000_000
  if (summary.eventsReleasedThroughUs < releaseFloor) return 'warning'
  const targetUs =
    (alignment?.actualEndMs ?? summary.verifiedThroughMs) * 1_000
  return summary.sourceReadThroughUs >= targetUs ? 'ok' : 'warning'
}

function writeLabel(
  summary: IntraMatchingSummary,
  alignment?: AlignmentStatus,
) {
  const tone = writeTone(summary, alignment)
  if (tone === 'waiting') return '等待首次落盘'
  const targetUs =
    (alignment?.actualEndMs ?? summary.verifiedThroughMs) * 1_000
  if (summary.sourceReadThroughUs < targetUs) return '等待增量落盘'
  if (tone === 'ok') return '订单持续落盘'
  return '事件释放滞后'
}

function StatusIcon({ tone }: { tone: string }) {
  if (tone === 'ok') return <CheckCircle2 size={17} />
  if (tone === 'waiting') return <Clock3 size={17} />
  return <CircleAlert size={17} />
}

interface WatermarkRowProps {
  icon: typeof Database
  label: string
  value: string
  detail: string
}

function WatermarkRow({
  icon: Icon,
  label,
  value,
  detail,
}: WatermarkRowProps) {
  return (
    <div className="intra-watermark-row">
      <span className="intra-watermark-icon" aria-hidden="true">
        <Icon size={16} />
      </span>
      <div>
        <span>{label}</span>
        <strong>{value}</strong>
      </div>
      <small>{detail}</small>
    </div>
  )
}

export function IntraMatchingPage() {
  const [summaries, setSummaries] = useState<IntraMatchingSummary[]>([])
  const [alignments, setAlignments] = useState<AlignmentStatus[]>([])
  const [selectedSlug, setSelectedSlug] = useState(strategyOrder[0])
  const [loading, setLoading] = useState(true)
  const [refreshing, setRefreshing] = useState(false)
  const [error, setError] = useState<string | null>(null)

  async function load(signal?: AbortSignal, refresh = false) {
    if (refresh) setRefreshing(true)
    try {
      const [nextSummaries, nextAlignments] = await Promise.all([
        getIntraMatchingSummaries(signal),
        getAlignmentStatuses(signal),
      ])
      setSummaries(nextSummaries)
      setAlignments(nextAlignments)
      setError(null)
    } catch (reason: unknown) {
      if (reason instanceof DOMException && reason.name === 'AbortError') {
        return
      }
      setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      setLoading(false)
      if (refresh) setRefreshing(false)
    }
  }

  useEffect(() => {
    const controller = new AbortController()
    void load(controller.signal)
    const interval = window.setInterval(() => {
      void load(controller.signal)
    }, 15_000)
    return () => {
      controller.abort()
      window.clearInterval(interval)
    }
  }, [])

  const orderedSummaries = useMemo(
    () =>
      summaries
        .slice()
        .sort(
          (left, right) =>
            strategyOrder.indexOf(left.strategySlug) -
            strategyOrder.indexOf(right.strategySlug),
        ),
    [summaries],
  )
  const alignmentBySlug = useMemo(
    () =>
      new Map(
        alignments.map((alignment) => [
          alignment.strategySlug,
          alignment,
        ]),
      ),
    [alignments],
  )
  const selected =
    orderedSummaries.find(
      (summary) => summary.strategySlug === selectedSlug,
    ) ?? orderedSummaries[0]
  const alignment = selected
    ? alignmentBySlug.get(selected.strategySlug)
    : undefined
  const writeState = selected
    ? writeTone(selected, alignment)
    : 'waiting'
  const tradeState = alignmentTone(alignment)

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
              <p>Intra 订单状态</p>
            </div>
          </Link>
          <Link className="header-nav-link" to="/">
            <ArrowLeft size={16} />
            盘子总览
          </Link>
        </div>
      </header>

      <main className="page-shell intra-page">
        <section className="intra-overview">
          <div className="section-heading">
            <div>
              <p className="eyebrow">INTRA ORDER SYNTHESIS</p>
              <h2>订单落盘与成交对齐</h2>
            </div>
            <button
              className="refresh-button"
              type="button"
              aria-label="刷新订单匹配状态"
              title="刷新"
              onClick={() => void load(undefined, true)}
              disabled={refreshing}
            >
              <RefreshCw
                size={15}
                className={refreshing ? 'is-spinning' : ''}
              />
              刷新
            </button>
          </div>

          <div className="intra-tabs" role="tablist" aria-label="Intra 盘子">
            {orderedSummaries.map((summary) => {
              const itemAlignment = alignmentBySlug.get(
                summary.strategySlug,
              )
              const healthy =
                writeTone(summary, itemAlignment) === 'ok' &&
                alignmentTone(itemAlignment) === 'ok'
              return (
                <button
                  key={summary.strategySlug}
                  type="button"
                  role="tab"
                  aria-selected={summary.strategySlug === selected?.strategySlug}
                  className={
                    summary.strategySlug === selected?.strategySlug
                      ? 'is-active'
                      : ''
                  }
                  onClick={() => setSelectedSlug(summary.strategySlug)}
                >
                  <span
                    className={
                      'intra-tab-dot ' +
                      (healthy ? 'is-ok' : 'is-warning')
                    }
                  />
                  <span>
                    <strong>{summary.displayName}</strong>
                    <small>{exchangeLabel(summary.exchange)}</small>
                  </span>
                </button>
              )
            })}
          </div>
        </section>

        {loading && (
          <div className="intra-loading" aria-label="正在加载订单状态" />
        )}
        {error && (
          <div className="error-state">
            <CircleAlert size={19} />
            <div>
              <strong>订单状态加载失败</strong>
              <span>{error}</span>
            </div>
          </div>
        )}

        {!loading && !error && selected && (
          <div className="intra-content" aria-live="polite">
            <section className="intra-health-band">
              <div className={'intra-health-item is-' + writeState}>
                <StatusIcon tone={writeState} />
                <div>
                  <span>订单落盘</span>
                  <strong>{writeLabel(selected, alignment)}</strong>
                  <small>{formatTimeMs(selected.updatedAtMs)}</small>
                </div>
              </div>
              <div className={'intra-health-item is-' + tradeState}>
                <StatusIcon tone={tradeState} />
                <div>
                  <span>Trade 对齐</span>
                  <strong>{alignmentLabel(alignment)}</strong>
                  <small>{formatTimeMs(alignment?.actualEndMs ?? null)}</small>
                </div>
              </div>
              <div className="intra-health-item is-neutral">
                <GitCompareArrows size={17} />
                <div>
                  <span>待平仓开仓</span>
                  <strong>{formatNumber(selected.pendingOrders, 0)}</strong>
                  <small>
                    {formatNumber(selected.pendingRemainingAmount, 8)} qty
                  </small>
                </div>
              </div>
            </section>

            <section className="intra-metrics" aria-label="订单匹配汇总">
              <div>
                <span>开仓总数</span>
                <strong>{formatNumber(selected.totalOrders, 0)}</strong>
              </div>
              <div>
                <span>完整匹配</span>
                <strong>{formatNumber(selected.completedOrders, 0)}</strong>
              </div>
              <div>
                <span>部分匹配</span>
                <strong>{formatNumber(selected.mixedOrders, 0)}</strong>
              </div>
              <div>
                <span>已净额处理</span>
                <strong>{formatNumber(selected.nettedOrders, 0)}</strong>
              </div>
              <div>
                <span>未分配 Hedge</span>
                <strong>{formatNumber(selected.unallocatedHedges, 0)}</strong>
              </div>
              <div>
                <span>Pending 名义金额</span>
                <strong>$ {formatNumber(selected.pendingNotional)}</strong>
              </div>
            </section>

            <section className="intra-watermarks">
              <header>
                <div>
                  <p className="eyebrow">WATERMARKS</p>
                  <h3>增量合成水位</h3>
                </div>
                <span>{selected.strategySlug}</span>
              </header>
              <div className="intra-watermark-list">
                <WatermarkRow
                  icon={Database}
                  label="RocksDB 读取水位"
                  value={formatTimeUs(selected.sourceReadThroughUs)}
                  detail="订单源已读取"
                />
                <WatermarkRow
                  icon={TimerReset}
                  label="事件释放水位"
                  value={formatTimeUs(selected.eventsReleasedThroughUs)}
                  detail={
                    '重排窗口 ' +
                    formatNumber(selected.reorderWindowUs / 1_000_000, 0) +
                    's'
                  }
                />
                <WatermarkRow
                  icon={GitCompareArrows}
                  label="Margin 截断水位"
                  value={formatTimeUs(selected.marginFinalizedThroughUs)}
                  detail="前序开仓已结算"
                />
                <WatermarkRow
                  icon={CheckCircle2}
                  label="Trade 校验终点"
                  value={formatTimeMs(alignment?.actualEndMs ?? null)}
                  detail={
                    (alignment?.mismatchCount ?? 0) === 0
                      ? '0 条差异'
                      : formatNumber(alignment?.mismatchCount ?? 0, 0) +
                        ' 条差异'
                  }
                />
              </div>
            </section>

            {(selected.unallocatedHedges > 0 ||
              selected.anchorMisses > 0) && (
              <section className="intra-warning-band">
                <CircleAlert size={17} />
                <div>
                  <strong>存在待处理 Hedge</strong>
                  <span>
                    未分配 {formatNumber(selected.unallocatedHedges, 0)} 条
                    · 锚点缺失 {formatNumber(selected.anchorMisses, 0)} 条
                    · 数量 {formatNumber(selected.unallocatedAmount, 8)}
                  </span>
                </div>
              </section>
            )}
          </div>
        )}
      </main>
    </>
  )
}
