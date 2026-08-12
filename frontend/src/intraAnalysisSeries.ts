import type { IntraAnalysisSeriesKey } from './types'

export const intraAnalysisMetricOptions: ReadonlyArray<{
  key: IntraAnalysisSeriesKey
  label: string
}> = [
  { key: 'realizedPnlUsdt', label: '实际成交' },
  { key: 'marketPnlUsdt', label: '选币基差' },
  { key: 'executionPnlUsdt', label: '闭环执行' },
  { key: 'executionCaptureUsdt', label: '窗口 MT 执行' },
]

const intraSymbolColors = [
  '#176b5b',
  '#2563a7',
  '#b45309',
  '#7c3a91',
  '#b5473c',
  '#3d7c2f',
  '#3f5d83',
  '#a66b12',
  '#087f8c',
  '#8e4b67',
  '#6c6f2d',
  '#4e6ba6',
  '#a34f2d',
  '#287271',
  '#715a9c',
  '#997404',
  '#2f7d4f',
  '#9a4e4e',
  '#426e86',
  '#765f3d',
]

export function intraAnalysisMetricLabel(metric: IntraAnalysisSeriesKey) {
  return intraAnalysisMetricOptions.find((option) => option.key === metric)?.label ?? metric
}

export function intraSymbolColor(index: number) {
  return intraSymbolColors[index % intraSymbolColors.length]
}
