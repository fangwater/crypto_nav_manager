export interface AnalysisMetricHelpItem {
  id: string
  title: string
  formula: string
  meaning: string
}

export const ANALYSIS_METRIC_HELP: readonly AnalysisMetricHelpItem[] = [
  {
    id: 'fee-mode-pnl',
    title: '当前口径收益',
    formula:
      'Fee 前 = 交易价差 + 闭环 Funding − 闭环 Interest；实际 Fee 后再减窗口成交费；参考 Fee 后按参考 bps × 闭环四腿本金扣费',
    meaning:
            '页面顶部主数字随“收益口径”切换。只有开、平都在选定区间内的正反配对才计算；区间外开仓或未配对单边不进这个数。',
  },
  {
    id: 'fee-impact',
    title: 'Fee 影响',
    formula:
      '实际 Fee 影响 = −窗口成交费；参考 Fee 影响 = −参考 bps × 闭环四腿本金；Fee 前口径下为 0',
    meaning:
      '显示当前口径相对“交易价差 + Funding − Interest”多扣了多少手续费。实际覆盖率看窗口成交里能换成 USDT 的名义金额占比。',
  },
  {
    id: 'closed-funding',
    title: '闭环 Funding',
    formula:
      '资金费事件按时点摊到当时仍开放的合约名义本金上；FIFO 平掉多少数量，就按比例释放多少已摊资金费',
    meaning:
      '不是账户资金费流水合计。只有跟着已闭环数量一起释放出来的那部分资金费才计入研究收益；没有开仓可摊的事件会被丢掉。',
  },
  {
    id: 'closed-interest',
    title: '闭环 Interest',
    formula:
      'USDT/USDC 利息按当时正套现货名义本金分摊；币种利息按当时反套现货名义本金分摊；FIFO 平掉多少数量就释放多少已摊利息',
    meaning:
      '页面上的闭环 Interest 是成本，显示为负数。无法换成 USDT 的利息只记覆盖缺口，不改收益。',
  },
  {
    id: 'fee-capture',
    title: '过费兑现',
    formula:
      'A 过费 = 扣费后收益 > 0 的闭环名义 / 全部闭环名义；B 不够 = 成交价差为正但扣费后 ≤ 0；C 没兑现 = 成交价差 ≤ 0。Fee 前口径下 A 为价差为正的名义占比',
    meaning:
      '看有多少名义真正盖过了手续费。全量 FIFO 分桶，不靠页面上最近闭环样本。Binance intra 实际费含现货 maker −0.4 bps 补丁。',
  },
  {
    id: 'fill-basis',
    title: '成交基差',
    formula: '成交基差 = 合约成交价 − 现货成交价；明细表按笔展示开仓腿 → 平仓腿',
    meaning:
      '每一笔都是开、平都在选定区间内的正反闭环。主数字是这些配对合在一起的成交价差，待配对单边不参与。',
  },
  {
    id: 'fifo-closed',
    title: 'FIFO 闭环',
    formula:
      '按币种、只在选定区间内把相反方向数量先进先出配成正反闭环；开仓腿或平仓腿落在区间外的不配对、不计入收益',
    meaning:
      'FIFO 用来完整评估一轮正反，但开、平必须都在你选的时间范围内。更早开、今天才平的不算进今日收益，区间内对不上的单边挂在待配对。',
  },
  {
    id: 'unpaired',
    title: '待配对',
    formula: '选定区间结束时仍未对上反向腿的剩余数量，按开仓现货价计名义',
    meaning:
      '包括区间内开了还没平的库存，以及区间内出现但对不上同区间反向腿的单边。这些不计算收益、胜率和过费分桶。',
  },
  {
    id: 'margin-new-create',
    title: 'Margin NEW − create',
    formula:
      '对 Margin venue、status=NEW 且 create_ts>0 的行：max(0, update_ts − create_ts)，再按小时窗口取 p50/p90；分位数只用不大于 100 ms 的非负样本',
    meaning:
      '现货开单从本地创建请求到交易所 NEW 回报的挂单时延。小时任务单独落库，不进 FIFO 收益图。',
  },
  {
    id: 'futures-new-create',
    title: 'Futures NEW − create',
    formula:
      '对 Futures venue、status=NEW 且 create_ts>0 的行：max(0, update_ts − create_ts)，再按小时窗口取 p50/p90；同样截断在 100 ms',
    meaning: '合约对冲单的挂单时延，口径与现货 NEW−create 对称。',
  },
  {
    id: 'spot-trigger-latency',
    title: '现货信号行情延迟',
    formula:
      '当 signal_open_ts 与 signal_hedge_ts 都大于 0 且现货腿更晚：signal_ts − signal_open_ts；按小时取 p50/p90，截断 100 ms',
    meaning:
      '由现货 BBO 触发的开仓，从交易所行情事件到信号的时延。含时钟偏差和网络，不是纯内部计算。',
  },
  {
    id: 'futures-trigger-latency',
    title: '合约信号行情延迟',
    formula:
      '当两腿 BBO 都有效且合约腿更晚：signal_ts − signal_hedge_ts；按小时取 p50/p90，截断 100 ms',
    meaning:
      '由合约 BBO 触发的开仓对应的行情到信号时延。平局或缺少 dual-BBO 的行不进入现货/合约任一系列。',
  },
]

export function analysisMetricHelpById(id: string): AnalysisMetricHelpItem | undefined {
  return ANALYSIS_METRIC_HELP.find((item) => item.id === id)
}

export function analysisMetricHelpForStrategy(slug: string): AnalysisMetricHelpItem[] {
  const includeCarry = slug !== 'binance-intra-arb01'
  return ANALYSIS_METRIC_HELP.flatMap((item) => {
    if (!includeCarry && (item.id === 'closed-funding' || item.id === 'closed-interest')) {
      return []
    }
    if (!includeCarry && item.id === 'fee-mode-pnl') {
      return [
        {
          ...item,
          formula:
            'Fee 前 = 交易价差；实际 Fee 后再减窗口成交费；参考 Fee 后按参考 bps × 闭环四腿本金扣费',
          meaning:
            '页面顶部主数字随“收益口径”切换。只有开、平都在选定区间内的正反配对才计算；区间外开仓或未配对单边不进这个数。Binance intra 研究口径不含闭环 Funding / Interest。实际 Fee 对现货 maker 按 −0.4 bps 补丁，与账户 PnL 相同。',
        },
      ]
    }
    if (!includeCarry && item.id === 'fee-impact') {
      return [
        {
          ...item,
          meaning:
            '显示当前口径相对交易价差多扣了多少手续费。Binance intra 现货 maker 按 −0.4 bps 补丁计入（成交表记 0，下一小时 MM2 返佣），与账户 PnL 同一口径。实际覆盖率看窗口成交里能换成 USDT 的名义金额占比。',
        },
      ]
    }
    return [item]
  })
}

export function toggleAnalysisHelperOpen(current: boolean): boolean {
  return !current
}
