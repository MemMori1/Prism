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

/**
 * 「空会话」：建出来之后一次都没真正跑过。
 *
 * 判据只看 token 与成本——列表里用户能看见的就是这两个数字，看到 `↓0 ↑0`
 * 的那些行，点「关闭空会话」时预期关掉的就是它们。不猜标题（引擎会自己
 * 起名，新建就有标题），也不去读时间线（那是几十次 IO，为了一个开关不值）。
 */
export function isBlankSession(session: SessionSummary): boolean {
  return (
    session.tokensInput <= 0 &&
    session.tokensOutput <= 0 &&
    session.tokensReasoning <= 0 &&
    session.cost <= 0
  )
}

/** 从会话列表里摘出空会话的 id（已关闭的、加过星标的不算）。 */
export function blankSessionIds(
  sessions: SessionSummary[],
  isHidden: (id: string) => boolean,
  isStarred: (id: string) => boolean,
): string[] {
  return sessions
    .filter(session => isBlankSession(session))
    // 星标是「我要留着」的明确表态，不参与折叠；已关闭的不必再关一次。
    .filter(session => !isHidden(session.id) && !isStarred(session.id))
    .map(session => session.id)
}
