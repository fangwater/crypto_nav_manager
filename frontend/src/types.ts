export interface Strategy {
  slug: string
  alias: string | null
  displayName: string
  dbSchema: string
  host: string
  strategyKind: 'funding_rate' | 'intra_exchange' | 'market_making' | 'cta'
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

