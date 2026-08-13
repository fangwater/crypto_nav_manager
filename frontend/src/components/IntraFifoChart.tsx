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
import { intraFeeModeConfig } from '../intraAnalysisSeries'
import type {
  IntraFeeMode,
  IntraAnalysisPoint,
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
  feeMode: IntraFeeMode
}

export function IntraFifoChart({
  points,
  symbolPoints,
  symbolColors,
  mode,
  feeMode,
}: IntraFifoChartProps) {
  const containerRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!containerRef.current) return

    const chart = echarts.init(containerRef.current, undefined, {
      renderer: 'canvas',
    })
    const isSymbolMode = mode === 'symbol'
    const fee = intraFeeModeConfig(feeMode)
    const feeImpact =
      feeMode === 'gross'
        ? null
        : {
            name: feeMode === 'actual' ? '实际 Fee 影响' : '参考 Fee 影响',
            values: points.map((point) =>
              feeMode === 'actual'
                ? -point.tradingFeeUsdt
                : -point.referenceTradingFeeUsdt,
            ),
          }
    const series = isSymbolMode
      ? symbolPoints.map((symbolSeries) => {
          const color = symbolColors[symbolSeries.symbol] ?? '#176b5b'
          return {
            name: symbolSeries.symbol,
            type: 'line' as const,
            data: symbolSeries.points.map((point) => [point.ts, point[fee.metric]]),
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
            name: fee.label,
            type: 'line' as const,
            data: points.map((point) => [point.ts, point[fee.metric]]),
            showSymbol: false,
            sampling: 'lttb' as const,
            connectNulls: true,
            lineStyle: { width: 2.2, color: fee.color },
            itemStyle: { color: fee.color },
            areaStyle: { color: `${fee.color}12`, origin: 0 },
            emphasis: { focus: 'series' as const },
          },
          ...(feeImpact
            ? [
                {
                  name: feeImpact.name,
                  type: 'line' as const,
                  data: points.map((point, index) => [point.ts, feeImpact.values[index]]),
                  showSymbol: false,
                  sampling: 'lttb' as const,
                  connectNulls: true,
                  lineStyle: {
                    width: 1.45,
                    color: '#b5473c',
                    type: 'dashed' as const,
                  },
                  itemStyle: { color: '#b5473c' },
                  emphasis: { focus: 'series' as const },
                },
              ]
            : []),
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
            fee.label,
            ...(feeImpact ? [feeImpact.name] : []),
            '选币基差',
            '闭环执行',
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
  }, [feeMode, mode, points, symbolColors, symbolPoints])

  return (
    <div
      ref={containerRef}
      className="analysis-chart"
      aria-label={
        mode === 'portfolio'
          ? `正反套 FIFO ${intraFeeModeConfig(feeMode).label}及闭环根因累计曲线`
          : `各币 ${intraFeeModeConfig(feeMode).label}累计曲线`
      }
    />
  )
}
