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
import { intraAnalysisMetricLabel } from '../intraAnalysisSeries'
import type {
  IntraAnalysisPoint,
  IntraAnalysisSeriesKey,
  IntraSymbolSeries,
} from '../types'

echarts.use([
  LineChart,
  DataZoomComponent,
  GridComponent,
  LegendComponent,
  TooltipComponent,
  CanvasRenderer,
])

export type IntraFifoChartMode = 'portfolio' | 'symbol'

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

interface IntraFifoChartProps {
  points: IntraAnalysisPoint[]
  symbolPoints: IntraSymbolSeries[]
  symbolColors: Record<string, string>
  mode: IntraFifoChartMode
  metric: IntraAnalysisSeriesKey
}

export function IntraFifoChart({
  points,
  symbolPoints,
  symbolColors,
  mode,
  metric,
}: IntraFifoChartProps) {
  const containerRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!containerRef.current) return

    const chart = echarts.init(containerRef.current, undefined, {
      renderer: 'canvas',
    })
    const isSymbolMode = mode === 'symbol'
    const series = isSymbolMode
      ? symbolPoints.map((symbolSeries) => {
          const color = symbolColors[symbolSeries.symbol] ?? '#176b5b'
          return {
            name: symbolSeries.symbol,
            type: 'line' as const,
            data: symbolSeries.points.map((point) => [point.ts, point[metric]]),
            showSymbol: false,
            sampling: 'lttb' as const,
            connectNulls: true,
            lineStyle: { width: 1.55, color },
            itemStyle: { color },
            emphasis: { focus: 'series' as const, lineStyle: { width: 2.6 } },
          }
        })
      : [
          {
            name: 'Fee 前收益',
            type: 'line' as const,
            data: points.map((point) => [point.ts, point.realizedPnlUsdt]),
            showSymbol: false,
            sampling: 'lttb' as const,
            connectNulls: true,
            lineStyle: { width: 2.2, color: '#176b5b' },
            itemStyle: { color: '#176b5b' },
            areaStyle: { color: 'rgba(23, 107, 91, 0.06)', origin: 0 },
            emphasis: { focus: 'series' as const },
          },
          {
            name: '累计实际 Fee 影响',
            type: 'line' as const,
            data: points.map((point) => [point.ts, -point.tradingFeeUsdt]),
            showSymbol: false,
            sampling: 'lttb' as const,
            connectNulls: true,
            lineStyle: { width: 1.45, color: '#b5473c', type: 'dashed' as const },
            itemStyle: { color: '#b5473c' },
            emphasis: { focus: 'series' as const },
          },
          {
            name: '实际 Fee 后收益',
            type: 'line' as const,
            data: points.map((point) => [point.ts, point.feeAfterPnlUsdt]),
            showSymbol: false,
            sampling: 'lttb' as const,
            connectNulls: true,
            lineStyle: { width: 2, color: '#087f8c' },
            itemStyle: { color: '#087f8c' },
            emphasis: { focus: 'series' as const },
          },
          {
            name: '参考 Fee 后收益',
            type: 'line' as const,
            data: points.map((point) => [point.ts, point.referenceFeeAfterPnlUsdt]),
            showSymbol: false,
            sampling: 'lttb' as const,
            connectNulls: true,
            lineStyle: { width: 1.65, color: '#6c6f2d', type: 'dotted' as const },
            itemStyle: { color: '#6c6f2d' },
            emphasis: { focus: 'series' as const },
          },
          {
            name: '选币基差',
            type: 'line' as const,
            data: points.map((point) => [point.ts, point.marketPnlUsdt]),
            showSymbol: false,
            sampling: 'lttb' as const,
            connectNulls: true,
            lineStyle: { width: 1.6, color: '#2563a7' },
            itemStyle: { color: '#2563a7' },
            emphasis: { focus: 'series' as const },
          },
          {
            name: '闭环执行',
            type: 'line' as const,
            data: points.map((point) => [point.ts, point.executionPnlUsdt]),
            showSymbol: false,
            sampling: 'lttb' as const,
            connectNulls: true,
            lineStyle: { width: 1.6, color: '#b45309' },
            itemStyle: { color: '#b45309' },
            emphasis: { focus: 'series' as const },
          },
          {
            name: '窗口 MT 执行',
            type: 'line' as const,
            data: points.map((point) => [point.ts, point.executionCaptureUsdt]),
            showSymbol: false,
            sampling: 'lttb' as const,
            connectNulls: true,
            lineStyle: { width: 1.5, color: '#7c3a91', type: 'dashed' as const },
            itemStyle: { color: '#7c3a91' },
            emphasis: { focus: 'series' as const },
          },
        ]

    chart.setOption(
      {
        animation: false,
        grid: { left: 72, right: 24, top: isSymbolMode ? 28 : 58, bottom: 74 },
        legend: {
          show: !isSymbolMode,
          type: 'scroll',
          top: 18,
          left: 72,
          right: 24,
          itemWidth: 20,
          itemHeight: 3,
          textStyle: { color: '#596273', fontSize: 10 },
          data: [
            'Fee 前收益',
            '累计实际 Fee 影响',
            '实际 Fee 后收益',
            '参考 Fee 后收益',
            '选币基差',
            '闭环执行',
            '窗口 MT 执行',
          ],
        },
        tooltip: {
          trigger: 'axis',
          confine: true,
          order: 'valueDesc',
          backgroundColor: 'rgba(255,255,255,0.97)',
          borderColor: '#d7dbe2',
          textStyle: { color: '#20252d', fontSize: 12 },
          extraCssText: 'max-height: 280px; overflow-y: auto;',
          valueFormatter: (value: unknown) =>
            `${money(typeof value === 'number' ? value : Number(value))} USDT`,
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
        yAxis: {
          type: 'value',
          scale: true,
          name: 'USDT',
          nameTextStyle: { color: '#697386', fontSize: 10 },
          axisLine: { show: false },
          axisTick: { show: false },
          axisLabel: { color: '#697386', formatter: money },
          splitLine: { lineStyle: { color: '#edf0f4' } },
        },
        dataZoom: [
          { type: 'inside', filterMode: 'none' },
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
  }, [metric, mode, points, symbolColors, symbolPoints])

  return (
    <div
      ref={containerRef}
      className="analysis-chart"
      aria-label={
        mode === 'portfolio'
          ? '正反套 FIFO Fee 前、实际 Fee 影响、实际 Fee 后、参考 Fee 后与执行累计曲线'
          : `各币 ${intraAnalysisMetricLabel(metric)}累计曲线`
      }
    />
  )
}
