import { LineChart } from 'echarts/charts'
import { GraphicComponent, GridComponent, TooltipComponent } from 'echarts/components'
import * as echarts from 'echarts/core'
import { CanvasRenderer } from 'echarts/renderers'
import { useEffect, useRef } from 'react'

echarts.use([
  LineChart, GridComponent, TooltipComponent, GraphicComponent, CanvasRenderer,
])

interface NavChartProps {
  name: string
}

export function NavChart({ name }: NavChartProps) {
  const containerRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!containerRef.current) return

    const chart = echarts.init(containerRef.current, undefined, {
      renderer: 'canvas',
    })

    chart.setOption({
      animation: false,
      grid: {
        left: 58,
        right: 24,
        top: 34,
        bottom: 42,
      },
      tooltip: {
        trigger: 'axis',
        axisPointer: {
          type: 'line',
          lineStyle: { color: '#8993a4', type: 'dashed' },
        },
      },
      xAxis: {
        type: 'time',
        axisLine: { lineStyle: { color: '#d7dbe2' } },
        axisTick: { show: false },
        axisLabel: { color: '#697386', hideOverlap: true },
        splitLine: { show: false },
      },
      yAxis: {
        type: 'value',
        scale: true,
        axisLine: { show: false },
        axisTick: { show: false },
        axisLabel: { color: '#697386' },
        splitLine: { lineStyle: { color: '#edf0f4' } },
      },
      series: [
        {
          name: '净值',
          type: 'line',
          data: [],
          showSymbol: false,
          smooth: false,
          lineStyle: { width: 2, color: '#1f7a68' },
          areaStyle: { color: 'rgba(31, 122, 104, 0.08)' },
        },
      ],
      graphic: [
        {
          type: 'group',
          left: 'center',
          top: 'middle',
          children: [
            {
              type: 'circle',
              shape: { cx: 18, cy: 18, r: 18 },
              style: {
                fill: '#f3f5f7',
                stroke: '#dfe3e8',
                lineWidth: 1,
              },
            },
            {
              type: 'polyline',
              shape: {
                points: [
                  [8, 22],
                  [14, 16],
                  [20, 19],
                  [28, 10],
                ],
              },
              style: {
                stroke: '#8993a4',
                lineWidth: 1.5,
                fill: 'none',
              },
            },
            {
              type: 'text',
              x: 50,
              y: 7,
              style: {
                text: '暂无净值数据',
                fill: '#2f3743',
                font: '600 14px system-ui, sans-serif',
              },
            },
            {
              type: 'text',
              x: 50,
              y: 29,
              style: {
                text: name,
                fill: '#8993a4',
                font: '12px ui-monospace, monospace',
              },
            },
          ],
        },
      ],
    })

    const observer = new ResizeObserver(() => chart.resize())
    observer.observe(containerRef.current)

    return () => {
      observer.disconnect()
      chart.dispose()
    }
  }, [name])

  return (
    <div
      ref={containerRef}
      className="nav-chart"
      aria-label={name + ' 净值图'}
    />
  )
}

