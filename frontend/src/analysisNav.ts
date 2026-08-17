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
