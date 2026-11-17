import { BarChart, ScatterChart } from 'echarts/charts'
import { GraphicComponent, GridComponent, LegendComponent, MarkLineComponent, TooltipComponent } from 'echarts/components'
import * as echarts from 'echarts/core'
import { CanvasRenderer } from 'echarts/renderers'
import { useEffect, useRef } from 'react'
import type { FrCostRow } from '../frEntryCost'
import { costMetric } from '../frCostScreen'

echarts.use([BarChart, ScatterChart, GraphicComponent, GridComponent, LegendComponent, MarkLineComponent, TooltipComponent, CanvasRenderer])
const fmt = (n: number | null) => n === null ? '未知' : n.toLocaleString('en-US', { maximumFractionDigits: 2 })

export function FrCostOverviewCharts({ rows, takerThreshold, basisThreshold, onSelect }: {
  rows: FrCostRow[]; takerThreshold: number | null; basisThreshold: number | null; onSelect: (symbol: string) => void
}) {
  const bars = useRef<HTMLDivElement>(null), scatter = useRef<HTMLDivElement>(null)
  const plotted = rows.filter(r => costMetric(r, 'taker') !== null && costMetric(r, 'basisAbs') !== null).length
  useEffect(() => {
    if (!bars.current || !scatter.current) return
    const barChart = echarts.init(bars.current), scatterChart = echarts.init(scatter.current)
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
    const points = rows.filter(r => costMetric(r, 'taker') !== null && costMetric(r, 'basisAbs') !== null)
    scatterChart.setOption({
      tooltip: { renderMode: 'richText', confine: true, formatter: (p: { dataIndex: number }) => explain(points[p.dataIndex]) },
      grid: { left: 65, right: 24, top: 40, bottom: 45 },
      xAxis: { type: 'value', min: 0, max: 100, name: '任一腿 taker %', nameLocation: 'middle', nameGap: 28, splitLine: { lineStyle: { color: '#edf0ef' } } },
      yAxis: { type: 'value', name: '|入场基差| bps', splitLine: { lineStyle: { color: '#edf0ef' } } },
      series: [{ type: 'scatter',
        data: points.map(r => {
          const deviation = costMetric(r, 'deviation')
          return { name: r.symbol, value: [costMetric(r, 'taker'), costMetric(r, 'basisAbs')],
            symbolSize: Math.max(8, Math.min(28, 5 + Math.log10(Math.max(1, r.summary.notionalUsdt ?? 1)) * 4)),
            itemStyle: { color: deviation === null ? '#93a4ae' : deviation > 0 ? '#ca7448' : '#287d72', opacity: 0.8 },
          }
        }),
        markLine: { silent: true, symbol: 'none', label: { show: false }, lineStyle: { type: 'dashed', color: '#b58d54' },
          data: [...(takerThreshold === null ? [] : [{ xAxis: takerThreshold }]), ...(basisThreshold === null ? [] : [{ yAxis: basisThreshold }])] },
      }],
    })
    barChart.on('click', p => { if (ranked[p.dataIndex]) onSelect(ranked[p.dataIndex].symbol) })
    scatterChart.on('click', p => { if (points[p.dataIndex]) onSelect(points[p.dataIndex].symbol) })
    const observer = new ResizeObserver(() => { barChart.resize(); scatterChart.resize() })
    observer.observe(bars.current); observer.observe(scatter.current)
    return () => { observer.disconnect(); barChart.dispose(); scatterChart.dispose() }
  }, [rows, takerThreshold, basisThreshold, onSelect])
  return <div className="fr-cost-comparison-charts">
    <article><h3>币种成本对比 · 前 12 个</h3><p>优先按可计算成本降序，成本未知时按已知手续费排序。浅色表示部分覆盖，各指标覆盖不同，不直接相加。</p>
      <div ref={bars} className="fr-cost-comparison-chart" role="img" aria-label="各币种手续费、执行偏差与建仓成本对比" /></article>
    <article><h3>Taker 与入场基差分布</h3><p>点越大，配对本金越大；橙色为不利执行，绿色为有利或持平，灰色为参考未知。图中 {plotted} / {rows.length} 个币种。</p>
      <div ref={scatter} className="fr-cost-comparison-chart" role="img" aria-label="当前持仓 taker 占比与基差绝对值散点图" /></article>
  </div>
}
