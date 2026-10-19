<script setup lang="ts">
/**
 * 工作台：**一页 ＝ 一个 Agent 的工作区**。
 *
 * 每页里有两种状态：
 * 1) 多列并排：一屏多列，每列一个会话（完整指挥能力：发送 / 插话 / 停止 / 审批）。
 * 2) 两栏「新建会话」页：左＝当前进行的会话 + 底部目录选择（可新增本机文件夹），
 *    右＝本页还没挑 Agent 时先挑 Agent；挑过之后右栏就是这个 Agent 的草稿输入框——
 *    发出第一条消息才真的建会话（不然聊不聊都会留下没名字的空壳，而目前删不掉）。
 *
 * 页面绑定的 Agent 定了就不在页内换：换 Agent 走顶栏「‹Agent› 的会话」面板底部的
 * 「＋ 用 … 新建会话」（会给那个 Agent 开一页 / 用它自己的草稿框）。
 * 所有页面都挂载（非当前页只是藏起来），后台页面里的会话照旧收引擎事件——左栏状态、托盘角标才是活的。
 * 页面数据（Agent / 目录 / 列 / 聚焦）持久化在 workspace.ts；列内会话概要按需拉取、收口后刷新。
 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { open as openDirectoryDialog } from '@tauri-apps/plugin-dialog'
import { api } from '../api'
import { clampColumnWidth, restoreColumnAction, workbenchShowsStartPage } from '../workbench'
import {
  allColumns,
  bindPageAgent,
  bindPageDirectory,
  claimPage,
  currentPage,
  currentPageId,
  ensurePageOfAgent,
  newColumn,
  pageOfSession,
  pages,
  persistWorkspace,
  switchPage,
  type WorkspaceColumn,
  type WorkspacePage,
} from '../workspace'
import { engineChoices } from '../sessionStart'
import { engineLabel } from '../workshop'
import { lastStartDirectory, lastStartEngine, rememberStart } from '../startPrefs'
import { basename } from '../format'
import type { EngineHealth, ProjectItem, SessionSummary } from '../types'
import EngineChoicePanel from './EngineChoicePanel.vue'
import NewSessionDraft from './NewSessionDraft.vue'
import SelectMenu from './SelectMenu.vue'
import SessionView from './SessionView.vue'

const props = defineProps<{
  projects: ProjectItem[]
  health: EngineHealth[]
}>()

const emit = defineEmits<{
  (e: 'error', message: string): void
  (e: 'settled'): void
  (e: 'status', status: {
    running: number
    waiting: number
    runningIds: string[]
    waitingIds: string[]
  }): void
}>()

/** 手动新增的目录：还没被引擎收录成项目，选中后照样能在里面建会话。 */
interface ExtraDir {
  id: string
  label: string
  worktree: string
}

const EXTRA_DIR_PREFIX = 'dir:'

const flashId = ref<string | null>(null)
/** 手动新增、还没被引擎收录成项目的目录（本次运行里加的，不落盘）。 */
const extraDirs = ref<ExtraDir[]>([])
const creatingSession = ref(false)
const sessionError = ref<string | null>(null)

/** 每个会话的聊天框实例：草稿那条消息要交给它的 sendText 发出去（流式 / 审批走平时那条路）。 */
const sessionViews = new Map<string, InstanceType<typeof SessionView>>()

function bindView(sessionId: string, view: unknown) {
  if (view) sessionViews.set(sessionId, view as InstanceType<typeof SessionView>)
  else sessionViews.delete(sessionId)
}

/** 当前页面。 */
const page = currentPage

/**
 * 本页的工作目录（绝对路径）。页面自己记着：切页 / 重启后各页目录都在，
 * 顶栏面板底部「＋ 用 … 新建会话」开出来的新页会自动沿用上次用过的目录。
 */
const selectedDir = computed<ExtraDir | null>(() => {
  const path = page.value.directory
  if (!path) return null
  const project = props.projects.find(item => samePath(item.worktree, path))
  if (project) {
    return {
      id: project.id,
      label: project.name || basename(project.worktree),
      worktree: project.worktree,
    }
  }
  const extra = extraDirs.value.find(item => samePath(item.worktree, path))
  if (extra) return extra
  return { id: `${EXTRA_DIR_PREFIX}${path}`, label: basename(path) || path, worktree: path }
})

const selectedDirId = computed(() => selectedDir.value?.id ?? '')

const choices = computed(() => engineChoices(props.health))

const pickerHint = computed(() => {
  if (!selectedDir.value) return '先在左边选一个工作目录，或从下拉里的「＋ 新增目录…」在本机挑一个文件夹'
  return '挑一个 Agent 开始这一页——之后这一页只用它；要换就去顶栏面板底部「＋ 新建会话」'
})

const pickerDescription = computed(() =>
  selectedDir.value
    ? '工作目录已选好。挑一个 Agent，先写第一条指令再看要不要建会话。'
    : '还没选工作目录。先在左边选一个，右边才会解锁。',
)

/** 「＋ 新建会话」按钮的提示：本页已经绑了 Agent 就直说用谁。 */
const newSessionHint = computed(() =>
  page.value.agent
    ? `新建会话：用本页的 ${engineLabel(page.value.agent)}（换 Agent ⇒ 顶栏面板底部「＋ 新建会话」）`
    : '新建会话：先在右栏挑一个 Agent（这一页之后只用它）',
)

/** 目录下拉：手动新增的排在前面；同路径的项目优先，避免出现两份。 */
const dirOptions = computed(() => {
  const raw = [
    ...extraDirs.value
      .filter(extra => !props.projects.some(item => samePath(item.worktree, extra.worktree)))
      .map(extra => ({ value: extra.id, label: extra.label, worktree: extra.worktree })),
    ...props.projects.map(item => ({
      value: item.id,
      label: item.name || basename(item.worktree),
      worktree: item.worktree,
    })),
  ]
  // 同一个目录可能被不同引擎各登记一次（native / opencode…）：只留一条。
  // 页面记的是路径，不是项目 id，选哪条结果都一样。
  const rows: typeof raw = []
  for (const row of raw) {
    if (!rows.some(kept => samePath(kept.worktree, row.worktree))) rows.push(row)
  }
  // 页面记着的目录这会儿不在列表里（项目被删掉之类）也要能显示出来；
  // 路径相同但 id 不同的，就地换成当前这条，别让下拉显示成「未选择」。
  const current = selectedDir.value
  if (current) {
    const same = rows.findIndex(row => samePath(row.worktree, current.worktree))
    if (same >= 0) rows[same] = { value: current.id, label: rows[same].label, worktree: current.worktree }
    else rows.unshift({ value: current.id, label: current.label, worktree: current.worktree })
  }
  // 目录名可以重名（两个 app 分处不同项目）：重名的才补一行父级路径来区分。
  // 名字唯一时不加第二行，列表保持紧凑。
  const counts = new Map<string, number>()
  for (const row of rows) counts.set(row.label, (counts.get(row.label) ?? 0) + 1)
  return rows.map(row =>
    (counts.get(row.label) ?? 0) > 1
      ? { value: row.value, label: row.label, hint: dirHint(row.worktree) }
      : { value: row.value, label: row.label },
  )
})

/** 本页的两栏「新建会话」页在前台吗；不在就画多列并排。 */
function showsStartPage(target: WorkspacePage): boolean {
  return workbenchShowsStartPage(target.startOpen, target.columns.length)
}

/** 多列里画哪些列：两栏页聚焦的那一条由右栏画，同一个会话不会出现两个 SessionView。 */
function canvasColumnsOf(target: WorkspacePage): WorkspaceColumn[] {
  if (!showsStartPage(target)) return target.columns
  return target.columns.filter(column => column.sessionId !== target.focusedId)
}

/** 某一页右栏聚焦的那一条；null 表示右栏显示挑 Agent / 该页的草稿框。 */
function focusedColumnOf(target: WorkspacePage): WorkspaceColumn | null {
  return target.columns.find(column => column.sessionId === target.focusedId) ?? null
}

const focusedColumn = computed(() => focusedColumnOf(page.value))

const runningCount = computed(() => allColumns.value.filter(column => column.status.running).length)
const waitingCount = computed(() =>
  allColumns.value.reduce((sum, column) => sum + column.status.waiting, 0),
)
const runningIds = computed(() =>
  allColumns.value.filter(column => column.status.running).map(column => column.sessionId),
)
const waitingIds = computed(() =>
  allColumns.value.filter(column => column.status.waiting > 0).map(column => column.sessionId),
)

// 状态汇总上报（顶栏角标 + 托盘用；id 列表供托盘与归档视图去重）。跨页统计：后台页面的会话也算。
watch(
  [runningCount, waitingCount, runningIds, waitingIds],
  () =>
    emit('status', {
      running: runningCount.value,
      waiting: waitingCount.value,
      runningIds: runningIds.value,
      waitingIds: waitingIds.value,
    }),
  { immediate: true },
)

function report(cause: unknown) {
  emit('error', cause instanceof Error ? cause.message : String(cause))
}

/** 目录比路径：忽略结尾的斜杠。（后端字段理论上一定在，但页面渲染经不起一个 undefined。） */
function samePath(a: string | null | undefined, b: string | null | undefined): boolean {
  return (a ?? '').replace(/\/+$/, '') === (b ?? '').replace(/\/+$/, '')
}

/** 路径提示：太长就只留尾部三段，前面用 … 代替。 */
function shortPath(path: string): string {
  const parts = path.replace(/\/+$/, '').split('/')
  return parts.length <= 4 ? path : `…/${parts.slice(-3).join('/')}`
}

/** 目录的父级路径：下拉里重名时用来看清是哪一个。 */
function dirHint(worktree: string): string {
  const clean = worktree.replace(/\/+$/, '')
  const cut = clean.lastIndexOf('/')
  return cut > 0 ? shortPath(clean.slice(0, cut)) : clean
}

function sleep(ms: number) {
  return new Promise(resolve => window.setTimeout(resolve, ms))
}

// ---- 会话概要 ----

/** 一次查询的结果；`missing`＝后端确认没这个会话，`error`＝这次没拉到（不许当成不存在）。 */
type SummaryLookup =
  | { kind: 'ok'; summary: SessionSummary }
  | { kind: 'missing' }
  | { kind: 'error'; error: unknown }

/**
 * 查一次会话概要：`api.sessionSummary` 解析出 null＝后端确认没这个会话；抛错＝这次查询失败。
 * 恢复布局时必须区分这两者，所以不能只看「有没有拿到」。
 */
async function lookupSummary(sessionId: string): Promise<SummaryLookup> {
  try {
    const summary = await api.sessionSummary(sessionId)
    return summary ? { kind: 'ok', summary } : { kind: 'missing' }
  } catch (error) {
    return { kind: 'error', error }
  }
}

/** 列内刷新用：拉不到就报一句、返回 null（列保持现状，不删）。 */
async function fetchSummary(sessionId: string): Promise<SessionSummary | null> {
  const result = await lookupSummary(sessionId)
  if (result.kind === 'error') report(result.error)
  return result.kind === 'ok' ? result.summary : null
}

/** 恢复布局用：查询失败（后端还没起来之类）重试几次，只有确认不存在才返回 missing。 */
async function lookupSummaryForRestore(sessionId: string): Promise<SummaryLookup> {
  let last: SummaryLookup = { kind: 'error', error: null }
  for (let attempt = 0; attempt < 4; attempt += 1) {
    const result = await lookupSummary(sessionId)
    if (result.kind !== 'error') return result
    last = result
    await sleep(300 * (attempt + 1))
  }
  return last
}

/** 新建会话后 DB 写入可能滞后：重试几次。 */
async function fetchSummaryWithRetry(sessionId: string): Promise<SessionSummary | null> {
  for (let attempt = 0; attempt < 6; attempt += 1) {
    const summary = await fetchSummary(sessionId)
    if (summary) return summary
    await sleep(250)
  }
  return null
}

// ---- 列的开合 ----

function columnOf(sessionId: string): WorkspaceColumn | null {
  for (const item of pages.value) {
    const column = item.columns.find(entry => entry.sessionId === sessionId)
    if (column) return column
  }
  return null
}

/** 加一列（已存在则只刷新概要），不改变当前看到的是哪一屏。 */
function addColumn(sessionId: string, session?: SessionSummary) {
  const target = page.value
  const existing = target.columns.find(column => column.sessionId === sessionId)
  if (existing) {
    if (session) existing.session = session
    return
  }
  const column = newColumn(sessionId)
  if (session) column.session = session
  target.columns.push(column)
  persistWorkspace()
  if (!session) {
    void fetchSummary(sessionId).then(summary => {
      const entry = target.columns.find(item => item.sessionId === sessionId)
      if (entry) entry.session = summary
    })
  }
}

function closeColumn(sessionId: string) {
  const target = page.value
  target.columns = target.columns.filter(column => column.sessionId !== sessionId)
  if (target.focusedId === sessionId) target.focusedId = null
  persistWorkspace()
}

/** 列里会话概要没拉到时原地重试；确认不存在只提示用户关闭，不自动删列。 */
async function retryColumn(sessionId: string) {
  const column = columnOf(sessionId)
  if (!column) return
  const result = await lookupSummary(sessionId)
  if (result.kind === 'ok') {
    column.session = result.summary
    return
  }
  if (result.kind === 'missing') {
    report(new Error('这个会话已经不存在了，可以关掉这一列'))
    return
  }
  report(result.error)
}

/** 让某列滚进视野并闪一下，提示「就是这一列」。 */
function focusColumn(sessionId: string) {
  flashId.value = sessionId
  window.setTimeout(() => {
    if (flashId.value === sessionId) flashId.value = null
  }, 1200)
  void nextTick(() => {
    const el = document.querySelector(`.wb-page.current [data-session-id="${sessionId}"]`)
    el?.scrollIntoView({ behavior: 'smooth', inline: 'center', block: 'nearest' })
  })
}

/** 在某一页里定位一条会话：落到多列画布并闪一下（那一页不是当前页就只切状态）。 */
function revealInPage(target: WorkspacePage, sessionId: string) {
  target.startOpen = false
  target.focusedId = null
  persistWorkspace()
  if (target.id === currentPageId.value) {
    sessionError.value = null
    focusColumn(sessionId)
  }
}

/**
 * 开一列：归档「⇥ 工作台」、顶栏点一条会话都走它。
 * 已经在列里（任何一页）→ 切到那一页并定位；没开 → 落到它那个 Agent 的页面（没有就新开一页）。
 */
async function openColumn(sessionId: string, session?: SessionSummary) {
  const owner = pageOfSession(sessionId)
  if (owner) {
    switchPage(owner.id)
    revealInPage(owner, sessionId)
    return
  }

  let summary: SessionSummary | null = session ?? null
  if (!summary) {
    const result = await lookupSummary(sessionId)
    if (result.kind === 'error') report(result.error)
    if (result.kind === 'ok') summary = result.summary
  }

  const engine = summary?.engine ?? null
  // 会话属于哪个 Agent 就落到哪个 Agent 的页面：页面不混 Agent。
  if (engine) ensurePageOfAgent(engine, summary?.directory ?? null)
  const target = page.value
  if (engine) claimPage(target, engine, summary?.directory ?? null)
  addColumn(sessionId, summary ?? undefined)
  revealInPage(target, sessionId)
}

/**
 * 顶栏导航点一条会话：**落进两栏「新建会话」页**——左栏（当前进行的会话）加一行并选中，
 * 右栏就是它的对话。不会切到多列并排：那是「⊞ 并排看」或顶栏 ⇥ 的显式动作。
 */
async function openInStartPage(sessionId: string, session?: SessionSummary) {
  const owner = pageOfSession(sessionId)
  if (owner) {
    switchPage(owner.id)
    owner.startOpen = true
    owner.focusedId = sessionId
    sessionError.value = null
    persistWorkspace()
    return
  }

  let summary: SessionSummary | null = session ?? null
  if (!summary) {
    const result = await lookupSummary(sessionId)
    if (result.kind === 'error') report(result.error)
    if (result.kind === 'ok') summary = result.summary
  }

  const engine = summary?.engine ?? null
  // 会话属于哪个 Agent 就落到哪个 Agent 的页面：页面不混 Agent。
  if (engine) ensurePageOfAgent(engine, summary?.directory ?? null)
  const target = page.value
  if (engine) claimPage(target, engine, summary?.directory ?? null)
  addColumn(sessionId, summary ?? undefined)
  showStartPage(sessionId)
}

/**
 * 「＋ 新建会话」：回本页的两栏页。
 * 本页已经绑了 Agent 就直接是这个 Agent 的草稿框（页内不换 Agent）；还没绑就是选择器。
 */
function startNewSession() {
  page.value.startOpen = true
  page.value.focusedId = null
  sessionError.value = null
  persistWorkspace()
}

// ---- 两栏「新建会话」页 ----

/** 切到两栏页；focus 为 null 时右栏是本页的草稿框（没绑 Agent 则是选择器）。 */
function showStartPage(focus: string | null) {
  page.value.startOpen = true
  page.value.focusedId = focus
  sessionError.value = null
  persistWorkspace()
}

/** 左栏点一条会话：右栏换成它的聊天框。 */
function focusSession(sessionId: string) {
  page.value.focusedId = sessionId
  sessionError.value = null
  persistWorkspace()
}

function resolveDirPath(dirId: string): string | null {
  const extra = extraDirs.value.find(item => item.id === dirId)
  if (extra) return extra.worktree
  return props.projects.find(item => item.id === dirId)?.worktree ?? null
}

function chooseDir(dirId: string) {
  sessionError.value = null
  const path = resolveDirPath(dirId)
  if (!path) return
  bindPageDirectory(page.value, path)
  // 记住这次挑的目录：下次新页面默认就是它，不用再挑一次。
  rememberStart({ directory: path })
}

/** 上次用的目录还在的话，进页面就替用户选好（新页面默认沿用上次）。 */
function restoreLastDirectory() {
  const target = page.value
  if (target.directory) return
  const last = lastStartDirectory.value
  if (!last) return
  bindPageDirectory(target, last)
}

/** 新增目录：挑一个本机文件夹当工作目录（还没被引擎收录也能直接在里面建会话）。 */
async function addDirectory() {
  try {
    const picked = await openDirectoryDialog({
      directory: true,
      multiple: false,
      title: '选择工作目录',
    })
    if (typeof picked !== 'string' || !picked) return
    const known = props.projects.find(item => samePath(item.worktree, picked))
    if (known) {
      bindPageDirectory(page.value, known.worktree)
      sessionError.value = null
      rememberStart({ directory: known.worktree })
      return
    }
    const id = `${EXTRA_DIR_PREFIX}${picked}`
    if (!extraDirs.value.some(item => item.id === id)) {
      extraDirs.value = [
        ...extraDirs.value,
        { id, label: basename(picked) || picked, worktree: picked },
      ]
    }
    bindPageDirectory(page.value, picked)
    sessionError.value = null
    rememberStart({ directory: picked })
  } catch (cause) {
    report(cause)
  }
}

/**
 * 选中 Agent：不建会话，只把这一页绑给这个 Agent，右栏换成首条消息框。
 * 真正建会话在 startFromDraft（用户点了发送）——这样空会话不会因为一次误点留下来。
 */
function onEngineSelected(engine: string) {
  if (!selectedDir.value) return
  sessionError.value = null
  bindPageAgent(page.value, engine)
}

/** 草稿发出第一条消息：建会话 → 开列 → 让那一列把这条消息发出去。 */
async function startFromDraft(text: string) {
  const dir = selectedDir.value
  const engine = page.value.agent
  if (!dir || !engine || !text.trim() || creatingSession.value) return
  creatingSession.value = true
  sessionError.value = null
  try {
    const created = await api.sessionCreate(dir.worktree, engine)
    // 记住这次的选择：下次「新建会话」默认沿用这个 Agent 与目录。
    rememberStart({ engine, directory: dir.worktree })
    const summary = await fetchSummaryWithRetry(created.sessionId)
    addColumn(created.sessionId, summary ?? undefined)
    page.value.focusedId = created.sessionId
    persistWorkspace()
    // 右栏原地变成这个新会话的聊天框。
    await nextTick()
    const view = sessionViews.get(created.sessionId)
    if (view) {
      await view.sendText(text)
    } else {
      // 概要没拉到时右栏是「加载中」：直接发，别把用户刚写的话吞了。
      await api.sessionSend(created.sessionId, text)
    }
    emit('settled')
  } catch (cause) {
    sessionError.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    creatingSession.value = false
  }
}

// ---- 列宽拖拽 ----

let resizeState: { sessionId: string; startX: number; startWidth: number } | null = null

function startResize(event: PointerEvent, column: WorkspaceColumn) {
  event.preventDefault()
  resizeState = { sessionId: column.sessionId, startX: event.clientX, startWidth: column.width }
  window.addEventListener('pointermove', onResizeMove)
  window.addEventListener('pointerup', stopResize)
  document.body.style.cursor = 'col-resize'
  document.body.style.userSelect = 'none'
}

function onResizeMove(event: PointerEvent) {
  if (!resizeState) return
  const column = columnOf(resizeState.sessionId)
  if (!column) return
  column.width = clampColumnWidth(resizeState.startWidth + (event.clientX - resizeState.startX))
}

function stopResize() {
  resizeState = null
  window.removeEventListener('pointermove', onResizeMove)
  window.removeEventListener('pointerup', stopResize)
  document.body.style.cursor = ''
  document.body.style.userSelect = ''
  persistWorkspace()
}

// ---- 列内事件 ----

function onColumnStatus(sessionId: string, status: { running: boolean; waiting: number }) {
  const column = columnOf(sessionId)
  if (column) column.status = status
}

async function onColumnSettled(sessionId: string) {
  const column = columnOf(sessionId)
  if (column) {
    const summary = await fetchSummary(sessionId)
    if (summary) column.session = summary
  }
  emit('settled')
}

function dotClass(column: WorkspaceColumn) {
  if (column.status.waiting > 0) return 'tone-wait'
  if (column.status.running) return 'tone-run'
  return 'tone-idle'
}

function dotTitle(column: WorkspaceColumn) {
  if (column.status.waiting > 0) return '等待审批'
  if (column.status.running) return '执行中'
  return '空闲'
}

/** 左栏列表里的状态文案。 */
function statusText(column: WorkspaceColumn) {
  if (column.status.waiting > 0) return `等审批 ${column.status.waiting}`
  if (column.status.running) return '执行中'
  return '空闲'
}

/** 左栏列表第二行：概要还没到手（正在拉 / 这次没拉到）就别说「空闲」。 */
function rowSubText(column: WorkspaceColumn) {
  return column.session ? statusText(column) : '加载中…'
}

function columnTitle(column: WorkspaceColumn) {
  return column.session?.title || '未命名会话'
}

// ---- 恢复布局 ----

/** 把存下来的列认回来：拉概要、认页面绑的 Agent、确认不存在的列才丢。 */
async function restoreColumns() {
  const targets = pages.value.flatMap(target =>
    target.columns.map(column => ({ target, sessionId: column.sessionId })),
  )
  if (!targets.length) return

  const results = await Promise.all(
    targets.map(async item => ({ ...item, lookup: await lookupSummaryForRestore(item.sessionId) })),
  )

  let dropped = false
  for (const item of results) {
    const column = item.target.columns.find(entry => entry.sessionId === item.sessionId)
    if (!column) continue
    // 后端确认这个会话不存在：丢这一条。
    if (restoreColumnAction(item.lookup.kind) === 'drop') {
      dropped = true
      item.target.columns = item.target.columns.filter(entry => entry.sessionId !== item.sessionId)
      if (item.target.focusedId === item.sessionId) item.target.focusedId = null
      continue
    }
    // 只是这次没拉到：列留着（渲染成「加载中/重试」），原因报到顶栏。
    if (item.lookup.kind !== 'ok') {
      if (item.lookup.kind === 'error') report(item.lookup.error)
      continue
    }
    column.session = item.lookup.summary
    // 从旧版单画布迁来的页面还没绑 Agent：按第一条会话的 Agent 认领（页面不混 Agent）。
    claimPage(item.target, item.lookup.summary.engine, item.lookup.summary.directory)
  }
  // 只有确实丢过列才写回布局；查询失败绝不写——写下去等于替用户把列删了。
  if (dropped) persistWorkspace()
}

onMounted(() => {
  // 默认沿用上次挑的工作目录（还在列表里才用），省得每次都挑。
  restoreLastDirectory()
  void restoreColumns()
})

// 新页 / 切页：目录还没选就沿用上次用过的；项目列表到位后也补一次（首屏可能还没加载出来）。
watch([currentPageId, () => props.projects], () => restoreLastDirectory())

onBeforeUnmount(() => {
  stopResize()
})

defineExpose({
  openColumn,
  openInStartPage,
  showStartPage,
  startNewSession,
  focusSession,
})
</script>

<template>
  <section class="workbench">
    <!-- 一页＝一个 Agent 的工作区。所有页面都挂载（非当前页藏起来），后台页面里的会话照旧收引擎事件。 -->
    <div
      v-for="item in pages"
      :key="item.id"
      v-show="item.id === currentPageId"
      class="wb-page"
      :class="{ current: item.id === currentPageId }"
    >
      <!-- 多列并排：一直挂载，两栏页里也继续收引擎事件，左栏的状态才是活的。 -->
      <div v-show="!showsStartPage(item)" class="wb-scroll">
        <div
          v-for="column in canvasColumnsOf(item)"
          :key="column.sessionId"
          class="wb-column"
          :class="{ flash: flashId === column.sessionId }"
          :data-session-id="column.sessionId"
          :style="{ width: `${column.width}px` }"
        >
          <header class="wb-head">
            <span class="wb-dot" :class="dotClass(column)" :title="dotTitle(column)" />
            <span class="wb-title" :title="columnTitle(column)">{{ columnTitle(column) }}</span>
            <span
              v-if="column.status.waiting"
              class="wb-waiting"
              :title="`${column.status.waiting} 个权限请求等待审批`"
            >
              审批 {{ column.status.waiting }}
            </span>
            <button class="wb-btn" title="关闭这一列" @click="closeColumn(column.sessionId)">✕</button>
          </header>

          <SessionView
            v-if="column.session"
            :ref="el => bindView(column.sessionId, el)"
            embedded
            :session="column.session"
            :has-project="true"
            @error="report"
            @settled="onColumnSettled(column.sessionId)"
            @status="onColumnStatus(column.sessionId, $event)"
          />
          <div v-else class="wb-missing">
            <div>会话加载中或已不存在</div>
            <div class="wb-missing-acts">
              <button class="wb-btn wide" @click="retryColumn(column.sessionId)">重试</button>
              <button class="wb-btn wide" @click="closeColumn(column.sessionId)">关闭这一列</button>
            </div>
          </div>

          <div class="wb-resize" title="拖动调整宽度" @pointerdown="startResize($event, column)" />
        </div>
      </div>

      <!-- 两栏「新建会话」页：左＝目录 + 当前进行的会话，右＝挑 Agent（挑过就是草稿框）。 -->
      <div v-if="showsStartPage(item)" class="wb-start">
        <aside class="wb-start-left" aria-label="当前进行的会话与目录">
          <button
            class="wb-start-new"
            :class="{ on: !item.focusedId }"
            :title="newSessionHint"
            @click="startNewSession"
          >
            ＋ 新建会话
          </button>

          <div class="wb-start-head">
            <span>当前进行的会话</span>
            <span v-if="item.columns.length" class="wb-start-count">{{ item.columns.length }}</span>
          </div>

          <div class="wb-start-list">
            <p v-if="!item.columns.length" class="wb-start-empty">
              这一页还没有会话——
              {{ item.agent ? `用 ${engineLabel(item.agent)} 建一个` : '先挑一个 Agent' }}；
              换 Agent 走顶栏面板底部的「＋ 新建会话」
            </p>
            <button
              v-for="column in item.columns"
              :key="column.sessionId"
              class="wb-start-item"
              :class="{ active: column.sessionId === item.focusedId }"
              :title="columnTitle(column)"
              @click="focusSession(column.sessionId)"
            >
              <span class="wb-dot" :class="dotClass(column)" :title="dotTitle(column)" />
              <span class="wb-start-item-main">
                <span class="wb-start-item-title">{{ columnTitle(column) }}</span>
                <span class="wb-start-item-sub" :class="dotClass(column)">
                  {{ rowSubText(column) }}
                </span>
              </span>
            </button>
          </div>

          <div class="wb-start-foot">
            <div class="wb-start-dir">
              <div class="wb-start-dir-head">
                <span class="wb-start-label">目录</span>
              </div>
              <SelectMenu
                :model-value="selectedDirId"
                :options="dirOptions"
                placeholder="选择目录…"
                size="sm"
                :title="selectedDir?.worktree ?? '选择工作目录'"
                action-label="新增目录…"
                @update:model-value="chooseDir"
                @action="addDirectory"
              />
              <p
                class="wb-start-path"
                :class="{ empty: !selectedDir }"
                :title="selectedDir?.worktree ?? '还没选目录'"
              >
                {{ selectedDir ? shortPath(selectedDir.worktree) : '还没选目录，右边 Agent 会锁住' }}
              </p>
            </div>
          </div>
        </aside>

        <div class="wb-start-right">
          <template
            v-for="focused in item.columns.filter(column => column.sessionId === item.focusedId)"
            :key="focused.sessionId"
          >
            <SessionView
              v-if="focused.session"
              :ref="el => bindView(focused.sessionId, el)"
              embedded
              :session="focused.session"
              :has-project="true"
              @error="report"
              @settled="onColumnSettled(focused.sessionId)"
              @status="onColumnStatus(focused.sessionId, $event)"
            />
            <div v-else class="wb-missing">
              <div>会话加载中或已不存在</div>
              <div class="wb-missing-acts">
                <button class="wb-btn wide" @click="retryColumn(focused.sessionId)">重试</button>
              </div>
            </div>
          </template>

          <template v-if="!focusedColumnOf(item)">
            <NewSessionDraft
              v-if="item.agent"
              :engine-label="engineLabel(item.agent)"
              :directory="item.directory ?? ''"
              :busy="creatingSession"
              :error="sessionError"
              @send="startFromDraft"
            />

            <EngineChoicePanel
              v-else
              :choices="choices"
              :locked="!item.directory"
              :recent="lastStartEngine"
              :description="pickerDescription"
              :hint="pickerHint"
              :error="sessionError"
              @select="onEngineSelected"
            />
          </template>
        </div>
      </div>

      <button
        v-if="!showsStartPage(item)"
        class="wb-add"
        :title="newSessionHint"
        @click="startNewSession"
      >
        ＋ 新建会话
      </button>
    </div>
  </section>
</template>

<style scoped src="../styles/components/WorkbenchPane.css"></style>
