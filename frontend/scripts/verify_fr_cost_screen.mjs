import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import ts from 'typescript'

const source = readFileSync(new URL('../src/frCostScreen.ts', import.meta.url), 'utf8')
const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS } })
const module = { exports: {} }
new Function('module', 'exports', outputText)(module, module.exports)
const { EMPTY_THRESHOLDS, matchesCostThresholds: match, compareCostRows: compare, costMetric, numberInput } = module.exports

function row(symbol, taker, basis, deviation, fee, cost, unknown = 0) {
  const leg = { takerNotionalPct: taker, makerNotionalPct: taker === null ? null : 100 - taker - unknown, unknownNotionalPct: unknown }
  return {
    symbol, direction: 'positive', fillSpreadBps: basis, executionDeviationBps: deviation, lastPairMs: 60000,
    pairedLiquidity: { spot: leg, futures: leg },
    summary: { pairCount: 2, notionalUsdt: 10000, feeUsdt: fee, estimatedCostUsdt: cost, estimatedCostBps: cost },
  }
}
const highTaker = row('AAAUSDT', 50, 5, 2, 3, 5)
const adverse = row('BBBUSDT', 5, -80, 20, 3, 23)
const favorable = row('CCCUSDT', 2, 80, -20, -1, -21)
const unknown = row('DDDUSDT', null, null, null, null, null)
const filters = { ...EMPTY_THRESHOLDS, taker: '30', deviation: '10', basisAbs: '50' }
assert.equal(match(highTaker, filters, 'any'), true)
assert.equal(match(highTaker, filters, 'all'), false)
assert.equal(match(adverse, filters, 'any'), true)
assert.equal(match(favorable, { ...EMPTY_THRESHOLDS, deviation: '10' }, 'any'), false, 'favorable execution is not adverse slippage')
assert.equal(match(favorable, { ...EMPTY_THRESHOLDS, basisAbs: '50' }, 'any'), true)
assert.equal(match(adverse, { ...EMPTY_THRESHOLDS, basisAbs: '50' }, 'any'), true, 'negative entry basis retains magnitude')
assert.equal(costMetric(favorable, 'feeBps'), -1, 'rebate stays negative')
assert.equal(costMetric(adverse, 'basis'), -80, 'displayed basis keeps its sign')
assert.equal(costMetric(unknown, 'feeBps'), null)
assert.equal(match(unknown, filters, 'any'), null, 'missing observations do not mean normal')
assert.equal(match(unknown, EMPTY_THRESHOLDS, 'all'), true, 'empty thresholds impose no condition')
const partial = row('EEEUSDT', 10, 2, null, 2, null, 40)
assert.equal(match(partial, { ...EMPTY_THRESHOLDS, taker: '30' }, 'any'), null, 'unknown roles might cross taker threshold')
assert.equal(match(partial, { ...EMPTY_THRESHOLDS, taker: '10' }, 'any'), true, 'known taker lower bound suffices at inclusive boundary')
assert.equal(match(partial, { ...EMPTY_THRESHOLDS, taker: '80' }, 'any'), false, 'even all unknown roles cannot cross threshold')
assert.equal(match(partial, { ...EMPTY_THRESHOLDS, taker: '80', deviation: '10' }, 'all'), false)
assert.equal(match(partial, { ...EMPTY_THRESHOLDS, taker: '80', deviation: '10' }, 'any'), null)
const noPair = { ...highTaker, summary: { ...highTaker.summary, pairCount: 0 } }
assert.equal(costMetric(noPair, 'cost'), null, 'closed/no-pair inventory is not a zero-cost position')
assert.equal(match(noPair, { ...EMPTY_THRESHOLDS, taker: '0' }, 'any'), null)
assert.equal(match({ ...highTaker, summary: { ...highTaker.summary, notionalUsdt: null } }, EMPTY_THRESHOLDS, 'any', 100), null, 'minimum notional cannot silently pass missing prices')
assert.equal(match(highTaker, filters, 'any', 20000), false, 'minimum position size remains an AND constraint')
for (const ascending of [true, false]) {
  const sorted = [unknown, adverse, highTaker, favorable].sort((a, b) => compare(a, b, 'costBps', ascending))
  assert.equal(sorted.at(-1).symbol, unknown.symbol, 'unknown always sorts last')
  assert.equal(sorted[0].symbol, ascending ? favorable.symbol : adverse.symbol)
}
assert.equal(numberInput(''), null)
assert.equal(numberInput('Infinity'), null)
assert.equal(numberInput('0'), 0)
assert.equal(numberInput('-2.5'), -2.5)
const coveredRow = { ...unknown, summary: { ...unknown.summary, notionalUsdt: 10000 }, covered: {
  fee: { valueUsdt: 1, bps: 10, quantityCoveragePct: 10 },
  deviation: { valueUsdt: 2, bps: 20, quantityCoveragePct: 10 },
  cost: { valueUsdt: 3, bps: 30, quantityCoveragePct: 10 },
} }
assert.equal(costMetric(coveredRow, 'costBps'), 30, 'partial bps uses the covered notional, not full position')
assert.equal(costMetric(coveredRow, 'cost'), 3, 'partial amount must not be extrapolated to whole position')
assert.equal(match(coveredRow, { ...EMPTY_THRESHOLDS, deviation: '15' }, 'any'), true)
console.log('FR screen: inclusive thresholds, any/all, signed and absolute basis, rebates, unknown roles, missing values and bidirectional sorting passed.')
