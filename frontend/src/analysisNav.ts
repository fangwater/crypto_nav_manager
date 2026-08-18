export const INTRA_ANALYSIS_SLUGS = [
  'binance-intra-arb01',
  'bybit-intra-arb01',
] as const

export type IntraAnalysisSlug = (typeof INTRA_ANALYSIS_SLUGS)[number]

export function isIntraAnalysisSlug(slug: string): slug is IntraAnalysisSlug {
  return (INTRA_ANALYSIS_SLUGS as readonly string[]).includes(slug)
}

export function intraAnalysisIncludesClosedCarry(slug: string) {
  return slug !== 'binance-intra-arb01'
}

export function intraAnalysisPath(slug: string): string | null {
  return isIntraAnalysisSlug(slug) ? `/analysis/${slug}` : null
}

export function intraAnalysisHref(slug: string): string {
  const path = intraAnalysisPath(slug)
  if (!path) {
    throw new Error(`strategy ${slug} has no analysis page`)
  }
  return path
}

export function suggestIntraAnalysisSlug(slug: string): IntraAnalysisSlug | null {
  if (isIntraAnalysisSlug(slug)) return null
  const needle = compactSlug(slug)
  if (!needle) return null
  let best: IntraAnalysisSlug | null = null
  let bestDistance = Number.POSITIVE_INFINITY
  for (const candidate of INTRA_ANALYSIS_SLUGS) {
    const distance = levenshtein(needle, compactSlug(candidate))
    if (distance > 0 && distance <= 2 && distance < bestDistance) {
      best = candidate
      bestDistance = distance
    }
  }
  return best
}

function compactSlug(value: string) {
  return value.toLowerCase().replace(/[^a-z0-9]/g, '')
}

function levenshtein(left: string, right: string) {
  const rows = left.length + 1
  const cols = right.length + 1
  const distances = Array.from({ length: rows }, (_, row) => {
    const line = new Array<number>(cols).fill(0)
    line[0] = row
    return line
  })
  for (let col = 0; col < cols; col += 1) distances[0][col] = col
  for (let row = 1; row < rows; row += 1) {
    for (let col = 1; col < cols; col += 1) {
      const cost = left[row - 1] === right[col - 1] ? 0 : 1
      distances[row][col] = Math.min(
        distances[row - 1][col] + 1,
        distances[row][col - 1] + 1,
        distances[row - 1][col - 1] + cost,
      )
    }
  }
  return distances[left.length][right.length]
}

export function strategySurfaceAnalysisLink(slug: string): {
  to: string
  label: string
} | null {
  const to = intraAnalysisPath(slug)
  return to ? { to, label: '组合分析' } : null
}

export function analysisPageTarget(slug: string): {
  route: string
  strategySlug: string
  rendersAnalysisPage: boolean
} {
  return {
    route: `/analysis/${slug}`,
    strategySlug: slug,
    rendersAnalysisPage: isIntraAnalysisSlug(slug),
  }
}
