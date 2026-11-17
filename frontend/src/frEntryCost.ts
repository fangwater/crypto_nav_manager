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
  analysis: FrEntryAnalysis
}

export interface LiquidityBucket {
  fillCount: number
  quantity: number
  notionalUsdt: number | null
  feeUsdt: number | null
  unconvertedFeeParts: number
}

export interface LegLiquidity {
  maker: LiquidityBucket
  taker: LiquidityBucket
  unknown: LiquidityBucket
  total: LiquidityBucket
  makerNotionalPct: number | null
  takerNotionalPct: number | null
  unknownNotionalPct: number | null
}

export interface LiquidityMix { spot: LegLiquidity; futures: LegLiquidity }
export interface BasisMilestone { eventMs: number; referenceMinuteMs: number; premiumBps: number | null }
export interface BasisEvolution {
  start: BasisMilestone | null
  lastPair: BasisMilestone | null
  asOf: BasisMilestone
  isScenario: boolean
  buildChangeBps: number | null
  afterPairChangeBps: number | null
  totalChangeBps: number | null
  coveredMinutes: number
  expectedMinutes: number
  points: { ts: number; premiumBps: number | null }[]
}
export interface FrEntryAnalysis {
  pairedLiquidity: LiquidityMix
  windowLiquidity: LiquidityMix | null
  spotVwap: number | null
  futuresVwap: number | null
  fillSpread: number | null
  fillSpreadBps: number | null
  executionDeviationBps: number | null
  deviationAssessment: 'adverse' | 'favorable' | 'at_reference' | 'unknown'
  basis: BasisEvolution
}

export function costBps(amount: number | null, notional: number | null): number | null {
  return amount !== null && notional !== null && notional > 0
    ? amount / notional * 10_000 : null
}

export function coverage(covered: number, total: number): number | null {
  return total > 0 ? covered / total * 100 : null
}
