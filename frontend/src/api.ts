import type { Strategy, StrategyPnl } from './types'

const API_BASE = '/nav-api'

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

export function getStrategies(): Promise<Strategy[]> {
  return getJson<Strategy[]>('/strategies')
}

export function getStrategy(slug: string): Promise<Strategy> {
  return getJson<Strategy>('/strategies/' + encodeURIComponent(slug))
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

