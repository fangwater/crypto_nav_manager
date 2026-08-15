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
      '页面顶部主数字随“收益口径”切换。只统计 FIFO 已经对上开平仓的数量，未闭环仓位不进这个数。',
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
    id: 'market-basis',
    title: '选币基差收益',
    formula:
      '开平仓各取同分钟 premium-index close 作为市场基差；收益 = 数量 × 开仓方向 × (开仓市场基差 − 平仓市场基差)',
    meaning:
      '衡量“选对了这个币的基差方向”带来的钱，尽量剥离成交滑点。缺 premium 的闭环不进这项。',
  },
  {
    id: 'closed-execution',
    title: '闭环两腿执行',
    formula:
      '开仓执行 = 数量 × 开仓方向 × (成交基差 − 开仓分钟市场基差)；平仓同理；两项相加',
    meaning:
      '成交价相对当时市场基差多赚或少赚的部分。选币基差 + 两腿执行 ≈ 交易价差，前提是开平仓都有 premium。',
  },
  {
    id: 'premium-coverage',
    title: 'Premium 覆盖',
    formula: '同时具备开平仓 premium 的闭环次数 / 全部 FIFO 闭环次数',
    meaning: '选币基差和执行拆解能覆盖多少闭环。覆盖不足时那两项会小于交易价差。',
  },
  {
    id: 'fifo-closed',
    title: 'FIFO 闭环',
    formula: '按币种、相反方向把开仓数量与后续平仓数量先进先出配对；胜率 = 交易价差为正的闭环次数 / 总闭环次数',
    meaning:
      '研究用的开平配对，不是交易所成交笔数。持仓中的数量要等对上反向腿才进入收益。',
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

export function toggleAnalysisHelperOpen(current: boolean): boolean {
  return !current
}
