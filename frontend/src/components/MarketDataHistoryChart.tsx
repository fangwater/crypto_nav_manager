import { HeatmapChart, ScatterChart } from 'echarts/charts'
import {
  GridComponent,
  TooltipComponent,
  VisualMapComponent,
} from 'echarts/components'
import * as echarts from 'echarts/core'
import { CanvasRenderer } from 'echarts/renderers'
import { useEffect, useRef } from 'react'
import type {
  MarketDataHealth,
  MarketDataHistory,
  MarketDataHistoryPoint,
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
  venue: string
  itemStyle?: {
    color: string
  }
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
    '<strong>' + escapeHtml(datum.value[1]) + '</strong>',
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

export function MarketDataHistoryChart({
  history,
}: MarketDataHistoryChartProps) {
  const containerRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const container = containerRef.current
    if (!container) return

    const chart = echarts.init(container, undefined, { renderer: 'canvas' })
    const order = new Map(TARGET_ORDER.map((name, index) => [name, index]))
    const targets = [...history.targets].sort(
      (left, right) =>
        (order.get(left.name) ?? TARGET_ORDER.length) -
          (order.get(right.name) ?? TARGET_ORDER.length) ||
        left.name.localeCompare(right.name),
    )
    const bucketMs = Math.max(1, history.bucket_secs) * 1_000
    const slotCount = Math.max(
      1,
      Math.min(
        1_440,
        Math.round(
          (history.retention_hours * 60 * 60) / history.bucket_secs,
        ),
      ),
    )
    const endBucket =
      Math.floor(history.generated_at_unix_ms / bucketMs) * bucketMs
    const startBucket = endBucket - (slotCount - 1) * bucketMs
    const slots = Array.from({ length: slotCount }, (_, index) =>
      String(startBucket + index * bucketMs),
    )
    const names = targets.map((target) => target.name)
    const heatmapData: HistoryDatum[] = []
    const eventData: HistoryDatum[] = []

    for (const target of targets) {
      for (const point of target.points) {
        if (
          point.bucket_start_unix_ms < startBucket ||
          point.bucket_start_unix_ms > endBucket
        ) {
          continue
        }
        const datum: HistoryDatum = {
          value: [
            String(point.bucket_start_unix_ms),
            target.name,
            STATUS_CODE[point.status],
          ],
          point,
          venue: target.venue,
        }
        heatmapData.push(datum)
        if (point.reconnects > 0 || point.disconnects > 0) {
          eventData.push({
            ...datum,
            itemStyle: {
              color: point.disconnects > 0 ? '#c53a42' : '#b7791f',
            },
          })
        }
      }
    }

    chart.setOption({
      animation: false,
      grid: {
        left: 112,
        right: 18,
        top: 16,
        bottom: 42,
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
          fontSize: 11,
          interval: Math.max(0, Math.floor(slotCount / 6) - 1),
          formatter: (value: string) =>
            axisTimeFormatter.format(Number(value)),
        },
        splitLine: { show: false },
      },
      yAxis: {
        type: 'category',
        data: names,
        inverse: true,
        axisLine: { show: false },
        axisTick: { show: false },
        axisLabel: {
          color: '#343d49',
          fontSize: 12,
          fontFamily: 'ui-monospace, SFMono-Regular, monospace',
          margin: 14,
        },
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
          name: '分钟状态',
          type: 'heatmap',
          data: heatmapData,
          progressive: 2_000,
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
          symbolSize: 9,
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
  }, [history])

  return (
    <div className="market-history-visual">
      <div
        ref={containerRef}
        className="market-history-chart"
        role="img"
        aria-label="五个行情进程最近二十四小时的每分钟网络状态"
      />
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
