import { LineChart } from 'echarts/charts'
import {
  DataZoomComponent,
  GridComponent,
  TooltipComponent,
} from 'echarts/components'
import * as echarts from 'echarts/core'
import { CanvasRenderer } from 'echarts/renderers'
import { useEffect, useMemo, useRef } from 'react'
import {
  bindHourlyLatencyChart,
  visibleLatencyChartModel,
  type HourlyLatencySeries,
} from '../analysisLatencyChart'

echarts.use([
  LineChart,
  DataZoomComponent,
  GridComponent,
  TooltipComponent,
  CanvasRenderer,
])

function chartTime(value: number) {
  const date = new Date(value)
  const month = String(date.getMonth() + 1).padStart(2, '0')
  const day = String(date.getDate()).padStart(2, '0')
  const hour = String(date.getHours()).padStart(2, '0')
  return `${month}-${day}\n${hour}:00`
}

export function IntraLatencyChart({
  series,
  selectedKeys,
}: {
  series: HourlyLatencySeries | null
  selectedKeys: readonly string[]
}) {
  const containerRef = useRef<HTMLDivElement>(null)
  const model = useMemo(
    () => visibleLatencyChartModel(bindHourlyLatencyChart(series), selectedKeys),
    [series, selectedKeys],
  )

  useEffect(() => {
    if (!containerRef.current) return
    const chart = echarts.init(containerRef.current, undefined, {
      renderer: 'canvas',
    })
    chart.setOption({
      animation: false,
      grid: { top: 18, right: 18, bottom: 48, left: 52 },
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
        lineStyle: {
          width: 1.6,
          color: line.color,
          type: line.dashed ? 'dashed' : 'solid',
        },
        itemStyle: { color: line.color },
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
      data-selected-count={model.series.length}
    />
  )
}
