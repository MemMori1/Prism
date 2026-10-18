/**
 * 顶部会话导航的纯逻辑：把各项目的会话合成「最近会话」，以及关键词筛选。
 *
 * 放在这里是为了能单测（组件只管画）。数据由 App 拉好（每项目一次 list_sessions），
 * 这里不碰 IPC。
 */
import type { SessionSummary } from './types'

export interface NavSessionItem {
  session: SessionSummary
  /** 项目显示名：同名会话靠它区分。 */
  projectName: string
}

/** 各项目的一组会话（App 拉完 list_sessions 后交给这里合并）。 */
export interface NavSessionGroup {
  projectName: string
  sessions: SessionSummary[]
}

/**
 * 合成「最近会话」：时间倒序、同一条只留一份、截到 limit 条。
 *
 * 同 id 去重是防重复项目（同一目录被两个引擎各收录一次时，
 * 项目列表里可能出现同源条目）把同一条会话报两遍。
 */
export function mergeRecentSessions(
  groups: NavSessionGroup[],
  limit = 40,
): NavSessionItem[] {
  const items: NavSessionItem[] = []
  const seen = new Set<string>()
  for (const group of groups) {
    for (const session of group.sessions) {
      if (seen.has(session.id)) continue
      seen.add(session.id)
      items.push({ session, projectName: group.projectName })
    }
  }
  items.sort((a, b) => b.session.timeUpdated - a.session.timeUpdated)
  return limit > 0 ? items.slice(0, limit) : []
}

/** 关键词筛选：标题或项目名命中即可（大小写不敏感，空词＝全要）。 */
export function filterNavSessions(items: NavSessionItem[], query: string): NavSessionItem[] {
  const needle = query.trim().toLowerCase()
  if (!needle) return items
  return items.filter(item => {
    const title = (item.session.title ?? '').toLowerCase()
    return title.includes(needle) || item.projectName.toLowerCase().includes(needle)
  })
}
