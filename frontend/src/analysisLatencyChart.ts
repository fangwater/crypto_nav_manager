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

export interface LatencyChartLine {
  key: string
  name: string
  values: Array<number | null>
}

export interface LatencyChartModel {
  chartId: 'hourly-latency'
  distinctFrom: 'fifo-closed-pnl'
  categoryTimes: number[]
  series: LatencyChartLine[]
}

const LATENCY_LINES: ReadonlyArray<{
  key: keyof HourlyLatencyPoint
  quantile: 'p50Ms' | 'p90Ms'
  name: string
}> = [
  { key: 'marginNewCreate', quantile: 'p50Ms', name: 'Margin NEW−create p50' },
  { key: 'marginNewCreate', quantile: 'p90Ms', name: 'Margin NEW−create p90' },
  { key: 'futuresNewCreate', quantile: 'p50Ms', name: 'Futures NEW−create p50' },
  { key: 'futuresNewCreate', quantile: 'p90Ms', name: 'Futures NEW−create p90' },
  { key: 'spotTrigger', quantile: 'p50Ms', name: '现货信号 p50' },
  { key: 'spotTrigger', quantile: 'p90Ms', name: '现货信号 p90' },
  { key: 'futuresTrigger', quantile: 'p50Ms', name: '合约信号 p50' },
  { key: 'futuresTrigger', quantile: 'p90Ms', name: '合约信号 p90' },
]

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
    series: LATENCY_LINES.map((line) => ({
      key: `${String(line.key)}-${line.quantile}`,
      name: line.name,
      values: ffillLatencyValues(
        points.map((point) => {
          const quantiles = point[line.key] as LatencyQuantiles
          return quantiles[line.quantile]
        }),
      ),
    })),
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
