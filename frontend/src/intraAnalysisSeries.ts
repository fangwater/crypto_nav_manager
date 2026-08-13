import type { IntraFeeMode, IntraPnlSeriesKey } from './types'

export const intraFeeModeOptions: ReadonlyArray<{
  key: IntraFeeMode
  label: string
  metric: IntraPnlSeriesKey
  color: string
}> = [
  { key: 'gross', label: 'Fee 前', metric: 'realizedPnlUsdt', color: '#176b5b' },
  { key: 'actual', label: '实际 Fee 后', metric: 'feeAfterPnlUsdt', color: '#087f8c' },
  {
    key: 'reference',
    label: '参考 Fee 后',
    metric: 'referenceFeeAfterPnlUsdt',
    color: '#6c6f2d',
  },
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

export function intraFeeModeConfig(mode: IntraFeeMode) {
  return intraFeeModeOptions.find((option) => option.key === mode) ?? intraFeeModeOptions[0]
}

export function intraSymbolColor(index: number) {
  return intraSymbolColors[index % intraSymbolColors.length]
}
