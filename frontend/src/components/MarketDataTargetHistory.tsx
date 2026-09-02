import { HeatmapChart, ScatterChart } from 'echarts/charts'
import {
  GridComponent,
  TooltipComponent,
  VisualMapComponent,
} from 'echarts/components'
import * as echarts from 'echarts/core'
import { CanvasRenderer } from 'echarts/renderers'
import { useEffect, useMemo, useRef } from 'react'
import type {
  MarketDataHealth,
  MarketDataHistory,
  MarketDataHistoryPoint,
  MarketDataTargetHistory,
} from '../marketDataApi'
import './MarketDataHistoryChart.css'

echarts.use([
  HeatmapChart,
  ScatterChart,
  GridComponent,
  TooltipComponent,
  VisualMapComponent,
  CanvasRenderer,
])

const TARGET_ORDER = [
  'spp_bn_mg',
  'spp_bn_fu_market',
  'spp_bn_fu_bookticker',
  'spp_gt_bo',
  'spp_bg_bo',
  'spp_ok_bo',
]

const DISPLAY_BUCKET_SECS = 5 * 60

const STATUS_CODE: Record<MarketDataHealth, number> = {
  OK: 0,
  UNKNOWN: 1,
  WARN: 2,
  CRITICAL: 3,
}

const STATUS_LABEL: Record<MarketDataHealth, string> = {
  OK: '正常',
  UNKNOWN: '未知',
  WARN: '告警',
  CRITICAL: '中断',
}

const STATUS_RANK: Record<MarketDataHealth, number> = {
  OK: 0,
  UNKNOWN: 1,
  WARN: 2,
  CRITICAL: 3,
}

const tooltipTimeFormatter = new Intl.DateTimeFormat(undefined, {
  month: '2-digit',
  day: '2-digit',
  hour: '2-digit',
  minute: '2-digit',
  hour12: false,
})

const axisTimeFormatter = new Intl.DateTimeFormat(undefined, {
  hour: '2-digit',
  minute: '2-digit',
  hour12: false,
})

interface HistoryDatum {
  value: [string, string, number]
  point: MarketDataHistoryPoint
  targetName: string
  venue: string
  itemStyle?: {
    color: string
  }
}

interface TargetTimelineProps {
  target: MarketDataTargetHistory
  points: MarketDataHistoryPoint[]
  slots: string[]
}

interface MarketDataHistoryChartProps {
  history: MarketDataHistory
}

function escapeHtml(value: string) {
  return value.replace(
    /[&<>"']/g,
    (character) =>
      ({
        '&': '&amp;',
        '<': '&lt;',
        '>': '&gt;',
        '"': '&quot;',
        "'": '&#39;',
      })[character] ?? character,
  )
}

function formatBytes(bytes: number) {
  if (bytes < 1_024) return bytes + ' B'
  if (bytes < 1_048_576) return (bytes / 1_024).toFixed(1) + ' KB'
  return (bytes / 1_048_576).toFixed(1) + ' MB'
}

function tooltipFormatter(raw: unknown) {
  const params = raw as { data?: HistoryDatum }
  const datum = params.data
  if (!datum?.point) return ''
  const point = datum.point
  return [
    '<strong>' + escapeHtml(datum.targetName) + '</strong>',
    '<span class="market-history-tooltip__venue">' +
      escapeHtml(datum.venue) +
      '</span>',
    tooltipTimeFormatter.format(point.bucket_start_unix_ms),
    '状态：' + STATUS_LABEL[point.status],
    '接收：' + formatBytes(point.rx_bytes),
    '重连 / 断开：' + point.reconnects + ' / ' + point.disconnects,
    '重传 / 丢包：' + point.retransmits + ' / ' + point.socket_drops,
    '最长静默：' + Math.round(point.max_rx_idle_secs) + ' 秒',
  ].join('<br />')
}

function aggregatePoints(
  points: MarketDataHistoryPoint[],
  bucketMs: number,
): MarketDataHistoryPoint[] {
  const buckets = new Map<number, MarketDataHistoryPoint>()

  for (const point of points) {
    const bucketStart =
      Math.floor(point.bucket_start_unix_ms / bucketMs) * bucketMs
    const current = buckets.get(bucketStart)
    if (!current) {
      buckets.set(bucketStart, {
        ...point,
        bucket_start_unix_ms: bucketStart,
      })
      continue
    }

    if (STATUS_RANK[point.status] > STATUS_RANK[current.status]) {
      current.status = point.status
    }
    current.samples += point.samples
    current.rx_bytes += point.rx_bytes
    current.reconnects += point.reconnects
    current.disconnects += point.disconnects
    current.retransmits += point.retransmits
    current.socket_drops += point.socket_drops
    current.max_rx_idle_secs = Math.max(
      current.max_rx_idle_secs,
      point.max_rx_idle_secs,
    )
    current.max_recv_queue_bytes = Math.max(
      current.max_recv_queue_bytes,
      point.max_recv_queue_bytes,
    )
    current.min_established_count = Math.min(
      current.min_established_count,
      point.min_established_count,
    )
    current.max_socket_count = Math.max(
      current.max_socket_count,
      point.max_socket_count,
    )
  }

  return [...buckets.values()].sort(
    (left, right) =>
      left.bucket_start_unix_ms - right.bucket_start_unix_ms,
  )
}

function TargetTimeline({ target, points, slots }: TargetTimelineProps) {
  const containerRef = useRef<HTMLDivElement>(null)
  const reconnects = points.reduce(
    (total, point) => total + point.reconnects,
    0,
  )
  const disconnects = points.reduce(
    (total, point) => total + point.disconnects,
    0,
  )

  useEffect(() => {
    const container = containerRef.current
    if (!container) return

    const chart = echarts.init(container, undefined, { renderer: 'canvas' })
    const heatmapData: HistoryDatum[] = points.map((point) => ({
      value: [
        String(point.bucket_start_unix_ms),
        'status',
        STATUS_CODE[point.status],
      ],
      point,
      targetName: target.name,
      venue: target.venue,
    }))
    const eventData: HistoryDatum[] = points
      .filter((point) => point.reconnects > 0 || point.disconnects > 0)
      .map((point) => ({
        value: [
          String(point.bucket_start_unix_ms),
          'status',
          STATUS_CODE[point.status],
        ],
        point,
        targetName: target.name,
        venue: target.venue,
        itemStyle: {
          color: point.disconnects > 0 ? '#c53a42' : '#b7791f',
        },
      }))

    chart.setOption({
      animation: false,
      grid: {
        left: 0,
        right: 12,
        top: 4,
        bottom: 24,
      },
      tooltip: {
        trigger: 'item',
        confine: true,
        formatter: tooltipFormatter,
        backgroundColor: '#ffffff',
        borderColor: '#d9dee6',
        borderWidth: 1,
        padding: [9, 11],
        textStyle: {
          color: '#27313f',
          fontSize: 12,
          lineHeight: 19,
        },
      },
      xAxis: {
        type: 'category',
        data: slots,
        boundaryGap: true,
        axisLine: { lineStyle: { color: '#d7dbe2' } },
        axisTick: { show: false },
        axisLabel: {
          color: '#697386',
          fontSize: 10,
          interval: Math.max(0, Math.floor(slots.length / 6) - 1),
          formatter: (value: string) =>
            axisTimeFormatter.format(Number(value)),
        },
        splitLine: { show: false },
      },
      yAxis: {
        type: 'category',
        data: ['status'],
        axisLine: { show: false },
        axisTick: { show: false },
        axisLabel: { show: false },
        splitLine: { show: false },
      },
      visualMap: {
        type: 'piecewise',
        show: false,
        seriesIndex: 0,
        dimension: 2,
        pieces: [
          { value: 0, color: '#2f806b' },
          { value: 1, color: '#a2aab5' },
          { value: 2, color: '#d39a32' },
          { value: 3, color: '#c53a42' },
        ],
      },
      series: [
        {
          name: '五分钟状态',
          type: 'heatmap',
          data: heatmapData,
          progressive: 1_000,
          itemStyle: {
            borderColor: '#ffffff',
            borderWidth: 0.5,
          },
          emphasis: {
            itemStyle: {
              borderColor: '#27313f',
              borderWidth: 1,
            },
          },
        },
        {
          name: '重连或断开',
          type: 'scatter',
          data: eventData,
          symbol: 'diamond',
          symbolSize: 8,
          z: 4,
        },
      ],
    })

    const observer = new ResizeObserver(() => chart.resize())
    observer.observe(container)
    return () => {
      observer.disconnect()
      chart.dispose()
    }
  }, [points, slots, target.name, target.venue])

  return (
    <div className="market-history-target">
      <div className="market-history-target__heading">
        <div>
          <strong>{target.name}</strong>
          <span>{target.venue}</span>
        </div>
        <small>
          {reconnects > 0 || disconnects > 0
            ? '重连 ' + reconnects + ' · 断开 ' + disconnects
            : '连接稳定'}
        </small>
      </div>
      <div
        ref={containerRef}
        className="market-history-target__chart"
        role="img"
        aria-label={target.name + ' 最近二十四小时五分钟聚合网络状态'}
      />
    </div>
  )
}

export function MarketDataHistoryChart({
  history,
}: MarketDataHistoryChartProps) {
  const view = useMemo(() => {
    const order = new Map(TARGET_ORDER.map((name, index) => [name, index]))
    const targets = [...history.targets].sort(
      (left, right) =>
        (order.get(left.name) ?? TARGET_ORDER.length) -
          (order.get(right.name) ?? TARGET_ORDER.length) ||
        left.name.localeCompare(right.name),
    )
    const bucketMs = DISPLAY_BUCKET_SECS * 1_000
    const slotCount = Math.round(
      (history.retention_hours * 60 * 60) / DISPLAY_BUCKET_SECS,
    )
    const endBucket =
      Math.floor(history.generated_at_unix_ms / bucketMs) * bucketMs
    const startBucket = endBucket - (slotCount - 1) * bucketMs
    const slots = Array.from({ length: slotCount }, (_, index) =>
      String(startBucket + index * bucketMs),
    )
    return {
      slots,
      targets: targets.map((target) => ({
        target,
        points: aggregatePoints(
          target.points.filter(
            (point) =>
              point.bucket_start_unix_ms >= startBucket &&
              point.bucket_start_unix_ms <= endBucket,
          ),
          bucketMs,
        ),
      })),
    }
  }, [history])

  return (
    <div className="market-history-visual">
      <div className="market-history-targets">
        {view.targets.map(({ target, points }) => (
          <TargetTimeline
            key={target.name}
            target={target}
            points={points}
            slots={view.slots}
          />
        ))}
      </div>
      <div className="market-history-legend" aria-label="状态图例">
        <span><i className="is-ok" />正常</span>
        <span><i className="is-unknown" />无数据</span>
        <span><i className="is-warn" />告警</span>
        <span><i className="is-critical" />中断</span>
        <span><i className="is-event" />重连 / 断开</span>
      </div>
    </div>
  )
}
