/**
 * 项目栏 / 会话栏的宽度：可拖拽、带最小最大宽度、记住上次的值。
 *
 * 三栏关系是「左固定 + 中固定 + 主区域吃剩余」，所以拖某一条分隔条时，
 * 只改它左边那一栏，宽度预算里要扣掉主区域的最小宽度，
 * 窗口变窄时再按「先收会话栏、后收项目栏」的顺序腾地方。
 *
 * 分隔条本身不占布局宽度（见 SplitHandle.css），因此预算里不用扣它。
 */
import { ref } from 'vue'

export const MIN_PROJECTS = 172
export const MAX_PROJECTS = 420
export const MIN_SESSIONS = 220
export const MAX_SESSIONS = 560
/** 主区域（会话详情）至少要留的宽度。 */
export const MIN_MAIN = 420

export const DEFAULT_PROJECTS = 216
export const DEFAULT_SESSIONS = 264

export interface PaneWidths {
  projects: number
  sessions: number
}

export type PaneKind = 'projects' | 'sessions'

export function clamp(value: number, min: number, max: number): number {
  if (Number.isNaN(value)) return min
  return Math.min(Math.max(value, min), max)
}

function budgetFor(available: number): number {
  return available - MIN_MAIN
}

/** 某一栏在当前布局下最多能拉多宽（其它栏和主区域都要留着）。 */
export function maxWidthFor(kind: PaneKind, widths: PaneWidths, available: number): number {
  const fixed = kind === 'projects' ? MAX_PROJECTS : MAX_SESSIONS
  // 还没量到宽度（隐藏 / 首帧）时不要卡死在最小值上。
  if (available <= 0) return fixed
  const other = kind === 'projects' ? widths.sessions : widths.projects
  return Math.min(fixed, budgetFor(available) - other)
}

/** 拖动中：以起手时的宽度为基准，按位移算出新宽度。 */
export function resizeWidths(
  kind: PaneKind,
  start: PaneWidths,
  deltaX: number,
  available: number,
): PaneWidths {
  if (kind === 'projects') {
    const max = Math.max(MIN_PROJECTS, maxWidthFor('projects', start, available))
    return { ...start, projects: clamp(start.projects + deltaX, MIN_PROJECTS, max) }
  }
  const max = Math.max(MIN_SESSIONS, maxWidthFor('sessions', start, available))
  return { ...start, sessions: clamp(start.sessions + deltaX, MIN_SESSIONS, max) }
}

/** 窗口变窄后收紧：先压会话栏到底，再压项目栏，都不低于各自最小值。 */
export function fitWidths(widths: PaneWidths, available: number): PaneWidths {
  let projects = clamp(widths.projects, MIN_PROJECTS, MAX_PROJECTS)
  let sessions = clamp(widths.sessions, MIN_SESSIONS, MAX_SESSIONS)
  // 没量到宽度时按记录值走，避免首帧把两栏压成最小值。
  if (available <= 0) return { projects, sessions }

  const budget = budgetFor(available)
  // 窗口窄到连两条最小宽度都放不下：保底给最小值，宁可主区域被挤没。
  if (budget <= 0) return { projects: MIN_PROJECTS, sessions: MIN_SESSIONS }

  let over = projects + sessions - budget
  if (over <= 0) return { projects, sessions }

  const fromSessions = Math.min(over, sessions - MIN_SESSIONS)
  sessions -= fromSessions
  over -= fromSessions
  if (over > 0) projects -= Math.min(over, projects - MIN_PROJECTS)
  return { projects, sessions }
}

/** 读回来的值可能是旧版本 / 被手改过的，一律夹到合法区间。 */
export function sanitizeWidths(raw: unknown): PaneWidths {
  const value = (raw ?? {}) as Partial<Record<PaneKind, unknown>>
  const pick = (input: unknown, fallback: number) =>
    typeof input === 'number' && Number.isFinite(input) ? input : fallback
  return {
    projects: clamp(pick(value.projects, DEFAULT_PROJECTS), MIN_PROJECTS, MAX_PROJECTS),
    sessions: clamp(pick(value.sessions, DEFAULT_SESSIONS), MIN_SESSIONS, MAX_SESSIONS),
  }
}

export const WIDTHS_KEY = 'prism-pane-widths'

function readWidths(): PaneWidths {
  try {
    const raw = localStorage.getItem(WIDTHS_KEY)
    if (!raw) return sanitizeWidths(null)
    return sanitizeWidths(JSON.parse(raw))
  } catch {
    return sanitizeWidths(null)
  }
}

/** 当前宽度（px）。拖拽时高频更新，落盘只在下手结束后。 */
const widths = ref<PaneWidths>(readWidths())

export const paneWidths = widths

export function setPaneWidths(next: PaneWidths): void {
  widths.value = next
}

/** 拖拽结束 / 改完键盘步进后调用。 */
export function savePaneWidths(): void {
  try {
    localStorage.setItem(WIDTHS_KEY, JSON.stringify(widths.value))
  } catch {
    // 写不进去（隐私模式 / 配额满）不影响本次会话的拖拽结果。
  }
}

export function resetPaneWidths(): void {
  widths.value = { projects: DEFAULT_PROJECTS, sessions: DEFAULT_SESSIONS }
  savePaneWidths()
}

/** 只把某一栏恢复默认（双击那条分隔条时用，别顺手把另一栏也重置了）。 */
export function resetPaneWidth(kind: PaneKind): void {
  widths.value =
    kind === 'projects'
      ? { ...widths.value, projects: DEFAULT_PROJECTS }
      : { ...widths.value, sessions: DEFAULT_SESSIONS }
  savePaneWidths()
}
