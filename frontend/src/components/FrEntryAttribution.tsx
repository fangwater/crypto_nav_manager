import { useState } from 'react'
import type { FrEntryAnalysis, LiquidityMix } from '../frEntryCost'
import { FrBasisChart } from './FrCostChart'

const fmt = (n: number | null, digits = 4) => n === null ? '未知' : n.toLocaleString('en-US', { maximumFractionDigits: digits })
const time = (n: number | null) => n === null ? '未知' : new Date(n).toLocaleString('zh-CN', { hour12: false })
const change = (n: number | null) => n === null ? '未知' : `${n > 0 ? '+' : ''}${fmt(n)} bps`

function LiquidityTable({ mix }: { mix: LiquidityMix }) {
  return <div className="fr-cost-table-wrap"><table>
    <thead><tr><th>市场 / 成交角色</th><th>成交笔数</th><th>基础币数量</th><th>成交金额 USDT</th><th>金额占比</th><th>手续费 USDT</th></tr></thead>
    <tbody>{(['spot', 'futures'] as const).flatMap(leg => {
      const stats = mix[leg]
      return (['maker', 'taker', 'unknown', 'total'] as const).map(role => {
        const b = stats[role]
        const pct = role === 'maker' ? stats.makerNotionalPct : role === 'taker' ? stats.takerNotionalPct : role === 'unknown' ? stats.unknownNotionalPct : b.notionalUsdt !== null && b.notionalUsdt > 0 ? 100 : null
        return <tr key={`${leg}-${role}`} className={role === 'total' ? 'fr-cost-total' : undefined}>
          <td>{leg === 'spot' ? '现货' : '合约'} · {role === 'unknown' ? '角色未知' : role === 'total' ? '合计' : role}</td>
          <td>{b.fillCount}</td><td>{fmt(b.quantity, 8)}</td><td>{fmt(b.notionalUsdt, 2)}</td><td>{fmt(pct, 2)}%</td>
          <td>{fmt(b.feeUsdt)}{b.unconvertedFeeParts > 0 && <small>含未折算手续费</small>}</td>
        </tr>
      })
    })}</tbody>
  </table></div>
}

export function FrEntryAttribution({ analysis }: { analysis: FrEntryAnalysis }) {
  const [scope, setScope] = useState<'paired' | 'window'>('paired')
  const { basis } = analysis
  const selected = scope === 'window' ? analysis.windowLiquidity : analysis.pairedLiquidity
  const deviation = analysis.executionDeviationBps ?? analysis.covered.deviation.bps
  const assessmentKey = deviation === null ? 'unknown' : deviation > 0.0001 ? 'adverse' : deviation < -0.0001 ? 'favorable' : 'at_reference'
  const assessment = {
    adverse: '成交劣于参考：存在不利执行偏离',
    favorable: '成交优于参考：获得执行优势',
    at_reference: '成交与参考基本一致',
    unknown: '参考覆盖不足，暂不能判断执行偏离',
  }[assessmentKey]
  return <>
    <section className="fr-cost-panel"><div className="fr-cost-table-head"><h2>手续费与 maker/taker 成交结构</h2>
      <select aria-label="成交统计范围" value={scope} onChange={e => setScope(e.target.value as 'paired' | 'window')}>
        <option value="paired">当前 FIFO 配对库存</option><option value="window" disabled={analysis.windowLiquidity === null}>建仓区间全部成交</option>
      </select></div>
      <p>占比按各腿成交金额加权，包含角色未知部分，不按订单笔数推算。负手续费表示返佣。</p>
      {scope === 'paired'
        ? <p>与主成本卡片范围一致：只统计仍持有的配对数量及其分摊手续费；同一成交拆成多个 FIFO 片段仍只记一笔。</p>
        : <p>范围：{time(basis.start?.eventMs ?? null)} → {time(basis.lastPair?.eventMs ?? null)}。包括该区间内已平仓、减仓和再平衡的成交，故不等于当前库存分摊成本。</p>}
      {selected ? <LiquidityTable mix={selected} /> : <p>建仓起点未知，无法统计完整区间。</p>}
    </section>
    <section className="fr-cost-panel"><h2>实际成交价差与执行偏离</h2>
      <div className="fr-cost-alignment">
        <span>现货成交 VWAP <b>{fmt(analysis.spotVwap, 8)}</b></span>
        <span>合约成交 VWAP <b>{fmt(analysis.futuresVwap, 8)}</b></span>
        <span>实际价差 F − S <b>{fmt(analysis.fillSpread, 8)} USDT / 币</b></span>
        <span>实际成交基差 <b>{fmt(analysis.fillSpreadBps)} bps</b></span>
        <span>执行偏离（已调整套利方向）<b>{change(deviation)}</b></span>
      </div>
      <p>执行偏离数量覆盖 {fmt(analysis.covered.deviation.quantityCoveragePct, 2)}%；部分覆盖仅描述已有完整参考的 FIFO 片段。</p>
      <p><strong>{assessment}</strong>。当前参考为 premium index 或手动情景；偏离含口径差异和等待对冲的影响，仅凭它不能断言纯滑点。</p>
    </section>
    <section className="fr-cost-panel"><h2>从开始建仓到现在：基差变化</h2>
      <div className="fr-cost-milestones">
        <article><span>开始：当前配对库存最早成交</span><strong>{time(basis.start?.eventMs ?? null)}</strong><b>{fmt(basis.start?.premiumBps ?? null)} bps</b><small>{basis.start ? `参考分钟 ${time(basis.start.referenceMinuteMs)}` : '含未知期初成本或暂无配对，不能恢复起点'}</small></article>
        <article><span>最后一笔 FIFO 配对完成</span><strong>{time(basis.lastPair?.eventMs ?? null)}</strong><b>{fmt(basis.lastPair?.premiumBps ?? null)} bps</b><small>{basis.lastPair ? `参考分钟 ${time(basis.lastPair.referenceMinuteMs)}` : '暂无已知成交完成的配对'}</small></article>
        <article><span>查询截至时刻</span><strong>{time(basis.asOf.eventMs)}</strong><b>{fmt(basis.asOf.premiumBps)} bps</b><small>最近完整分钟 {time(basis.asOf.referenceMinuteMs)}</small></article>
      </div>
      <div className="fr-cost-milestones fr-cost-changes">
        <article><span>建仓期间：最后配对 − 开始</span><b>{change(basis.buildChangeBps)}</b></article>
        <article><span>配对之后：现在 − 最后配对</span><b>{change(basis.afterPairChangeBps)}</b></article>
        <article><span>全程：现在 − 开始</span><b>{change(basis.totalChangeBps)}</b></article>
      </div>
      <p>采用未调整方向的 premium 基差代理：正变化表示扩大，负变化表示收敛。配对后的基差变化独立于建仓执行偏离，不重复计入滑点或建仓手续费。</p>
      <p>最后配对仅是当前 FIFO 剩余库存中最后一个片段的较晚成交时点，不表示目标仓位已建满，也不表示之后没有减仓。{basis.coveredMinutes.toLocaleString()} / {basis.expectedMinutes.toLocaleString()} 个完整分钟有参考数据。</p>
      {basis.isScenario && <p className="fr-cost-notice">当前使用恒定参考情景；不据此判断真实市场基差“没有变化”。切回自动参考可查看市场变化。</p>}
      {basis.points.length > 0 ? <FrBasisChart basis={basis} fillSpreadBps={analysis.fillSpreadBps} /> : <p>暂缺从建仓起点到查询时刻的参考序列。</p>}
    </section>
  </>
}
