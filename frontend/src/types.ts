export interface Strategy {
  slug: string
  alias: string | null
  displayName: string
  dbSchema: string
  host: string
  strategyKind: 'funding_rate' | 'intra_exchange' | 'market_making'
  exchange: 'binance' | 'bybit' | 'gate' | 'bitget' | 'okx'
  accountMode: string
  envPath: string
  csvOutputDir: string
  stMs: number
  configUrl: string
  sortOrder: number
  envExists: boolean
  credentialsReady: boolean
  missingKeys: string[]
}

export interface StrategySnapshotSummary {
  strategySlug: string
  snapshotTsMs: number
  fetchedAtMs: number
  sourceUrl: string
}

export interface AccountRisk {
  strategySlug: string
  exchange: Strategy['exchange']
  connected: boolean
  status: 'live' | 'stale' | 'waiting' | 'unavailable'
  riskLevel: 'free_trade' | 'warning' | 'reduce_only' | 'liquidation' | null
  scope: string | null
  sourceTsMs: number | null
  receivedAtMs: number | null
  uniMmr: number | null
  adjustedEquityUsd: number | null
  actualEquityUsd: number | null
  maintenanceMarginUsd: number | null
  initialMarginUsd: number | null
  borrowedUsd: number | null
  notionalUsd: number | null
}

export interface HistorySyncDataset {
  dataset: string
  successEndMs: number | null
  fetchedAtMs: number | null
}

export interface HistorySyncStatus {
  strategySlug: string
  scheduled: boolean
  lastFetchedAtMs: number | null
  datasets: HistorySyncDataset[]
}

export interface AlignmentStatus {
  strategySlug: string
  state: 'waiting' | 'running' | 'succeeded' | 'mismatch' | 'failed'
  phase: string
  progressPercent: number
  startedAtMs: number | null
  updatedAtMs: number
  completedAtMs: number | null
  candidateEndMs: number | null
  scanStartMs: number | null
  pgSuccessEndMs: number | null
  actualEndMs: number | null
  groupCount: number | null
  mismatchCount: number | null
  pgEventCount: number | null
  localEventCount: number | null
  message: string | null
}

export interface TradingFeeRate {
  market: string
  instrument: string
  makerRate: string
  takerRate: string
  feeTier: string | null
  feeGroup: string | null
  effectiveAtMs: number
  fetchedAtMs: number
}

export interface AccountFeeRates {
  slug: string
  displayName: string
  exchange: Strategy['exchange']
  accountMode: string
  strategyKind: Strategy['strategyKind']
  sortOrder: number
  rates: TradingFeeRate[]
  hiddenRateCount: number
  hiddenInstrumentCount: number
}
export interface PnlSummary {
  tradeCount: number
  volumeUsdt: number
  feeBeforePnlUsdt: number
  tradingFeeUsdt: number
  feeAfterPnlUsdt: number
  fundingPnlUsdt: number
  interestCostUsdt: number
  floatingPnlUsdt: number
  totalPnlUsdt: number
  returnBpsOnVolume: number
  openAmountUsdt: number
  unconvertedFeeCount: number
  unconvertedInterestCount: number
}

export interface SymbolPnlSummary extends PnlSummary {
  symbol: string
}

export interface PnlPoint {
  ts: number
  feeBeforePnlUsdt: number
  feeAfterPnlUsdt: number
  fundingPnlUsdt: number
  interestCostUsdt: number
  floatingPnlUsdt: number
  totalPnlUsdt: number
  spotPositionUsdt: number
  futuresPositionUsdt: number
  exposureUsdt: number
  spotPositionQty: number
  futuresPositionQty: number
  exposureQty: number
}

export interface SymbolPnlSeries {
  symbol: string
  points: PnlPoint[]
}

export interface PnlSourceInfo {
  adapter: string
  loadedTradeRows: number
  loadedFundingRows: number
  loadedInterestRows: number
  returnedPoints: number
  returnedSymbolPoints: number
  sampled: boolean
  interestIncluded: boolean
  initialSnapshotTsMs: number | null
  initialPositionCount: number
  skippedInitialPositionCount: number
}

export interface StrategyPnl {
  strategyStartMs: number
  startMs: number
  endMs: number
  selectedSymbols: string[]
  availableSymbols: string[]
  summary: PnlSummary
  symbols: SymbolPnlSummary[]
  points: PnlPoint[]
  symbolPoints: SymbolPnlSeries[]
  source: PnlSourceInfo
}

export type PnlSeriesKey =
  | 'totalPnlUsdt'
  | 'feeBeforePnlUsdt'
  | 'feeAfterPnlUsdt'
  | 'fundingPnlUsdt'
  | 'interestCostUsdt'
  | 'floatingPnlUsdt'

export type PositionSeriesKey =
  | 'spotPositionUsdt'
  | 'futuresPositionUsdt'
  | 'exposureUsdt'

export type PositionUnit = 'usdt' | 'qty'
