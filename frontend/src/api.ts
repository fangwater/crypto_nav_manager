import type { HourlyLatencySeries } from './analysisLatencyChart'
import type {
  AccountFeeRates,
  AccountRisk,
  AlignmentStatus,
  AuthSession,
  FrPositionLimitOverview,
  HistorySyncStatus,
  Health,
  IntraAnalysis,
  IntraMatchingSummary,
  ManagedUser,
  NavRole,
  OpsOverview,
  Strategy,
  StrategyPnl,
  StrategySnapshotSummary,
} from './types'

const API_BASE = import.meta.env.VITE_NAV_API_BASE ?? '/nav-api'
const OPS_API_BASE = '/ops-api'

let onUnauthorized: (() => void) | null = null

export function setUnauthorizedHandler(handler: (() => void) | null) {
  onUnauthorized = handler
}

async function apiFetch(path: string, init?: RequestInit): Promise<Response> {
  const response = await fetch(API_BASE + path, {
    ...init,
    headers: {
      Accept: 'application/json',
      ...(init?.body ? { 'Content-Type': 'application/json' } : {}),
      ...init?.headers,
    },
  })
  if (response.status === 401) {
    onUnauthorized?.()
  }
  return response
}

async function throwUnlessOk(response: Response): Promise<void> {
  if (response.ok) return
  const payload = (await response.json().catch(() => null)) as
    | { error?: string }
    | null
  throw new Error(payload?.error ?? 'HTTP ' + response.status)
}

async function getJson<T>(path: string, signal?: AbortSignal): Promise<T> {
  const response = await apiFetch(path, { signal })
  await throwUnlessOk(response)
  return response.json() as Promise<T>
}

export function getHealth(signal?: AbortSignal): Promise<Health> {
  return getJson<Health>('/health', signal)
}

export async function login(
  username: string,
  password: string,
): Promise<AuthSession> {
  const response = await apiFetch('/auth/login', {
    method: 'POST',
    body: JSON.stringify({ username, password }),
  })
  await throwUnlessOk(response)
  return response.json() as Promise<AuthSession>
}

export async function logout(): Promise<void> {
  const response = await apiFetch('/auth/logout', { method: 'POST' })
  await throwUnlessOk(response)
}

export function getSession(signal?: AbortSignal): Promise<AuthSession> {
  return getJson<AuthSession>('/auth/me', signal)
}

export async function changePassword(
  currentPassword: string,
  newPassword: string,
): Promise<void> {
  const response = await apiFetch('/auth/password', {
    method: 'PUT',
    body: JSON.stringify({ currentPassword, newPassword }),
  })
  await throwUnlessOk(response)
}

export function getAdminUsers(signal?: AbortSignal): Promise<ManagedUser[]> {
  return getJson<ManagedUser[]>('/admin/users', signal)
}

export async function createUser(payload: {
  username: string
  password: string
  role: NavRole
}): Promise<ManagedUser> {
  const response = await apiFetch('/admin/users', {
    method: 'POST',
    body: JSON.stringify(payload),
  })
  await throwUnlessOk(response)
  return response.json() as Promise<ManagedUser>
}

export async function updateUser(
  userId: number,
  patch: { role?: NavRole; password?: string },
): Promise<void> {
  const response = await apiFetch('/admin/users/' + userId, {
    method: 'PUT',
    body: JSON.stringify(patch),
  })
  await throwUnlessOk(response)
}

export async function deleteUser(userId: number): Promise<void> {
  const response = await apiFetch('/admin/users/' + userId, {
    method: 'DELETE',
  })
  await throwUnlessOk(response)
}

export async function setUserStrategies(
  userId: number,
  strategySlugs: string[],
): Promise<void> {
  const response = await apiFetch('/admin/users/' + userId + '/strategies', {
    method: 'PUT',
    body: JSON.stringify({ strategySlugs }),
  })
  await throwUnlessOk(response)
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
  const response = await apiFetch(
    '/strategies/' + encodeURIComponent(slug) + '/initial-snapshot',
    {
      method: 'PUT',
      body: JSON.stringify({ snapshotTsMs }),
    },
  )
  await throwUnlessOk(response)
  return response.json() as Promise<StrategySnapshotSummary>
}

export async function clearInitialSnapshot(slug: string): Promise<void> {
  const response = await apiFetch(
    '/strategies/' + encodeURIComponent(slug) + '/initial-snapshot',
    {
      method: 'DELETE',
    },
  )
  await throwUnlessOk(response)
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
  const response = await apiFetch(
    '/fee-rates/' + encodeURIComponent(slug) + '/sync',
    { method: 'POST' },
  )
  await throwUnlessOk(response)
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

export interface IntraHourlyLatencyQuery {
  startMs?: number
  endMs?: number
  signal?: AbortSignal
}

export function getIntraHourlyLatency(
  slug: string,
  query: IntraHourlyLatencyQuery = {},
): Promise<HourlyLatencySeries> {
  const params = new URLSearchParams()
  if (query.startMs != null) params.set('startMs', String(query.startMs))
  if (query.endMs != null) params.set('endMs', String(query.endMs))
  const suffix = params.size > 0 ? '?' + params.toString() : ''
  return getJson<HourlyLatencySeries>(
    '/analysis/' + encodeURIComponent(slug) + '/hourly-latency' + suffix,
    query.signal,
  )
}
