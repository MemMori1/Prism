/**
 * 工作区状态：**一个页面＝一个 Agent 的工作区**。
 *
 * 为什么这么分：一个页面只装一个 Agent 的会话（列），顶栏显示「<Agent> 的会话」+ 本页的会话，
 * 右边 ＋ 新建一个空白页面（重新挑 Agent）。想换 Agent 就开新页面，而不是在同一个页里混着开——
 * 混着开过一版，用户看到的是「选完一个工具就不让换」，其实是缺了「开新页」这条路。
 *
 * 放在模块级（像 panes.ts / startPrefs.ts）：顶栏导航与工作台共用同一份状态，
 * 不用把页面的列再从组件 emit 上去一遍。
 *
 * 纯逻辑（解析 / 序列化 / 迁移 / 新建）与状态分开，方便单测。
 */
import { computed, ref } from 'vue'
import { clampColumnWidth, parseWorkbenchLayout, WORKBENCH_DEFAULT_WIDTH } from './workbench'
import type { SessionSummary } from './types'

export const WORKSPACE_KEY = 'prism-workspace-v1'
/** 旧版「单画布 + 列」的布局键：只在迁移时读一次。 */
export const LEGACY_KEY = 'prism-workbench-v1'

export interface WorkspaceColumn {
  sessionId: string
  width: number
  /** 运行期：会话概要（切页 / 重启后由工作台补）。 */
  session: SessionSummary | null
  /** 运行期：跑没跑、待审批几条。 */
  status: { running: boolean; waiting: number }
}

export interface WorkspacePage {
  id: string
  /** 这一页绑的 Agent；null＝刚新建、还没挑。 */
  agent: string | null
  /** 本页的工作目录（绝对路径）；null＝还没选。 */
  directory: string | null
  columns: WorkspaceColumn[]
  /** 两栏「新建会话」页在前台吗（false＝看多列画布）。 */
  startOpen: boolean
  /** 右栏聚焦的会话；null＝看本页的默认视图（草稿 / 挑 Agent）。 */
  focusedId: string | null
}

export interface WorkspaceState {
  pages: WorkspacePage[]
  currentId: string
}

// ---- 纯逻辑 ----

export function newColumn(
  sessionId: string,
  width: number = WORKBENCH_DEFAULT_WIDTH,
): WorkspaceColumn {
  return {
    sessionId,
    width: clampColumnWidth(width),
    session: null,
    status: { running: false, waiting: 0 },
  }
}

let pageSeq = 0

export function newPage(agent: string | null = null, directory: string | null = null): WorkspacePage {
  pageSeq += 1
  return {
    id: `page-${pageSeq.toString(36)}-${Date.now().toString(36)}`,
    agent,
    directory,
    columns: [],
    startOpen: true,
    focusedId: null,
  }
}

function parseColumn(raw: unknown): WorkspaceColumn | null {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return null
  const record = raw as Record<string, unknown>
  if (typeof record.sessionId !== 'string' || !record.sessionId) return null
  const width =
    typeof record.width === 'number' ? clampColumnWidth(record.width) : WORKBENCH_DEFAULT_WIDTH
  return newColumn(record.sessionId, width)
}

function text(value: unknown): string | null {
  return typeof value === 'string' && value ? value : null
}

/** 解析持久化的页面数据；坏数据一律当没有（返回 null），绝不抛。 */
export function parseWorkspaceState(raw: string | null): WorkspaceState | null {
  if (!raw) return null
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch {
    return null
  }
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return null
  const record = parsed as Record<string, unknown>
  if (!Array.isArray(record.pages)) return null

  const pages: WorkspacePage[] = []
  for (const item of record.pages) {
    if (!item || typeof item !== 'object' || Array.isArray(item)) continue
    const page = item as Record<string, unknown>
    const id = text(page.id)
    if (!id) continue
    const columns = Array.isArray(page.columns)
      ? page.columns.map(parseColumn).filter((column): column is WorkspaceColumn => !!column)
      : []
    const focusedId = text(page.focusedId)
    pages.push({
      id,
      agent: text(page.agent),
      directory: text(page.directory),
      columns,
      startOpen: page.startOpen !== false,
      // 聚焦的那条不在这页的列里就当没有（列可能已经被关掉/迁走）。
      focusedId: focusedId && columns.some(column => column.sessionId === focusedId) ? focusedId : null,
    })
  }
  if (!pages.length) return null

  const currentId = pages.some(page => page.id === record.currentId)
    ? String(record.currentId)
    : pages[0].id
  return { pages, currentId }
}

/** 序列化：只留能重建页面的字段（会话概要 / 状态是运行期的，不存）。 */
export function serializeWorkspaceState(state: WorkspaceState): string {
  return JSON.stringify({
    currentId: state.currentId,
    pages: state.pages.map(page => ({
      id: page.id,
      agent: page.agent,
      directory: page.directory,
      startOpen: page.startOpen,
      focusedId: page.focusedId,
      columns: page.columns.map(column => ({
        sessionId: column.sessionId,
        width: clampColumnWidth(column.width),
      })),
    })),
  })
}

/**
 * 旧版「单画布 + 列」→ 页面：**一列一页**。
 *
 * 旧布局里各列可能来自不同引擎，而页面只认一个 Agent；一列一页最保守——
 * 每页的 Agent 等加载到会话概要后再认（`agent` 先留空）。
 */
export function pagesFromLegacyLayout(raw: string | null): WorkspacePage[] {
  return parseWorkbenchLayout(raw).map(entry => {
    const page = newPage()
    page.startOpen = false
    page.focusedId = entry.sessionId
    page.columns = [newColumn(entry.sessionId, entry.width)]
    return page
  })
}

// ---- 状态 ----

function readRaw(key: string): string | null {
  try {
    return localStorage.getItem(key)
  } catch {
    return null
  }
}

function writeRaw(key: string, value: string | null): void {
  try {
    if (value === null) localStorage.removeItem(key)
    else localStorage.setItem(key, value)
  } catch {
    // 存储不可用（隐私模式等）：当没记住，不拦人。
  }
}

function initialState(): WorkspaceState {
  const saved = parseWorkspaceState(readRaw(WORKSPACE_KEY))
  if (saved) return saved
  const migrated = pagesFromLegacyLayout(readRaw(LEGACY_KEY))
  if (migrated.length) {
    const state: WorkspaceState = { pages: migrated, currentId: migrated[0].id }
    writeRaw(WORKSPACE_KEY, serializeWorkspaceState(state))
    writeRaw(LEGACY_KEY, null)
    return state
  }
  const page = newPage()
  return { pages: [page], currentId: page.id }
}

const state = ref<WorkspaceState>(initialState())

export const pages = computed(() => state.value.pages)
export const currentPageId = computed(() => state.value.currentId)
/** 当前页面：列表空不了（关掉最后一页会补一个空白页）。 */
export const currentPage = computed(
  () => state.value.pages.find(page => page.id === state.value.currentId) ?? state.value.pages[0],
)
/** 所有页面的列（顶栏角标、托盘统计要跨页）。 */
export const allColumns = computed(() => state.value.pages.flatMap(page => page.columns))

export function persistWorkspace(): void {
  writeRaw(WORKSPACE_KEY, serializeWorkspaceState(state.value))
}

/** 这个会话开在哪个页面里（没有＝null）。 */
export function pageOfSession(sessionId: string): WorkspacePage | null {
  return state.value.pages.find(page => page.columns.some(c => c.sessionId === sessionId)) ?? null
}

/** 这个 Agent 的页面（没有＝null）。 */
export function pageOfAgent(agent: string): WorkspacePage | null {
  return state.value.pages.find(page => page.agent === agent) ?? null
}

export function switchPage(pageId: string): boolean {
  if (!state.value.pages.some(page => page.id === pageId)) return false
  state.value.currentId = pageId
  persistWorkspace()
  return true
}

/** 新建一个空白页面并切过去（顶栏的 ＋）。 */
export function addPage(agent: string | null = null, directory: string | null = null): WorkspacePage {
  const page = newPage(agent, directory)
  state.value.pages = [...state.value.pages, page]
  state.value.currentId = page.id
  persistWorkspace()
  return page
}

/** 关掉一个页面；关掉最后一页时补一个空白页，当前页永远存在。 */
export function closePage(pageId: string): void {
  const rest = state.value.pages.filter(page => page.id !== pageId)
  if (!rest.length) {
    const page = newPage()
    state.value.pages = [page]
    state.value.currentId = page.id
  } else {
    state.value.pages = rest
    if (state.value.currentId === pageId) state.value.currentId = rest[rest.length - 1].id
  }
  persistWorkspace()
}

/** 某个 Agent 的页面：有就切过去（顺手补目录），没有就新建一页再切过去。 */
export function ensurePageOfAgent(agent: string, directory: string | null = null): WorkspacePage {
  const found = pageOfAgent(agent)
  if (found) {
    if (directory && !found.directory) {
      found.directory = directory
      persistWorkspace()
    }
    state.value.currentId = found.id
    persistWorkspace()
    return found
  }
  return addPage(agent, directory)
}

/**
 * 认领页面：还没绑 Agent 的页面（含从旧布局迁来的）按第一条会话的 Agent 认领。
 * 已经绑了别的 Agent 就返回 false——页面不混 Agent。
 */
export function claimPage(page: WorkspacePage, agent: string, directory: string | null = null): boolean {
  if (page.agent && page.agent !== agent) return false
  if (!page.agent) page.agent = agent
  if (directory && !page.directory) page.directory = directory
  persistWorkspace()
  return true
}

export function bindPageAgent(page: WorkspacePage, agent: string): void {
  page.agent = agent
  persistWorkspace()
}

export function bindPageDirectory(page: WorkspacePage, directory: string | null): void {
  page.directory = directory
  persistWorkspace()
}
