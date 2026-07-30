import type { AlignmentStatus } from './types'

const formatter = new Intl.DateTimeFormat(undefined, {
  month: '2-digit',
  day: '2-digit',
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
  hour12: false,
})

const phaseLabels: Record<string, string> = {
  preparing: '准备校对',
  loading_watermark: '读取水位',
  syncing_trades: '刷新成交',
  exporting_pg: '导出 PG 成交',
  exporting_orders: '导出中心订单',
  comparing: '比对数量',
  complete: '校对完成',
}

export function alignmentTone(status: AlignmentStatus) {
  if (status.state === 'running') return 'running'
  if (status.state === 'succeeded') return 'succeeded'
  if (status.state === 'mismatch') return 'mismatch'
  if (status.state === 'failed') return 'failed'
  return 'waiting'
}

export function alignmentLabel(status: AlignmentStatus) {
  if (status.state === 'running') {
    return `${phaseLabels[status.phase] ?? status.phase} ${status.progressPercent}%`
  }
  if (status.state === 'succeeded') return '已对齐'
  if (status.state === 'mismatch') {
    return `${status.mismatchCount ?? 0} 组差异`
  }
  if (status.state === 'failed') return '校对失败'
  return '等待首次校对'
}

export function alignmentTime(status: AlignmentStatus) {
  if (status.actualEndMs === null) return '--'
  return formatter.format(status.actualEndMs)
}

export function alignmentTitle(status: AlignmentStatus) {
  const lines = [alignmentLabel(status)]
  if (status.message) lines.push(status.message)
  if (status.pgEventCount !== null && status.localEventCount !== null) {
    lines.push(
      `PG execution: ${status.pgEventCount}; 中心回报: ${status.localEventCount}`,
    )
  }
  return lines.join('\n')
}
