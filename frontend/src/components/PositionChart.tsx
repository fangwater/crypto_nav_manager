import { LineChart } from 'echarts/charts'
import {
  DataZoomComponent,
  GridComponent,
  TooltipComponent,
} from 'echarts/components'
import * as echarts from 'echarts/core'
import { CanvasRenderer } from 'echarts/renderers'
import { useEffect, useRef } from 'react'
import type {
  PnlPoint,
  PositionSeriesKey,
  SymbolPnlSeries,
} from '../types'

echarts.use([
  LineChart,
  DataZoomComponent,
  GridComponent,
  TooltipComponent,
  CanvasRenderer,
])

interface PositionChartProps {
  points: PnlPoint[]
  symbolPoints: SymbolPnlSeries[]
  visibleSeries: PositionSeriesKey[]
  mode: 'portfolio' | 'symbols'
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
  PositionSeriesKey,
  { label: string; color: string; lineType: 'solid' | 'dashed' | 'dotted' }
> = {
  spotPositionUsdt: {
    label: 'Spot 仓位',
    color: '#2563a7',
    lineType: 'solid',
  },
  futuresPositionUsdt: {
    label: 'Futures 仓位',
    color: '#b7791f',
    lineType: 'dashed',
  },
  exposureUsdt: {
    label: '敞口',
    color: '#c2413b',
    lineType: 'solid',
  },
}

function amount(value: number) {
  return value.toLocaleString('en-US', {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  })
}

export function PositionChart({
  points,
  symbolPoints,
  visibleSeries,
  mode,
}: PositionChartProps) {
  const containerRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!containerRef.current) return

    const chart = echarts.init(containerRef.current, undefined, {
      renderer: 'canvas',
    })
    const series =
      mode === 'portfolio'
        ? visibleSeries.map((key) => {
            const meta = seriesMeta[key]
            return {
              name: meta.label,
              type: 'line' as const,
              data: points.map((point) => [point.ts, point[key]]),
              showSymbol: false,
              sampling: 'lttb' as const,
              connectNulls: true,
              lineStyle: {
                width: key === 'exposureUsdt' ? 2.2 : 1.7,
                color: meta.color,
                type: meta.lineType,
              },
              itemStyle: { color: meta.color },
              emphasis: { focus: 'series' as const },
            }
          })
        : symbolPoints.flatMap((item, symbolIndex) =>
            visibleSeries.map((key) => {
              const meta = seriesMeta[key]
              const color = symbolPalette[symbolIndex % symbolPalette.length]
              return {
                name: `${item.symbol} ${meta.label}`,
                type: 'line' as const,
                data: item.points.map((point) => [point.ts, point[key]]),
                showSymbol: false,
                sampling: 'lttb' as const,
                connectNulls: true,
                lineStyle: {
                  width: key === 'exposureUsdt' ? 2 : 1.35,
                  color,
                  type: meta.lineType,
                  opacity: key === 'exposureUsdt' ? 1 : 0.72,
                },
                itemStyle: { color },
                emphasis: { focus: 'series' as const },
              }
            }),
          )

    chart.setOption(
      {
        animation: false,
        grid: {
          left: 76,
          right: 24,
          top: 16,
          bottom: 74,
        },
        tooltip: {
          trigger: 'axis',
          confine: true,
          backgroundColor: 'rgba(255,255,255,0.97)',
          borderColor: '#d7dbe2',
          textStyle: { color: '#20252d', fontSize: 12 },
          valueFormatter: (value: unknown) =>
            `${amount(typeof value === 'number' ? value : Number(value))} U`,
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
          axisLabel: { color: '#697386', hideOverlap: true },
          splitLine: { show: false },
        },
        yAxis: {
          type: 'value',
          scale: true,
          name: 'U',
          nameTextStyle: { color: '#697386', fontSize: 10 },
          axisLine: { show: false },
          axisTick: { show: false },
          axisLabel: {
            color: '#697386',
            formatter: (value: number) => amount(value),
          },
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
            fillerColor: 'rgba(37, 99, 167, 0.12)',
            handleStyle: { color: '#ffffff', borderColor: '#2563a7' },
            moveHandleStyle: { color: '#8aa7c7' },
            dataBackground: {
              lineStyle: { color: '#9aa4b2' },
              areaStyle: { color: '#dfe3e8' },
            },
            selectedDataBackground: {
              lineStyle: { color: '#2563a7' },
              areaStyle: { color: '#bdd0e3' },
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
  }, [mode, points, symbolPoints, visibleSeries])

  return (
    <div
      ref={containerRef}
      className="pnl-chart position-chart"
      aria-label={
        mode === 'portfolio'
          ? '组合仓位与敞口时间曲线'
          : '分币仓位与敞口时间曲线'
      }
    />
  )
}
