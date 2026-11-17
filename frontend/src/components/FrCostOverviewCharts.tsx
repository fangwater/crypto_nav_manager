import { BarChart } from 'echarts/charts'
import { GraphicComponent, GridComponent, LegendComponent, TooltipComponent } from 'echarts/components'
import * as echarts from 'echarts/core'
import { CanvasRenderer } from 'echarts/renderers'
import { useEffect, useRef } from 'react'
import type { FrCostRow } from '../frEntryCost'
import { costMetric } from '../frCostScreen'

echarts.use([BarChart, GraphicComponent, GridComponent, LegendComponent, TooltipComponent, CanvasRenderer])
const fmt = (n: number | null) => n === null ? '未知' : n.toLocaleString('en-US', { maximumFractionDigits: 2 })

export function FrCostOverviewCharts({ rows, onSelect }: {
  rows: FrCostRow[]; onSelect: (symbol: string) => void
}) {
  const bars = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (!bars.current) return
    const barChart = echarts.init(bars.current)
    const rank = (row: FrCostRow) => costMetric(row, 'costBps') ?? costMetric(row, 'feeBps') ?? -Infinity
    const ranked = [...rows].filter(r => costMetric(r, 'costBps') !== null || costMetric(r, 'deviation') !== null || costMetric(r, 'feeBps') !== null)
      .sort((a, b) => rank(b) - rank(a)).slice(0, 12)
    const explain = (row: FrCostRow) => `${row.symbol} · ${row.direction === 'reverse' ? '反套' : '正套'}\n`
      + `成本 ${fmt(costMetric(row, 'costBps'))} bps · 数量覆盖 ${fmt(row.covered.cost.quantityCoveragePct)}%\n`
      + `执行偏差 ${fmt(costMetric(row, 'deviation'))} bps · 数量覆盖 ${fmt(row.covered.deviation.quantityCoveragePct)}%\n`
      + `手续费 ${fmt(costMetric(row, 'feeBps'))} bps · 数量覆盖 ${fmt(row.covered.fee.quantityCoveragePct)}%\n`
      + `入场基差 ${fmt(row.fillSpreadBps)} bps\n现货 taker ${fmt(row.pairedLiquidity.spot.takerNotionalPct)}% / 合约 taker ${fmt(row.pairedLiquidity.futures.takerNotionalPct)}%\n`
      + `配对本金 ${fmt(row.summary.notionalUsdt)} USDT\n点击查看 FIFO 明细`
    barChart.setOption({
      color: ['#256e66', '#d17a4b', '#8699b0'],
      tooltip: { trigger: 'axis', renderMode: 'richText', confine: true, formatter: (items: { dataIndex: number }[]) => items[0] ? explain(ranked[items[0].dataIndex]) : '' },
      legend: { top: 0, textStyle: { fontSize: 11 } },
      grid: { left: 95, right: 18, top: 43, bottom: 35 },
      xAxis: { type: 'value', name: 'bps', nameLocation: 'middle', nameGap: 22, splitLine: { lineStyle: { color: '#edf0ef' } } },
      yAxis: { type: 'category', inverse: true, data: ranked.map(r => r.symbol.replace(/USDT$/, '')), axisTick: { show: false }, axisLine: { show: false } },
      series: ([['costBps', 'cost', '成本估算'], ['deviation', 'deviation', '执行偏差'], ['feeBps', 'fee', '手续费']] as const).map(([key, coverage, name]) => ({
        name, type: 'bar', barMaxWidth: 9,
        data: ranked.map(r => ({ value: costMetric(r, key), itemStyle: { opacity: (r.covered[coverage].quantityCoveragePct ?? 0) >= 99.9999 ? 1 : 0.5 } })),
      })),
      graphic: ranked.length ? [] : [{ type: 'text', left: 'center', top: 'middle', style: { text: '暂无可计算片段，可手动补齐参考', fill: '#65777e', fontSize: 12 } }],
    })
    barChart.on('click', p => { if (ranked[p.dataIndex]) onSelect(ranked[p.dataIndex].symbol) })
    const observer = new ResizeObserver(() => { barChart.resize() })
    observer.observe(bars.current)
    return () => { observer.disconnect(); barChart.dispose() }
  }, [rows, onSelect])
  return <div className="fr-cost-comparison-charts">
    <article><h3>币种成本对比 · 前 12 个</h3><p>优先按可计算成本降序，成本未知时按已知手续费排序。浅色表示部分覆盖，各指标覆盖不同，不直接相加。</p>
      <div ref={bars} className="fr-cost-comparison-chart" role="img" aria-label="各币种手续费、执行偏差与建仓成本对比" /></article>
  </div>
}
