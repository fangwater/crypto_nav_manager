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
