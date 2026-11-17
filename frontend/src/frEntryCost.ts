export interface FrCostTotals {
  pairCount: number
  quantity: number
  pricedQuantity: number
  referenceCoveredQuantity: number
  costCoveredQuantity: number
  crossMinuteQuantity: number
  maxLagMs: number
  notionalUsdt: number | null
  entrySpreadUsdt: number | null
  feeUsdt: number | null
  referenceSpreadUsdt: number | null
  executionDeviationUsdt: number | null
  estimatedCostUsdt: number | null
  estimatedCostBps: number | null
}

export interface FrCostLot {
  id: string
  orderId: string
  ts: number
  leg: 'spot' | 'futures'
  side: number
  price: number | null
  quantity: number
  feeUsdt: number | null
  liquidity: string
}

export interface FrCostPair {
  spot: FrCostLot
  futures: FrCostLot
  quantity: number
  direction: 'positive' | 'reverse'
  completedAtMs: number
  lagMs: number
  crossMinute: boolean
  notionalUsdt: number | null
  entrySpreadUsdt: number | null
  feeUsdt: number | null
  premiumRate: number | null
  referenceMinutes: number
  expectedMinutes: number
  referenceSpreadUsdt: number | null
  executionDeviationUsdt: number | null
  estimatedCostUsdt: number | null
}

export interface FrEntryCost {
  strategySlug: string
  displayName: string
  exchange: string
  symbol: string
  availableSymbols: string[]
  historyStartMs: number
  endMs: number
  lastTradeMs: number | null
  referenceSource: string
  referenceBps: number | null
  factualSnapshotMs: number | null
  factualPosition: { spotQuantity: number; futuresQuantity: number } | null
  warnings: string[]
  alignment: { ts: number; spotQuantity: number; futuresQuantity: number; netQuantity: number }[]
  report: {
    spotQuantity: number
    futuresQuantity: number
    netQuantity: number
    tradeCount: number
    summary: FrCostTotals
    minutes: (FrCostTotals & { ts: number })[]
    pairs: FrCostPair[]
    unmatched: FrCostLot[]
  }
}

export function costBps(amount: number | null, notional: number | null): number | null {
  return amount !== null && notional !== null && notional > 0
    ? amount / notional * 10_000 : null
}

export function coverage(covered: number, total: number): number | null {
  return total > 0 ? covered / total * 100 : null
}
