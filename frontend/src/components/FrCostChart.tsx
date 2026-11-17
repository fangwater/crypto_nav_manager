import { BarChart, LineChart } from 'echarts/charts'
import { DataZoomComponent, GridComponent, LegendComponent, TooltipComponent } from 'echarts/components'
import * as echarts from 'echarts/core'
import { CanvasRenderer } from 'echarts/renderers'
import { useEffect, useRef } from 'react'
import type { FrEntryCost } from '../frEntryCost'
import { costBps } from '../frEntryCost'

echarts.use([BarChart, LineChart, DataZoomComponent, GridComponent, LegendComponent, TooltipComponent, CanvasRenderer])

export function FrCostChart({ minutes }: { minutes: FrEntryCost['report']['minutes'] }) {
  const ref = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (!ref.current) return
    const chart = echarts.init(ref.current)
    chart.setOption({
      color: ['#157f78', '#e99b3e', '#6374bb'],
      tooltip: { trigger: 'axis', valueFormatter: (value: number | null) => value === null ? '未知' : value.toFixed(4) },
      legend: { top: 4, data: ['建仓成本估算 USDT', '成交基差 bps', '参考基差 bps'] },
      grid: { left: 70, right: 65, top: 50, bottom: 80 },
      xAxis: { type: 'category', data: minutes.map(p => new Date(p.ts).toLocaleString('zh-CN', { hour12: false })) },
      yAxis: [{ type: 'value', name: 'USDT' }, { type: 'value', name: 'bps', splitLine: { show: false } }],
      dataZoom: [{ type: 'inside' }, { type: 'slider', bottom: 12 }],
      series: [
        { type: 'bar', name: '建仓成本估算 USDT', data: minutes.map(p => p.estimatedCostUsdt), barMaxWidth: 20 },
        { type: 'line', name: '成交基差 bps', yAxisIndex: 1, showSymbol: false, connectNulls: false,
          data: minutes.map(p => costBps(p.entrySpreadUsdt, p.notionalUsdt)) },
        { type: 'line', name: '参考基差 bps', yAxisIndex: 1, showSymbol: false, connectNulls: false,
          data: minutes.map(p => costBps(p.referenceSpreadUsdt, p.notionalUsdt)) },
      ],
    })
    const observer = new ResizeObserver(() => chart.resize())
    observer.observe(ref.current)
    return () => { observer.disconnect(); chart.dispose() }
  }, [minutes])
  return <div ref={ref} className="fr-cost-chart" role="img" aria-label="当前剩余仓位的分钟建仓成本与参考基差图" />
}

export function FrAlignmentChart({ points }: { points: FrEntryCost['alignment'] }) {
  const ref = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (!ref.current) return
    const chart = echarts.init(ref.current)
    chart.setOption({
      color: ['#b47435'],
      tooltip: { trigger: 'axis' },
      grid: { left: 85, right: 30, top: 25, bottom: 60 },
      xAxis: { type: 'time' },
      yAxis: { type: 'value', name: '净 base 数量', scale: true },
      dataZoom: [{ type: 'inside' }, { type: 'slider', bottom: 0 }],
      series: [{ name: '分钟末未对齐数量', type: 'line', step: 'end', showSymbol: false, data: points.map(p => [p.ts, p.netQuantity]) }],
    })
    const observer = new ResizeObserver(() => chart.resize())
    observer.observe(ref.current)
    return () => { observer.disconnect(); chart.dispose() }
  }, [points])
  return <div ref={ref} className="fr-cost-alignment-chart" role="img" aria-label="最近 24 小时按成交重建的分钟末净敞口" />
}
