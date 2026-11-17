import type { FrCostRow } from '../frEntryCost'
import { adverseEntryBasis, COST_REVIEW_RULES, costMetric, costMetricCoverage, rankCostConcerns } from '../frCostScreen'
import type { CoveredCostMetric } from '../frCostScreen'

const fmt = (n: number | null) => n === null ? '未知' : n.toLocaleString('en-US', { maximumFractionDigits: 2 })
const pct = (n: number | null) => n === null ? '未知' : `${fmt(n)}%`

export function FrCostShortlist({ rows, onSelect }: { rows: FrCostRow[]; onSelect: (symbol: string) => void }) {
  const concerns = rankCostConcerns(rows)
  const insufficient = rows.filter(row => (costMetricCoverage(row, 'costBps') ?? 0) < COST_REVIEW_RULES.minimumCoveragePct).length
  function metric(row: FrCostRow, key: CoveredCostMetric, label: string) {
    const value = costMetric(row, key), coverage = costMetricCoverage(row, key)
    const covered = row.covered[key === 'feeBps' ? 'fee' : key === 'costBps' ? 'cost' : 'deviation']
    return <div className="fr-cost-review-metric">
      <span>{label}</span><strong className={value !== null && value > 0 ? 'fr-cost-adverse' : ''}>{fmt(value)}{value !== null && <em> bps</em>}</strong>
      <small>{value === null ? '数据待补齐' : `覆盖 ${pct(coverage)}${coverage !== null && coverage < COST_REVIEW_RULES.minimumCoveragePct ? ' · 不参与排名' : ''}`}</small>
      {key === 'costBps' && value !== null && <small>{fmt(costMetric(row, 'cost'))} USDT{coverage !== null && coverage < 99.9999 ? ' · 仅覆盖部分' : ''}</small>}
      {key !== 'costBps' && value !== null && covered.quantityCoveragePct !== null && covered.quantityCoveragePct < 99.9999 && <small>仅覆盖部分</small>}
    </div>
  }
  return <div className="fr-cost-shortlist">
    <p className="fr-cost-review-summary">{rows.length ? `当前 ${rows.length} 个持仓中，${concerns.length} 个触发检查条件，展示前 ${Math.min(COST_REVIEW_RULES.limit, concerns.length)} 个。` : '当前没有 FR 持仓。'}
      {!!insufficient && ` ${insufficient} 个持仓的成本数据不足，不代表成本正常。`}</p>
    <ol className="fr-cost-review-list" aria-label="优先检查的持仓">
      {concerns.slice(0, COST_REVIEW_RULES.limit).map(({ row, reasons }, index) => {
        const basis = adverseEntryBasis(row)
        return <li key={row.symbol} data-symbol={row.symbol} className="fr-cost-review-row">
          <div className="fr-cost-review-identity">
            <div><span className="fr-cost-review-rank">{index + 1}</span><button className="fr-cost-symbol" onClick={() => onSelect(row.symbol)} aria-label={`查看 ${row.symbol} 建仓明细`}>{row.symbol} ↗</button></div>
            <small>{row.direction === 'positive' ? '正套' : row.direction === 'reverse' ? '反套' : '方向未知'} · 配对本金 {fmt(costMetric(row, 'notional'))} USDT</small>
            <div className="fr-cost-review-reasons">{reasons.map(reason => <span key={reason}>{reason}</span>)}</div>
          </div>
          {metric(row, 'costBps', '建仓成本估算')}
          {metric(row, 'deviation', '执行偏差估算')}
          {metric(row, 'feeBps', '手续费')}
          <div className="fr-cost-review-metric"><span>Taker · 现货 / 合约</span><strong>{pct(row.pairedLiquidity.spot.takerNotionalPct)} / {pct(row.pairedLiquidity.futures.takerNotionalPct)}</strong><small>按各腿成交额占比</small></div>
          <div className="fr-cost-review-metric"><span>入场基差</span><strong>{fmt(costMetric(row, 'basis'))}{row.fillSpreadBps !== null && <em> bps</em>}</strong><small>{basis === null ? '方向或价格未知' : basis > 0 ? '方向不利' : basis < 0 ? '方向有利' : '零基差'}</small></div>
        </li>
      })}
    </ol>
    {!!rows.length && !concerns.length && <p className="fr-cost-review-empty">已知数据暂未触发检查条件。缺失或覆盖不足的指标不作正常判断，可展开完整表格核对。</p>}
    <details className="fr-cost-review-rules"><summary>入选与排序规则</summary>
      <p>先看执行偏差、建仓成本或手续费 ≥ {COST_REVIEW_RULES.costBps} bps 的持仓，按三项中已达阈值的最大 bps 降序；再看任一腿 taker ≥ {COST_REVIEW_RULES.takerPct}%，按比例降序；最后看方向不利的入场基差 ≥ {COST_REVIEW_RULES.adverseBasisBps} bps，按幅度降序。同值按币种排列，最多展示 {COST_REVIEW_RULES.limit} 个，不凑数。</p>
      <p>费用与执行指标至少覆盖 {COST_REVIEW_RULES.minimumCoveragePct}% 配对数量才参与排名；这只是检查门槛，部分覆盖仍不能代表整仓。正套的负基差、反套的正基差为方向不利；有利的大基差不单独触发检查。执行偏差是相对 premium 的估算，不是精确滑点。</p>
      <p>这里使用固定检查条件；展开后的完整表格支持自定义阈值和排序，不影响此列表。未入选不等于建仓正常。</p>
    </details>
  </div>
}
