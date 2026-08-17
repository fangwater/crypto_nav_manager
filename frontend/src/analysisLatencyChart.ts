export interface LatencyQuantiles {
  sampleCount: number
  normalCount: number
  p50Ms: number | null
  p90Ms: number | null
}

export interface HourlyLatencyPoint {
  strategySlug: string
  windowStartMs: number
  windowEndMs: number
  computedAtMs: number
  marginNewCreate: LatencyQuantiles
  futuresNewCreate: LatencyQuantiles
  spotTrigger: LatencyQuantiles
  futuresTrigger: LatencyQuantiles
}

export interface HourlyLatencySeries {
  strategySlug: string
  points: HourlyLatencyPoint[]
}

export type LatencyFamilyKey = keyof Pick<
  HourlyLatencyPoint,
  'marginNewCreate' | 'futuresNewCreate' | 'spotTrigger' | 'futuresTrigger'
>

export type LatencyQuantile = 'p50Ms' | 'p90Ms'
export type LatencyLineFilter = 'all' | 'p50' | 'p90'

export interface LatencyChartLineDef {
  key: string
  family: LatencyFamilyKey
  familyName: string
  quantile: LatencyQuantile
  quantileLabel: 'p50' | 'p90'
  name: string
  color: string
  dashed: boolean
}

export interface LatencyChartLine {
  key: string
  name: string
  color: string
  dashed: boolean
  values: Array<number | null>
}

export interface LatencyChartModel {
  chartId: 'hourly-latency'
  distinctFrom: 'fifo-closed-pnl'
  categoryTimes: number[]
  series: LatencyChartLine[]
}

export const LATENCY_CHART_LINES: readonly LatencyChartLineDef[] = [
  {
    key: 'marginNewCreate-p50Ms',
    family: 'marginNewCreate',
    familyName: 'Margin NEW−create',
    quantile: 'p50Ms',
    quantileLabel: 'p50',
    name: 'Margin NEW−create p50',
    color: '#176b5b',
    dashed: false,
  },
  {
    key: 'marginNewCreate-p90Ms',
    family: 'marginNewCreate',
    familyName: 'Margin NEW−create',
    quantile: 'p90Ms',
    quantileLabel: 'p90',
    name: 'Margin NEW−create p90',
    color: '#7aa89d',
    dashed: true,
  },
  {
    key: 'futuresNewCreate-p50Ms',
    family: 'futuresNewCreate',
    familyName: 'Futures NEW−create',
    quantile: 'p50Ms',
    quantileLabel: 'p50',
    name: 'Futures NEW−create p50',
    color: '#2563a7',
    dashed: false,
  },
  {
    key: 'futuresNewCreate-p90Ms',
    family: 'futuresNewCreate',
    familyName: 'Futures NEW−create',
    quantile: 'p90Ms',
    quantileLabel: 'p90',
    name: 'Futures NEW−create p90',
    color: '#7ea2c8',
    dashed: true,
  },
  {
    key: 'spotTrigger-p50Ms',
    family: 'spotTrigger',
    familyName: '现货信号',
    quantile: 'p50Ms',
    quantileLabel: 'p50',
    name: '现货信号 p50',
    color: '#b45309',
    dashed: false,
  },
  {
    key: 'spotTrigger-p90Ms',
    family: 'spotTrigger',
    familyName: '现货信号',
    quantile: 'p90Ms',
    quantileLabel: 'p90',
    name: '现货信号 p90',
    color: '#d4a373',
    dashed: true,
  },
  {
    key: 'futuresTrigger-p50Ms',
    family: 'futuresTrigger',
    familyName: '合约信号',
    quantile: 'p50Ms',
    quantileLabel: 'p50',
    name: '合约信号 p50',
    color: '#7c3a91',
    dashed: false,
  },
  {
    key: 'futuresTrigger-p90Ms',
    family: 'futuresTrigger',
    familyName: '合约信号',
    quantile: 'p90Ms',
    quantileLabel: 'p90',
    name: '合约信号 p90',
    color: '#b48bc9',
    dashed: true,
  },
]

export const LATENCY_CHART_FAMILIES: readonly {
  family: LatencyFamilyKey
  name: string
}[] = [
  { family: 'marginNewCreate', name: 'Margin NEW−create' },
  { family: 'futuresNewCreate', name: 'Futures NEW−create' },
  { family: 'spotTrigger', name: '现货信号' },
  { family: 'futuresTrigger', name: '合约信号' },
]

export const LATENCY_LINE_FILTERS: readonly {
  key: LatencyLineFilter
  label: string
}[] = [
  { key: 'all', label: '全部' },
  { key: 'p50', label: 'p50' },
  { key: 'p90', label: 'p90' },
]

export function defaultLatencyLineKeys(): string[] {
  return latencyLineKeysForFilter('p50')
}

export function latencyLineKeysForFilter(filter: LatencyLineFilter): string[] {
  if (filter === 'all') {
    return LATENCY_CHART_LINES.map((line) => line.key)
  }
  const quantile: LatencyQuantile = filter === 'p50' ? 'p50Ms' : 'p90Ms'
  return LATENCY_CHART_LINES.filter((line) => line.quantile === quantile).map(
    (line) => line.key,
  )
}

export function sameLatencyLineKeys(
  left: readonly string[],
  right: readonly string[],
): boolean {
  return (
    left.length === right.length &&
    left.every((key, index) => key === right[index])
  )
}

export function latencyLineFilterFromKeys(
  selected: readonly string[],
): LatencyLineFilter | 'custom' {
  for (const filter of ['p50', 'p90', 'all'] as const) {
    if (sameLatencyLineKeys(selected, latencyLineKeysForFilter(filter))) {
      return filter
    }
  }
  return 'custom'
}

export function toggleLatencyLineKey(
  selected: readonly string[],
  key: string,
): string[] {
  const next = new Set(selected)
  if (next.has(key)) next.delete(key)
  else next.add(key)
  return LATENCY_CHART_LINES.map((line) => line.key).filter((lineKey) =>
    next.has(lineKey),
  )
}

export function toggleLatencyFamilyKeys(
  selected: readonly string[],
  family: LatencyFamilyKey,
): string[] {
  const familyKeys = LATENCY_CHART_LINES.filter((line) => line.family === family).map(
    (line) => line.key,
  )
  const allSelected = familyKeys.every((key) => selected.includes(key))
  const next = new Set(selected)
  if (allSelected) familyKeys.forEach((key) => next.delete(key))
  else familyKeys.forEach((key) => next.add(key))
  return LATENCY_CHART_LINES.map((line) => line.key).filter((lineKey) =>
    next.has(lineKey),
  )
}

export function ffillLatencyValues(
  values: Array<number | null>,
): Array<number | null> {
  let last: number | null = null
  return values.map((value) => {
    if (value != null && Number.isFinite(value)) {
      last = value
      return value
    }
    return last
  })
}

export function bindHourlyLatencyChart(
  series: HourlyLatencySeries | null | undefined,
): LatencyChartModel {
  const points = [...(series?.points ?? [])].sort(
    (left, right) => left.windowStartMs - right.windowStartMs,
  )
  return {
    chartId: 'hourly-latency',
    distinctFrom: 'fifo-closed-pnl',
    categoryTimes: points.map((point) => point.windowStartMs),
    series: LATENCY_CHART_LINES.map((line) => ({
      key: line.key,
      name: line.name,
      color: line.color,
      dashed: line.dashed,
      values: ffillLatencyValues(
        points.map((point) => point[line.family][line.quantile]),
      ),
    })),
  }
}

export function visibleLatencyChartModel(
  model: LatencyChartModel,
  selectedKeys: readonly string[],
): LatencyChartModel {
  const allowed = new Set(selectedKeys)
  return {
    ...model,
    series: model.series.filter((line) => allowed.has(line.key)),
  }
}

export function latencyChartHasRequiredSeries(model: LatencyChartModel): boolean {
  const names = new Set(model.series.map((line) => line.name))
  return (
    names.has('Margin NEW−create p50') &&
    names.has('Margin NEW−create p90') &&
    names.has('Futures NEW−create p50') &&
    names.has('Futures NEW−create p90') &&
    names.has('现货信号 p50') &&
    names.has('现货信号 p90') &&
    names.has('合约信号 p50') &&
    names.has('合约信号 p90') &&
    model.chartId === 'hourly-latency' &&
    model.distinctFrom === 'fifo-closed-pnl'
  )
}
