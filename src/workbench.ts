/**
 * 工作台纯逻辑：列宽约束与布局序列化（与组件解耦，方便单测）。
 */

export interface WorkbenchLayoutEntry {
  sessionId: string
  width: number
}

export const WORKBENCH_MIN_WIDTH = 360
export const WORKBENCH_MAX_WIDTH = 900
export const WORKBENCH_DEFAULT_WIDTH = 480

/**
 * 工作台的落地页：进工作台（侧栏点进来、或恢复上次就停在工作台）默认是两栏「新建会话」页。
 * 只有在页面里显式点了「⊞ 并排看」，或从归档点「⇥ 工作台」开列时，才看多列画布。
 * 一列都没有时无论如何都是两栏页（没有列可看）。
 */
export function workbenchShowsStartPage(startOpen: boolean, columnCount: number): boolean {
  return startOpen || columnCount === 0
}

/** 一次会话概要查询的结果：ok＝拿到了；missing＝后端确认会话不存在；error＝这次没拉到（网络抖动、后端没起来）。 */
export type SummaryLookupKind = 'ok' | 'missing' | 'error'

/**
 * 恢复上次并排布局时，一条存下来的列该怎么办。
 * 只有「后端确认这个会话不存在」才丢；查询失败一律留着——否则后端一次抖动就把用户并排的列写没了。
 */
export function restoreColumnAction(kind: SummaryLookupKind): 'keep' | 'drop' {
  return kind === 'missing' ? 'drop' : 'keep'
}

/** 列宽夹到可用区间；非法数字回落到默认宽。 */
export function clampColumnWidth(width: number): number {
  if (!Number.isFinite(width)) return WORKBENCH_DEFAULT_WIDTH
  return Math.min(WORKBENCH_MAX_WIDTH, Math.max(WORKBENCH_MIN_WIDTH, Math.round(width)))
}

/** 把持久化字符串解析成布局条目；容错：坏数据一律丢弃，不抛异常。 */
export function parseWorkbenchLayout(raw: string | null): WorkbenchLayoutEntry[] {
  if (!raw) return []
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch {
    return []
  }
  if (!Array.isArray(parsed)) return []
  const entries: WorkbenchLayoutEntry[] = []
  for (const item of parsed) {
    if (!item || typeof item !== 'object') continue
    const record = item as Record<string, unknown>
    if (typeof record.sessionId !== 'string' || !record.sessionId) continue
    const width =
      typeof record.width === 'number' ? clampColumnWidth(record.width) : WORKBENCH_DEFAULT_WIDTH
    entries.push({ sessionId: record.sessionId, width })
  }
  return entries
}

/** 序列化布局条目（只保留 id 与宽度）。 */
export function serializeWorkbenchLayout(entries: WorkbenchLayoutEntry[]): string {
  return JSON.stringify(
    entries.map(entry => ({ sessionId: entry.sessionId, width: clampColumnWidth(entry.width) })),
  )
}

/**
 * 两个目录是不是同一个（忽略结尾斜杠；任一侧为空就按「不同」算）。
 */
export function sameDirectory(a: string | null | undefined, b: string | null | undefined): boolean {
  if (!a || !b) return false
  return a.replace(/\/+$/, '') === b.replace(/\/+$/, '')
}

/**
 * 这一列属不属于这个目录——页面只展示「当前目录 + 当前 Agent」的会话。
 *
 * 会话还没到手时先算「属于」：拿不到目录就没法判断，这时候藏起来只会让人以为会话丢了。
 */
export function columnInDirectory(
  column: { session: { directory: string } | null },
  directory: string | null | undefined,
): boolean {
  if (!directory) return true
  if (!column.session) return true
  return sameDirectory(column.session.directory, directory)
}
