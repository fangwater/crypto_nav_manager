import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import ts from 'typescript'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')
const require = createRequire(join(root, 'package.json'))

function loadTs(relativePath) {
  const source = readFileSync(join(root, relativePath), 'utf8')
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
      esModuleInterop: true,
    },
    fileName: relativePath,
  })
  const module = { exports: {} }
  const fn = new Function('require', 'module', 'exports', outputText)
  fn(require, module, module.exports)
  return module.exports
}

function read(relativePath) {
  return readFileSync(join(root, relativePath), 'utf8')
}

export function verifyHelper() {
  const help = loadTs('src/analysisMetricHelp.ts')
  const titles = help.ANALYSIS_METRIC_HELP.map((item) => item.title)
  for (const required of [
    '闭环 Funding',
    '闭环 Interest',
    '当前口径收益',
    'Fee 影响',
    '选币基差收益',
    '闭环两腿执行',
    'Premium 覆盖',
    'FIFO 闭环',
    'Margin NEW − create',
    'Futures NEW − create',
    '现货信号行情延迟',
    '合约信号行情延迟',
  ]) {
    assert.ok(titles.includes(required), `missing helper copy for ${required}`)
  }
  const funding = help.analysisMetricHelpById('closed-funding')
  const interest = help.analysisMetricHelpById('closed-interest')
  assert.match(funding.formula, /FIFO/)
  assert.match(funding.meaning, /已闭环数量/)
  assert.match(interest.formula, /正套现货名义本金/)
  assert.match(interest.meaning, /成本/)

  let open = false
  open = help.toggleAnalysisHelperOpen(open)
  assert.equal(open, true)
  open = help.toggleAnalysisHelperOpen(open)
  assert.equal(open, false)

  const helperSource = read('src/components/AnalysisMetricHelper.tsx')
  assert.match(helperSource, /useState\(false\)/)
  assert.match(helperSource, /aria-expanded=\{open\}/)
  assert.match(helperSource, /\{open && \(/)
  assert.match(helperSource, /ANALYSIS_METRIC_HELP/)

  const page = read('src/pages/IntraAnalysisPage.tsx')
  assert.match(page, /<AnalysisMetricHelper \/>/)
  return { helperItems: titles.length, collapsedByDefault: true }
}

export function verifyNavigation() {
  const nav = loadTs('src/analysisNav.ts')
  for (const slug of ['binance-intra-arb01', 'bybit-intra-arb01']) {
    const link = nav.strategySurfaceAnalysisLink(slug)
    assert.equal(link.to, `/analysis/${slug}`)
    const page = nav.analysisPageTarget(slug)
    assert.equal(page.route, `/analysis/${slug}`)
    assert.equal(page.strategySlug, slug)
    assert.equal(page.rendersAnalysisPage, true)
  }
  assert.equal(nav.strategySurfaceAnalysisLink('bybit-intra-arb02'), null)

  const app = read('src/App.tsx')
  assert.match(app, /path="\/analysis\/:slug"/)
  assert.match(app, /<IntraAnalysisPage \/>/)

  const strategyPage = read('src/pages/PnlStrategyPage.tsx')
  assert.match(strategyPage, /strategySurfaceAnalysisLink\(strategy.slug\)/)
  assert.match(strategyPage, /to=\{analysisLink.to\}/)

  const matchingPage = read('src/pages/IntraMatchingPage.tsx')
  assert.match(matchingPage, /strategySurfaceAnalysisLink\(selected.strategySlug\)/)

  const indexPage = read('src/pages/IndexPage.tsx')
  assert.match(indexPage, /strategySurfaceAnalysisLink\(strategy.slug\)/)
  return { routes: ['/analysis/binance-intra-arb01', '/analysis/bybit-intra-arb01'] }
}

export function verifyLatencyChart() {
  const chart = loadTs('src/analysisLatencyChart.ts')
  const series = {
    strategySlug: 'binance-intra-arb01',
    points: [
      {
        strategySlug: 'binance-intra-arb01',
        windowStartMs: 1_000,
        windowEndMs: 3_601_000,
        computedAtMs: 3_601_000,
        marginNewCreate: { sampleCount: 2, normalCount: 2, p50Ms: 1.1, p90Ms: 1.8 },
        futuresNewCreate: { sampleCount: 1, normalCount: 1, p50Ms: 0.8, p90Ms: 0.9 },
        spotTrigger: { sampleCount: 2, normalCount: 2, p50Ms: 0.4, p90Ms: 0.6 },
        futuresTrigger: { sampleCount: 1, normalCount: 1, p50Ms: 1.2, p90Ms: 1.3 },
      },
    ],
  }
  const model = chart.bindHourlyLatencyChart(series)
  assert.equal(model.chartId, 'hourly-latency')
  assert.equal(model.distinctFrom, 'fifo-closed-pnl')
  assert.ok(chart.latencyChartHasRequiredSeries(model))
  const names = model.series.map((line) => line.name)
  assert.deepEqual(names, [
    'Margin NEW−create p50',
    'Margin NEW−create p90',
    'Futures NEW−create p50',
    'Futures NEW−create p90',
    '现货信号 p50',
    '现货信号 p90',
    '合约信号 p50',
    '合约信号 p90',
  ])
  assert.deepEqual(model.series[0].values, [1.1])
  assert.deepEqual(model.series[7].values, [1.3])

  const page = read('src/pages/IntraAnalysisPage.tsx')
  assert.match(page, /data-chart-id="fifo-closed-pnl"/)
  assert.match(page, /data-chart-id="hourly-latency"/)
  assert.match(page, /<IntraLatencyChart series=\{latencySeries\} \/>/)
  return { seriesCount: model.series.length, distinctFromFifo: true }
}

const mode = process.argv[2]
if (mode === 'helper') {
  console.log(JSON.stringify(verifyHelper(), null, 2))
} else if (mode === 'nav') {
  console.log(JSON.stringify(verifyNavigation(), null, 2))
} else if (mode === 'chart') {
  console.log(JSON.stringify(verifyLatencyChart(), null, 2))
} else if (import.meta.url === `file://${process.argv[1]}`) {
  console.log(JSON.stringify({
    helper: verifyHelper(),
    nav: verifyNavigation(),
    chart: verifyLatencyChart(),
  }, null, 2))
}
