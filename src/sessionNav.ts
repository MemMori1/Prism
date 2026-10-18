/**
 * 顶部会话导航的纯逻辑：把各项目的会话合成「最近会话」、按 Agent 过滤、关键词筛选，
 * 以及面板左边那列 Agent（各自有多少会话）。
 *
 * 放在这里是为了能单测（组件只管画）。数据由 App 拉好（每项目一次 list_sessions），
 * 这里不碰 IPC。
 *
 * 口径（用户拍的）：顶栏只有**一个**控件——`<Agent> 的会话`；它点开就是面板，
 * 会话都收在里面（不在顶栏摊成一排标签）。面板里先挑 Agent，右边才是这个 Agent 的会话，
 * 点会话只让**左侧会话栏**对应那条激活（不开工作台的新列，不搞多列赛马）。
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

/** 面板左列的 Agent：会话数是按最近会话算的，不是全库。 */
export interface NavAgentChoice {
  id: string
  /** 最近会话里这个 Agent 有几条（0＝它名下暂时没有可点的）。 */
  count: number
  /** 本机能不能用（置灰用；不影响列出历史会话）。 */
  available: boolean
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

/**
 * 只要某个 Agent 的会话。
 *
 * 面板右边列的是「<Agent> 的会话」：选中哪个 Agent 就只列它名下的，
 * agent 为 null（还不知道看谁）＝没有可列的会话。
 */
export function sessionsOfEngine(items: NavSessionItem[], agent: string | null): NavSessionItem[] {
  if (!agent) return []
  return items.filter(item => item.session.engine === agent)
}

/**
 * 面板左列的 Agent 列表：给定顺序（新会话页那套 PRESENTATION）＋ 最近会话里出现过的引擎。
 *
 * 最近会话里冒出来的、但目录里没有的引擎也要列（不然那些会话在面板里永远点不到）；
 * 一条会话都没有的 Agent 也留着——用户会先在左边点它，再看右边「还没有会话」。
 */
export function navAgentList(
  items: NavSessionItem[],
  known: Array<{ id: string; available: boolean }>,
): NavAgentChoice[] {
  const counts = new Map<string, number>()
  for (const item of items) {
    const engine = item.session.engine
    counts.set(engine, (counts.get(engine) ?? 0) + 1)
  }
  const list: NavAgentChoice[] = []
  const seen = new Set<string>()
  for (const entry of known) {
    if (seen.has(entry.id)) continue
    seen.add(entry.id)
    list.push({
      id: entry.id,
      available: entry.available,
      count: counts.get(entry.id) ?? 0,
    })
  }
  for (const [id, count] of counts) {
    if (seen.has(id)) continue
    list.push({ id, available: true, count })
  }
  return list
}
