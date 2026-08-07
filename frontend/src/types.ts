export interface Health {
  status: string
  strategies: number
  readOnly: boolean
}

export type OpsHealth = 'healthy' | 'warning' | 'critical'
export type OpsComponentHealth =
  | 'online'
  | 'warning'
  | 'offline'
  | 'duplicate'
  | 'zombie'
export type OpsComponentRole =
  | 'trade_signal'
  | 'pre_trade'
  | 'account_monitor'
  | 'trade_engine'
  | 'persist_manager'
  | 'viz_server'

export interface OpsAlertSample {
  severity: 'warning' | 'error'
  atMs: number
  count: number
  message: string
}

export interface OpsAlertSummary {
  warningCount: number
  errorCount: number
  lastAlertAtMs: number | null
  truncated: boolean
  samples: OpsAlertSample[]
}

export interface OpsComponent {
  role: OpsComponentRole
  critical: boolean
  status: OpsComponentHealth
  pid: number | null
  instances: number
  linuxState: string | null
  manager: string
  managerName: string
  alerts: OpsAlertSummary
}

export interface OpsCurrentPosition {
  marginQty: number
  futuresQty: number
  netQty: number
  marginUsd: number
  futuresUsd: number
  netUsd: number
}

export interface OpsTradingBlock {
  symbol: string
  asset: string
  blockedLeg: 'margin' | 'futures' | 'unknown'
  venue: string
  side: string
  orderQty: number | null
  orderPrice: number | null
  httpStatus: number | null
  errorCode: number | null
  errorLabel: string
  errorMessage: string
  firstSeenAtMs: number
  lastSeenAtMs: number
  count: number
  latestClientOrderId: string
  positionStatus: 'live' | 'unavailable'
  positionSnapshotAtMs: number | null
  positionError: string | null
  currentPosition: OpsCurrentPosition | null
}

export interface OpsEnvironment {
  strategySlug: string
  host: string
  profile: 'funding_rate' | 'intra_exchange' | 'market_making'
  status: OpsHealth
  components: OpsComponent[]
  tradingBlocks: OpsTradingBlock[]
}

export interface OpsOverview {
  generatedAtMs: number
  environments: OpsEnvironment[]
}

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
  automaticEnabled: boolean
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

export interface IntraMatchingSummary {
  strategySlug: string
  displayName: string
  exchange: 'binance' | 'bybit'
  sourceReadThroughUs: number
  eventsReleasedThroughUs: number
  marginFinalizedThroughUs: number
  verifiedThroughMs: number
  reorderWindowUs: number
  updatedAtMs: number
  totalOrders: number
  pendingOrders: number
  completedOrders: number
  nettedOrders: number
  mixedOrders: number
  pendingFillAmount: number
  pendingRemainingAmount: number
  pendingNotional: number
  totalHedges: number
  unallocatedHedges: number
  unallocatedAmount: number
  anchorMisses: number
  lastOrderUpdatedAtMs: number | null
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

export type FrLimitExchange = 'binance' | 'gate'
export type FrLimitStatus = 'healthy' | 'warning' | 'error'

export interface FrLimitSourceCounts {
  snapshotRows: number
  exchangeLimitRows: number
  exchangePositionRows: number
  displayedRows: number
  nearLimitRows: number
  symbolConfigRows: number | null
  positionRiskRows: number | null
  leverageBracketRows: number | null
}

export interface FrLimitRow {
  symbol: string
  asset: string
  status: 'healthy' | 'warning' | 'unavailable'
  side: 'long' | 'short' | 'flat'
  trackedInSnapshot: boolean
  positionSource: 'exchange' | 'snapshot'
  positionNotionalUsdt: number
  snapshotOpenUsdt: number | null
  snapshotFuturesUsdt: number | null
  snapshotRestDeltaUsdt: number | null
  exchangeLimitUsdt: number | null
  guardBufferUsdt: number | null
  guardCapUsdt: number | null
  remainingUsdt: number | null
  usageRatio: number | null
  nearLimit: boolean
  amountU: number | null
  pendingLimitOrders: number | null
  leverage: number | null
  symbolConfigLimitUsdt: number | null
  positionRiskLimitUsdt: number | null
  bracketLimitUsdt: number | null
  error: string | null
}

export interface FrLimitEnvironment {
  strategySlug: string
  exchange: FrLimitExchange
  status: FrLimitStatus
  snapshotTsMs: number | null
  exchangeFetchedAtMs: number | null
  paramsLive: boolean
  sourceCounts: FrLimitSourceCounts
  rows: FrLimitRow[]
  warnings: string[]
  error: string | null
}

export interface FrPositionLimitOverview {
  generatedAtMs: number
  alertThresholdRatio: number
  environments: FrLimitEnvironment[]
}
