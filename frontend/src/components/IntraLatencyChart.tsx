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
import {
  bindHourlyLatencyChart,
  type HourlyLatencySeries,
} from '../analysisLatencyChart'

echarts.use([
  LineChart,
  DataZoomComponent,
  GridComponent,
  LegendComponent,
  TooltipComponent,
  CanvasRenderer,
])

const seriesColors = [
  '#176b5b',
  '#7aa89d',
  '#2563a7',
  '#7ea2c8',
  '#b45309',
  '#d4a373',
  '#7c3a91',
  '#b48bc9',
]

function chartTime(value: number) {
  const date = new Date(value)
  const month = String(date.getMonth() + 1).padStart(2, '0')
  const day = String(date.getDate()).padStart(2, '0')
  const hour = String(date.getHours()).padStart(2, '0')
  return `${month}-${day}\n${hour}:00`
}

export function IntraLatencyChart({
  series,
}: {
  series: HourlyLatencySeries | null
}) {
  const containerRef = useRef<HTMLDivElement>(null)
  const model = bindHourlyLatencyChart(series)

  useEffect(() => {
    if (!containerRef.current) return
    const chart = echarts.init(containerRef.current, undefined, {
      renderer: 'canvas',
    })
    chart.setOption({
      animation: false,
      color: seriesColors,
      grid: { top: 56, right: 18, bottom: 48, left: 52 },
      legend: {
        top: 0,
        left: 0,
        itemWidth: 10,
        itemHeight: 6,
        textStyle: { fontSize: 10, color: '#5b6570' },
      },
      tooltip: {
        trigger: 'axis',
        valueFormatter: (value: number | string) =>
          value === '' || value == null
            ? '--'
            : `${Number(value).toFixed(3)} ms`,
      },
      dataZoom: [{ type: 'inside', xAxisIndex: 0 }],
      xAxis: {
        type: 'category',
        data: model.categoryTimes.map(chartTime),
        axisLabel: { fontSize: 10, color: '#7a838d' },
      },
      yAxis: {
        type: 'value',
        name: 'ms',
        axisLabel: { fontSize: 10, color: '#7a838d' },
        splitLine: { lineStyle: { color: '#eceff2' } },
      },
      series: model.series.map((line) => ({
        name: line.name,
        type: 'line',
        connectNulls: true,
        showSymbol: model.categoryTimes.length <= 24,
        data: line.values,
      })),
    })
    const onResize = () => chart.resize()
    window.addEventListener('resize', onResize)
    return () => {
      window.removeEventListener('resize', onResize)
      chart.dispose()
    }
  }, [model])

  return (
    <div
      ref={containerRef}
      className="analysis-chart analysis-latency-chart"
      data-chart-id={model.chartId}
      data-distinct-from={model.distinctFrom}
    />
  )
}
