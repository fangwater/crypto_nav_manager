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

export interface MarketDataHistoryPoint {
  bucket_start_unix_ms: number
  status: MarketDataHealth
  samples: number
  rx_bytes: number
  reconnects: number
  disconnects: number
  retransmits: number
  socket_drops: number
  max_rx_idle_secs: number
  max_recv_queue_bytes: number
  min_established_count: number
  max_socket_count: number
}

export interface MarketDataTargetHistory {
  name: string
  venue: string
  points: MarketDataHistoryPoint[]
}

export interface MarketDataHistory {
  generated_at_unix_ms: number
  from_unix_ms: number
  bucket_secs: number
  retention_hours: number
  targets: MarketDataTargetHistory[]
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

export async function getMarketDataHistory(
  hours = 24,
  signal?: AbortSignal,
): Promise<MarketDataHistory> {
  const query = new URLSearchParams({ hours: String(hours) })
  const response = await fetch(
    MARKET_DATA_API_BASE + '/v1/history?' + query.toString(),
    {
      headers: { Accept: 'application/json' },
      signal,
    },
  )
  if (!response.ok) {
    throw new Error('HTTP ' + response.status)
  }
  return response.json() as Promise<MarketDataHistory>
}
