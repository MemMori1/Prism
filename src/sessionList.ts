/**
 * 会话列表的排序 / 时间范围筛选（归档侧与搜索侧共用的纯逻辑）。
 */

import type { SessionSummary } from './types'

export type SessionSort = 'recent' | 'created' | 'tokens'
export type TimeRange = 'all' | 'today' | 'week' | 'month'

/** 排序（返回新数组，不改动入参）。 */
export function sortSessions(sessions: SessionSummary[], sort: SessionSort): SessionSummary[] {
  const copy = [...sessions]
  copy.sort((a, b) => {
    if (sort === 'created') return b.timeCreated - a.timeCreated
    if (sort === 'tokens') {
      const left = a.tokensInput + a.tokensOutput + a.tokensReasoning
      const right = b.tokensInput + b.tokensOutput + b.tokensReasoning
      return right - left
    }
    return b.timeUpdated - a.timeUpdated
  })
  return copy
}

/** 时间范围的起点（本地时区；一周从周一算起）。all → null。 */
export function rangeToSince(range: TimeRange, now: number): number | null {
  const start = new Date(now)
  switch (range) {
    case 'today':
      start.setHours(0, 0, 0, 0)
      return start.getTime()
    case 'week': {
      start.setHours(0, 0, 0, 0)
      const weekday = (start.getDay() + 6) % 7
      start.setDate(start.getDate() - weekday)
      return start.getTime()
    }
    case 'month':
      start.setHours(0, 0, 0, 0)
      start.setDate(1)
      return start.getTime()
    default:
      return null
  }
}

/** 按更新时间下限过滤会话（session.timeUpdated >= since）。 */
export function filterSessionsByRange(
  sessions: SessionSummary[],
  since: number | null,
): SessionSummary[] {
  if (since === null) return sessions
  return sessions.filter(session => session.timeUpdated >= since)
}
