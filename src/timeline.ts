/** 时间线的展示派生：每轮用时、可复制正文。纯函数，便于单测。 */

import type { TimelinePart, TimelineRecord } from './types'

/**
 * 一条记录「本轮用时」的上限：超过就认为中间隔着离开/挂机，
 * 不当作耗时的展示（避免冒出「3 天」这种噪声）。
 */
export const TURN_DURATION_MAX_MS = 24 * 60 * 60 * 1000

/**
 * 每条记录的耗时＝与上一条记录的时间差。
 *
 * 引擎记的是「这条记录落库的时刻」：单轮对话（用户→助手）算出来就是这一轮
 * 真正花的时间；多轮带工具时，是相邻两段之间的间隔。缺时间、非正数、
 * 超上限一律不给（宁可不显示，也不给个假数）。
 */
export function turnDurations(records: TimelineRecord[]): Map<string, number> {
  const result = new Map<string, number>()
  let previous = 0
  for (const record of records) {
    const current = record.timeCreated ?? 0
    if (previous > 0 && current > previous) {
      const delta = current - previous
      if (delta <= TURN_DURATION_MAX_MS) result.set(record.id, delta)
    }
    if (current > 0) previous = current
  }
  return result
}

/** 一轮里可复制的正文：只要正文，不带思考过程与工具调用。 */
export function recordText(record: TimelineRecord): string {
  return record.parts
    .filter((part): part is Extract<TimelinePart, { kind: 'text' }> => part.kind === 'text')
    .map(part => part.text.trim())
    .filter(Boolean)
    .join('\n\n')
}
