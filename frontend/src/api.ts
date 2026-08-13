import type {
  AccountFeeRates,
  AccountRisk,
  AlignmentStatus,
  FrPositionLimitOverview,
  HistorySyncStatus,
  Health,
  IntraAnalysis,
  IntraMatchingSummary,
  OpsOverview,
  Strategy,
  StrategyPnl,
  StrategySnapshotSummary,
} from './types'

const API_BASE = import.meta.env.VITE_NAV_API_BASE ?? '/nav-api'
const OPS_API_BASE = '/ops-api'

export function getHealth(signal?: AbortSignal): Promise<Health> {
  return getJson<Health>('/health', signal)
}

async function getJson<T>(path: string, signal?: AbortSignal): Promise<T> {
  const response = await fetch(API_BASE + path, {
    headers: { Accept: 'application/json' },
    signal,
  })

  if (!response.ok) {
    const payload = (await response.json().catch(() => null)) as
      | { error?: string }
      | null
    throw new Error(payload?.error ?? 'HTTP ' + response.status)
  }

  return response.json() as Promise<T>
}

async function getOpsJson<T>(path: string, signal?: AbortSignal): Promise<T> {
  const response = await fetch(OPS_API_BASE + path, {
    headers: { Accept: 'application/json' },
    signal,
  })
  if (!response.ok) {
    throw new Error('HTTP ' + response.status)
  }
  return response.json() as Promise<T>
}

export function getOpsOverview(signal?: AbortSignal): Promise<OpsOverview> {
  return getOpsJson<OpsOverview>('/api/v1/overview', signal)
}

export function getStrategies(): Promise<Strategy[]> {
  return getJson<Strategy[]>('/strategies')
}

export function getAccountRisks(signal?: AbortSignal): Promise<AccountRisk[]> {
  return getJson<AccountRisk[]>('/account-risks', signal)
}

export function getHistorySyncStatuses(
  signal?: AbortSignal,
): Promise<HistorySyncStatus[]> {
  return getJson<HistorySyncStatus[]>('/history-sync-status', signal)
}

export function getAlignmentStatuses(
  signal?: AbortSignal,
): Promise<AlignmentStatus[]> {
  return getJson<AlignmentStatus[]>('/alignment-status', signal)
}

export function getIntraMatchingSummaries(
  signal?: AbortSignal,
): Promise<IntraMatchingSummary[]> {
  return getJson<IntraMatchingSummary[]>('/intra-matching', signal)
}

export function getStrategy(slug: string): Promise<Strategy> {
  return getJson<Strategy>('/strategies/' + encodeURIComponent(slug))
}

export function getStrategySnapshots(
  slug: string,
): Promise<StrategySnapshotSummary[]> {
  return getJson<StrategySnapshotSummary[]>(
    '/snapshots/' + encodeURIComponent(slug) + '/history',
  )
}

export function getInitialSnapshot(
  slug: string,
): Promise<StrategySnapshotSummary | null> {
  return getJson<StrategySnapshotSummary | null>(
    '/strategies/' + encodeURIComponent(slug) + '/initial-snapshot',
  )
}

export async function setInitialSnapshot(
  slug: string,
  snapshotTsMs: number,
): Promise<StrategySnapshotSummary> {
  const response = await fetch(
    API_BASE + '/strategies/' + encodeURIComponent(slug) + '/initial-snapshot',
    {
      method: 'PUT',
      headers: {
        Accept: 'application/json',
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({ snapshotTsMs }),
    },
  )
  if (!response.ok) {
    const payload = (await response.json().catch(() => null)) as
      | { error?: string }
      | null
    throw new Error(payload?.error ?? 'HTTP ' + response.status)
  }
  return response.json() as Promise<StrategySnapshotSummary>
}

export async function clearInitialSnapshot(slug: string): Promise<void> {
  const response = await fetch(
    API_BASE + '/strategies/' + encodeURIComponent(slug) + '/initial-snapshot',
    {
      method: 'DELETE',
      headers: { Accept: 'application/json' },
    },
  )
  if (!response.ok) {
    const payload = (await response.json().catch(() => null)) as
      | { error?: string }
      | null
    throw new Error(payload?.error ?? 'HTTP ' + response.status)
  }
}

export function getFeeRates(signal?: AbortSignal): Promise<AccountFeeRates[]> {
  return getJson<AccountFeeRates[]>('/fee-rates', signal)
}

export function getAccountFeeRates(slug: string): Promise<AccountFeeRates> {
  return getJson<AccountFeeRates>(
    '/fee-rates/' + encodeURIComponent(slug),
  )
}

export async function syncFeeRates(slug: string): Promise<void> {
  const response = await fetch(
    API_BASE + '/fee-rates/' + encodeURIComponent(slug) + '/sync',
    {
      method: 'POST',
      headers: { Accept: 'application/json' },
    },
  )
  if (!response.ok) {
    const payload = (await response.json().catch(() => null)) as
      | { error?: string }
      | null
    throw new Error(payload?.error ?? 'HTTP ' + response.status)
  }
}

export function getFrPositionLimits(
  signal?: AbortSignal,
): Promise<FrPositionLimitOverview> {
  return getJson<FrPositionLimitOverview>('/fr-position-limits', signal)
}

export interface StrategyPnlQuery {
  startMs: number
  endMs: number
  symbols?: string[]
  maxPoints?: number
  signal?: AbortSignal
}

export function getStrategyPnl(
  slug: string,
  query: StrategyPnlQuery,
): Promise<StrategyPnl> {
  const params = new URLSearchParams({
    startMs: String(query.startMs),
    endMs: String(query.endMs),
    maxPoints: String(query.maxPoints ?? 3000),
  })
  if (query.symbols?.length) {
    params.set('symbols', query.symbols.join(','))
  }
  return getJson<StrategyPnl>(
    '/strategies/' + encodeURIComponent(slug) + '/pnl?' + params,
    query.signal,
  )
}

export interface IntraAnalysisQuery {
  startMs: number
  endMs: number
  symbols?: string[]
  referenceFeeBps?: number
  maxPoints?: number
  maxMatches?: number
  signal?: AbortSignal
}

export function getIntraAnalysis(
  slug: string,
  query: IntraAnalysisQuery,
): Promise<IntraAnalysis> {
  const params = new URLSearchParams({
    startMs: String(query.startMs),
    endMs: String(query.endMs),
    maxPoints: String(query.maxPoints ?? 3000),
    maxMatches: String(query.maxMatches ?? 200),
    referenceFeeBps: String(query.referenceFeeBps ?? 1),
  })
  if (query.symbols?.length) {
    params.set('symbols', query.symbols.join(','))
  }
  return getJson<IntraAnalysis>(
    '/analysis/' + encodeURIComponent(slug) + '/intra-fifo?' + params,
    query.signal,
  )
}
