import type { Strategy } from './types'

const API_BASE = '/nav-api'

async function getJson<T>(path: string): Promise<T> {
  const response = await fetch(API_BASE + path, {
    headers: { Accept: 'application/json' },
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

