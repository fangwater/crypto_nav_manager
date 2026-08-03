export type MarketDataHealth = 'OK' | 'WARN' | 'CRITICAL' | 'UNKNOWN'

export interface MarketDataProcess {
  pid: number
  affinity: number[]
}

export interface MarketDataNetworkWindow {
  socket_count: number
  established_count: number
  rx_bytes: number | null
  rx_idle_secs: number | null
  retransmits: number | null
  socket_drops: number | null
  reconnects: number | null
  disconnects: number | null
  recv_queue_bytes: number
}

export interface MarketDataTarget {
  name: string
  venue: string
  expected_cpu: number
  process: MarketDataProcess | null
  process_candidates: number
  network: MarketDataNetworkWindow
  status: MarketDataHealth
  reasons: string[]
}

export interface MarketDataSnapshot {
  timestamp_unix_ms: number
  window_secs: number | null
  host: {
    hostname: string
    kernel_release: string
  }
  targets: MarketDataTarget[]
  system: {
    interface: string
    status: MarketDataHealth
    reasons: string[]
  }
  capabilities: {
    bpf: boolean
    bpf_reason: string | null
  }
}

const MARKET_DATA_API_BASE = '/market-data-api'

export async function getMarketDataSnapshot(
  signal?: AbortSignal,
): Promise<MarketDataSnapshot> {
  const response = await fetch(MARKET_DATA_API_BASE + '/v1/snapshot', {
    headers: { Accept: 'application/json' },
    signal,
  })
  if (!response.ok) {
    throw new Error('HTTP ' + response.status)
  }
  return response.json() as Promise<MarketDataSnapshot>
}
