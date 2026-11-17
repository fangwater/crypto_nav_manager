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
    case 'fee': return s.feeUsdt ?? row.covered?.fee.valueUsdt ?? null
    case 'feeBps': return bps(s.feeUsdt) ?? row.covered?.fee.bps ?? null
    case 'basis': return row.fillSpreadBps
    case 'basisAbs': return row.fillSpreadBps === null ? null : Math.abs(row.fillSpreadBps)
    case 'deviation': return row.executionDeviationBps ?? row.covered?.deviation.bps ?? null
    case 'cost': return s.estimatedCostUsdt ?? row.covered?.cost.valueUsdt ?? null
    case 'costBps': return s.estimatedCostBps ?? row.covered?.cost.bps ?? null
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

export const COST_REVIEW_RULES = { costBps: 10, takerPct: 30, adverseBasisBps: 50, minimumCoveragePct: 50, limit: 5 } as const
export type CoveredCostMetric = 'feeBps' | 'deviation' | 'costBps'

export function costMetricCoverage(row: FrCostRow, key: CoveredCostMetric): number | null {
  if (costMetric(row, key) === null) return null
  const full = key === 'feeBps'
    ? row.summary.notionalUsdt !== null && row.summary.notionalUsdt > 0 ? row.summary.feeUsdt : null
    : key === 'deviation' ? row.executionDeviationBps : row.summary.estimatedCostBps
  if (full !== null) return 100
  return row.covered?.[key === 'feeBps' ? 'fee' : key === 'costBps' ? 'cost' : 'deviation'].quantityCoveragePct ?? null
}

// Positive means economically adverse for the actual position direction.
// Raw (F-S)/S alone cannot distinguish favorable and adverse entry basis.
export function adverseEntryBasis(row: FrCostRow): number | null {
  const basis = costMetric(row, 'basis')
  return basis === null || row.direction === null ? null : row.direction === 'positive' ? -basis : basis
}

export interface CostConcern {
  row: FrCostRow
  reasons: string[]
  priority: number
  magnitude: number
}

export function rankCostConcerns(rows: FrCostRow[]): CostConcern[] {
  const { costBps, takerPct, adverseBasisBps, minimumCoveragePct } = COST_REVIEW_RULES
  return rows.flatMap(row => {
    if (!row.summary.pairCount) return []
    const reasons: string[] = []
    const adverseCosts: number[] = []
    for (const [key, label] of [['deviation', '执行偏差高'], ['costBps', '成本偏高'], ['feeBps', '手续费偏高']] as const) {
      const value = costMetric(row, key)
      if (value !== null && value >= costBps && (costMetricCoverage(row, key) ?? 0) >= minimumCoveragePct) {
        reasons.push(label); adverseCosts.push(value)
      }
    }
    const highTaker: number[] = []
    for (const [key, label] of [['spot', '现货 taker 高'], ['futures', '合约 taker 高']] as const) {
      const value = row.pairedLiquidity[key].takerNotionalPct
      // A known lower bound can prove a breach even when the other leg or
      // some liquidity roles are unknown. Unknown share is never imputed.
      if (value !== null && value >= takerPct) { reasons.push(label); highTaker.push(value) }
    }
    const basis = adverseEntryBasis(row)
    if (basis !== null && basis >= adverseBasisBps) reasons.push('不利入场基差')
    if (!reasons.length) return []
    return [{ row, reasons, priority: adverseCosts.length ? 0 : highTaker.length ? 1 : 2,
      magnitude: adverseCosts.length ? Math.max(...adverseCosts) : highTaker.length ? Math.max(...highTaker) : basis! }]
  }).sort((a, b) => a.priority - b.priority || b.magnitude - a.magnitude || a.row.symbol.localeCompare(b.row.symbol))
}
