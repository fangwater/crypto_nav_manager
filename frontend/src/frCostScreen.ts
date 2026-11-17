import type { FrCostRow } from './frEntryCost'

export type CostMetric = 'notional' | 'spotTaker' | 'futuresTaker' | 'taker' | 'fee' | 'feeBps' | 'basis' | 'basisAbs' | 'deviation' | 'cost' | 'costBps' | 'lastPair'
export type ThresholdMetric = 'taker' | 'feeBps' | 'basisAbs' | 'deviation' | 'costBps'
export type Thresholds = Record<ThresholdMetric, string>
export const EMPTY_THRESHOLDS: Thresholds = { taker: '', feeBps: '', basisAbs: '', deviation: '', costBps: '' }

export function numberInput(value: string): number | null {
  const parsed = value.trim() ? Number(value) : NaN
  return Number.isFinite(parsed) ? parsed : null
}

export function costMetric(row: FrCostRow, key: CostMetric): number | null {
  if (!row.summary.pairCount) return null
  const { summary: s, pairedLiquidity: mix } = row
  const bps = (n: number | null) => n !== null && s.notionalUsdt !== null && s.notionalUsdt > 0 ? n / s.notionalUsdt * 10_000 : null
  switch (key) {
    case 'notional': return s.notionalUsdt
    case 'spotTaker': return mix.spot.takerNotionalPct
    case 'futuresTaker': return mix.futures.takerNotionalPct
    case 'taker': return mix.spot.takerNotionalPct !== null && mix.futures.takerNotionalPct !== null
      ? Math.max(mix.spot.takerNotionalPct, mix.futures.takerNotionalPct) : null
    case 'fee': return s.feeUsdt
    case 'feeBps': return bps(s.feeUsdt)
    case 'basis': return row.fillSpreadBps
    case 'basisAbs': return row.fillSpreadBps === null ? null : Math.abs(row.fillSpreadBps)
    case 'deviation': return row.executionDeviationBps
    case 'cost': return s.estimatedCostUsdt
    case 'costBps': return s.estimatedCostBps
    case 'lastPair': return row.lastPairMs
  }
}

// Three-valued filtering: unknown observations cannot prove a threshold passes
// or fails. Known taker share is a lower bound when liquidity roles are missing.
function thresholdResult(row: FrCostRow, key: ThresholdMetric, threshold: number): boolean | null {
  if (key === 'taker') {
    const legs = [row.pairedLiquidity.spot, row.pairedLiquidity.futures]
    if (!row.summary.pairCount) return null
    if (legs.some(leg => leg.takerNotionalPct !== null && leg.takerNotionalPct >= threshold)) return true
    if (legs.some(leg => leg.takerNotionalPct === null || leg.unknownNotionalPct === null
      || (leg.unknownNotionalPct > 0 && leg.takerNotionalPct + leg.unknownNotionalPct >= threshold))) return null
    return false
  }
  const value = costMetric(row, key)
  return value === null ? null : value >= threshold
}

export function matchesCostThresholds(row: FrCostRow, thresholds: Thresholds, mode: 'any' | 'all', minimumNotional: number | null = null): boolean | null {
  const results = (Object.entries(thresholds) as [ThresholdMetric, string][]).flatMap(([key, raw]) => {
    const cutoff = numberInput(raw)
    return cutoff === null ? [] : [thresholdResult(row, key, cutoff)]
  })
  const combined = !results.length ? true
    : mode === 'any' && results.includes(true) ? true
      : mode === 'all' && results.includes(false) ? false
        : results.includes(null) ? null : mode === 'all'
  if (minimumNotional === null || combined === false) return combined
  const notional = costMetric(row, 'notional')
  if (notional === null) return null
  return notional < minimumNotional ? false : combined
}

export function compareCostRows(a: FrCostRow, b: FrCostRow, key: CostMetric | 'symbol', ascending: boolean): number {
  if (key === 'symbol') return (ascending ? 1 : -1) * a.symbol.localeCompare(b.symbol)
  const av = costMetric(a, key), bv = costMetric(b, key)
  if (av === null || bv === null) return av === bv ? a.symbol.localeCompare(b.symbol) : av === null ? 1 : -1
  return (ascending ? av - bv : bv - av) || a.symbol.localeCompare(b.symbol)
}
