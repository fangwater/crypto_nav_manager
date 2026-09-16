import { LineChart } from 'echarts/charts'
import {
  DataZoomComponent,
  GridComponent,
  LegendComponent,
  TooltipComponent,
} from 'echarts/components'
import * as echarts from 'echarts/core'
import { CanvasRenderer } from 'echarts/renderers'
import { useEffect, useRef } from 'react'
import type { PnlPoint, PnlSeriesKey, SymbolPnlSeries } from '../types'

echarts.use([
  LineChart,
  DataZoomComponent,
  GridComponent,
  LegendComponent,
  TooltipComponent,
  CanvasRenderer,
])

interface PnlChartProps {
  points: PnlPoint[]
  symbolPoints: SymbolPnlSeries[]
  visibleSeries: PnlSeriesKey[]
  mode: 'portfolio' | 'symbols'
  principalUsdt?: number | null
}

const symbolPalette = [
  '#176b5b',
  '#2563a7',
  '#b7791f',
  '#c2413b',
  '#7357a3',
  '#2f855a',
  '#9c4f87',
  '#4b6478',
  '#d97706',
  '#0f766e',
  '#4467a8',
  '#a33f55',
]

const seriesMeta: Record<
  PnlSeriesKey,
  {
    label: string
    color: string
    dashed?: boolean
    negate?: boolean
    percent?: boolean
    area?: boolean
  }
> = {
  totalPnlUsdt: { label: 'Total PnL', color: '#176b5b' },
  feeBeforePnlUsdt: { label: 'Fee 前净值', color: '#2563a7' },
  feeAfterPnlUsdt: { label: 'Fee 后净值', color: '#7357a3' },
  fundingPnlUsdt: { label: 'Funding', color: '#b7791f' },
  interestCostUsdt: {
    label: 'Interest 成本',
    color: '#c2413b',
    dashed: true,
    negate: true,
  },
  floatingPnlUsdt: {
    label: '浮动盈亏',
    color: '#4b6478',
    dashed: true,
  },
  returnOnPrincipalPct: {
    label: '本金收益率',
    color: '#0e7490',
    percent: true,
  },
  drawdownPct: {
    label: '净值回撤',
    color: '#b5473c',
    percent: true,
    area: true,
  },
}

const percentSeriesLabels = new Set(
  (Object.keys(seriesMeta) as PnlSeriesKey[])
    .filter((key) => seriesMeta[key].percent)
    .map((key) => seriesMeta[key].label),
)

function isPercentSeries(key: PnlSeriesKey) {
  return seriesMeta[key].percent === true
}

type PortfolioPnlKey = Exclude<
  PnlSeriesKey,
  'returnOnPrincipalPct' | 'drawdownPct'
>

function portfolioSeriesValue(point: PnlPoint, key: PortfolioPnlKey) {
  if (key === 'feeBeforePnlUsdt' || key === 'feeAfterPnlUsdt') {
    return point[key] + point.floatingPnlUsdt
  }
  return point[key]
}

function money(value: number) {
  return value.toLocaleString('en-US', {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  })
}

function chartTime(value: number) {
  const date = new Date(value)
  const month = String(date.getMonth() + 1).padStart(2, '0')
  const day = String(date.getDate()).padStart(2, '0')
  const hour = String(date.getHours()).padStart(2, '0')
  const minute = String(date.getMinutes()).padStart(2, '0')
  return `${month}-${day}\n${hour}:${minute}`
}

export function PnlChart({
  points,
  symbolPoints,
  visibleSeries,
  mode,
  principalUsdt,
}: PnlChartProps) {
  const containerRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!containerRef.current) return

    const chart = echarts.init(containerRef.current, undefined, {
      renderer: 'canvas',
    })
    const principal =
      typeof principalUsdt === 'number' &&
      Number.isFinite(principalUsdt) &&
      principalUsdt > 0
        ? principalUsdt
        : null
    const endTotalPnl = points.length
      ? points[points.length - 1].totalPnlUsdt
      : 0
    let peakPnl = Number.NEGATIVE_INFINITY
    const derived = points.map((point) => {
      peakPnl = Math.max(peakPnl, point.totalPnlUsdt)
      const peakNav =
        principal !== null ? principal + peakPnl - endTotalPnl : 0
      return {
        returnOnPrincipalPct:
          principal !== null ? (point.totalPnlUsdt / principal) * 100 : null,
        drawdownPct:
          principal !== null && peakNav > 0
            ? (-(peakPnl - point.totalPnlUsdt) / peakNav) * 100
            : null,
      }
    })
    const activeSeries =
      principal !== null
        ? visibleSeries
        : visibleSeries.filter((key) => !isPercentSeries(key))
    const hasPercentAxis =
      mode === 'portfolio' && activeSeries.some(isPercentSeries)
    const series =
      mode === 'portfolio'
        ? activeSeries.map((key) => {
            const meta = seriesMeta[key]
            return {
              name: meta.label,
              type: 'line' as const,
              yAxisIndex: meta.percent ? 1 : 0,
              data: meta.percent
                ? points.map((point, index) => [
                    point.ts,
                    key === 'returnOnPrincipalPct'
                      ? derived[index].returnOnPrincipalPct
                      : derived[index].drawdownPct,
                  ])
                : points.map((point) => [
                    point.ts,
                    (meta.negate ? -1 : 1) *
                      portfolioSeriesValue(point, key as PortfolioPnlKey),
                  ]),
              showSymbol: false,
              sampling: 'lttb' as const,
              connectNulls: true,
              lineStyle: {
                width: key === 'totalPnlUsdt' ? 2.4 : 1.6,
                color: meta.color,
                type: meta.dashed ? ('dashed' as const) : ('solid' as const),
              },
              itemStyle: { color: meta.color },
              areaStyle: meta.area
                ? { opacity: 0.16, color: meta.color }
                : undefined,
              emphasis: { focus: 'series' as const },
            }
          })
        : symbolPoints.map((item, index) => {
            const color = symbolPalette[index % symbolPalette.length]
            return {
              name: item.symbol,
              type: 'line' as const,
              data: item.points.map((point) => [point.ts, point.totalPnlUsdt]),
              showSymbol: false,
              sampling: 'lttb' as const,
              connectNulls: true,
              lineStyle: { width: 1.7, color },
              itemStyle: { color },
              emphasis: { focus: 'series' as const },
            }
          })

    chart.setOption(
      {
        animation: false,
        color:
          mode === 'portfolio'
            ? activeSeries.map((key) => seriesMeta[key].color)
            : symbolPalette,
        grid: {
          left: 70,
          right: hasPercentAxis ? 62 : 24,
          top: mode === 'portfolio' ? 28 : 16,
          bottom: 74,
        },
        legend: {
          show: false,
          type: 'scroll',
          top: 0,
          right: 20,
          textStyle: { color: '#596273', fontSize: 11 },
          itemWidth: 18,
          itemHeight: 3,
        },
        tooltip: {
          trigger: 'axis',
          confine: true,
          backgroundColor: 'rgba(255,255,255,0.97)',
          borderColor: '#d7dbe2',
          textStyle: { color: '#20252d', fontSize: 12 },
          formatter: (params: unknown) => {
            const items = (
              Array.isArray(params) ? params : [params]
            ) as Array<{
              axisValue?: number
              marker?: string
              seriesName?: string
              value?: [number, number | null]
            }>
            const title =
              items[0]?.axisValue != null
                ? chartTime(items[0].axisValue).replace('\n', ' ')
                : ''
            const lines = items.map((item) => {
              const value = item.value?.[1]
              const text =
                value == null || !Number.isFinite(value)
                  ? '--'
                  : item.seriesName && percentSeriesLabels.has(item.seriesName)
                    ? `${value.toFixed(2)}%`
                    : money(value)
              return `${item.marker ?? ''}${item.seriesName ?? ''}: ${text}`
            })
            return [title, ...lines].join('<br/>')
          },
          axisPointer: {
            type: 'line',
            lineStyle: { color: '#8993a4', type: 'dashed' },
          },
        },
        xAxis: {
          type: 'time',
          boundaryGap: false,
          axisLine: { lineStyle: { color: '#d7dbe2' } },
          axisTick: { show: false },
          axisLabel: {
            color: '#697386',
            hideOverlap: true,
            formatter: (value: number) => chartTime(value),
          },
          splitLine: { show: false },
        },
        yAxis: [
          {
            type: 'value',
            scale: true,
            axisLine: { show: false },
            axisTick: { show: false },
            axisLabel: {
              color: '#697386',
              formatter: (value: number) => money(value),
            },
            splitLine: { lineStyle: { color: '#edf0f4' } },
          },
          {
            type: 'value',
            scale: true,
            position: 'right',
            show: hasPercentAxis,
            axisLine: { show: false },
            axisTick: { show: false },
            axisLabel: {
              color: '#697386',
              formatter: (value: number) => `${value.toFixed(1)}%`,
            },
            splitLine: { show: false },
          },
        ],
        dataZoom: [
          {
            type: 'inside',
            filterMode: 'none',
          },
          {
            type: 'slider',
            height: 24,
            bottom: 20,
            borderColor: '#dfe3e8',
            backgroundColor: '#f5f6f8',
            fillerColor: 'rgba(31, 122, 104, 0.12)',
            handleStyle: { color: '#ffffff', borderColor: '#1f7a68' },
            moveHandleStyle: { color: '#8ab7ad' },
            dataBackground: {
              lineStyle: { color: '#9aa4b2' },
              areaStyle: { color: '#dfe3e8' },
            },
            selectedDataBackground: {
              lineStyle: { color: '#1f7a68' },
              areaStyle: { color: '#b9d9d1' },
            },
            textStyle: { color: '#697386', fontSize: 10 },
          },
        ],
        series,
      },
      true,
    )

    const observer = new ResizeObserver(() => chart.resize())
    observer.observe(containerRef.current)

    return () => {
      observer.disconnect()
      chart.dispose()
    }
  }, [mode, points, symbolPoints, visibleSeries, principalUsdt])

  return (
    <div
      ref={containerRef}
      className="pnl-chart"
      aria-label={mode === 'portfolio' ? '组合 PnL 时间曲线' : '分币 PnL 时间曲线'}
    />
  )
}
