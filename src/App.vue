<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, computed, watch, nextTick } from 'vue'
import { api } from './api'
import { basename, relativeTime } from './format'
import { refreshGitInfo } from './gitInfo'
import { clearRenderError, renderError } from './renderErrors'
import { mergeTrayCounts, type ArchiveStatus, type WorkbenchStatus } from './trayStatus'
import {
  notifications,
  unreadCount,
  markAllRead,
  clearNotifications,
  pushNotification,
  type NotifyItem,
} from './notify'
import { onRoutineEvent } from './routineEvents'
import { engineLabel } from './workshop'
import {
  fitWidths,
  paneWidths,
  resetPaneWidth,
  resizeWidths,
  savePaneWidths,
  setPaneWidths,
  type PaneKind,
  type PaneWidths,
} from './panes'
import type {
  EngineHealth,
  ProjectItem,
  RoutineEventPayload,
  SearchHit,
  SessionSummary,
} from './types'
import ProjectsPane from './components/ProjectsPane.vue'
import SessionsPane from './components/SessionsPane.vue'
import SplitHandle from './components/SplitHandle.vue'
import SessionView from './components/SessionView.vue'
import SettingsDialog from './components/SettingsDialog.vue'
import NativeSetup from './components/NativeSetup.vue'
import WorkbenchPane from './components/WorkbenchPane.vue'
import SessionNav from './components/SessionNav.vue'
import NewSessionPanel from './components/NewSessionPanel.vue'
import SearchPane from './components/SearchPane.vue'
import WorkshopPane from './components/WorkshopPane.vue'
import AgentsPane from './components/AgentsPane.vue'
import DagPane from './components/DagPane.vue'
import AutomationPane from './components/AutomationPane.vue'
import UsagePane from './components/UsagePane.vue'
import RacePane from './components/RacePane.vue'
import HomePane from './components/HomePane.vue'
import AppRail from './components/AppRail.vue'
import { toggleDevtools } from './devtools'
import { VIEW_KEY, isAppView, type AppView } from './nav'
import { lastStartDirectory, lastStartEngine, rememberStart } from './startPrefs'
import { engineChoices } from './sessionStart'
import { mergeRecentSessions, navAgentList, type NavSessionItem } from './sessionNav'
import { currentPage, ensurePageOfAgent } from './workspace'

const health = ref<EngineHealth[]>([])
const error = ref<string | null>(null)
const showSettings = ref(false)

const storedView = localStorage.getItem(VIEW_KEY)
const view = ref<AppView>(isAppView(storedView) ? storedView : 'home')
const workbenchRef = ref<InstanceType<typeof WorkbenchPane> | null>(null)
const wbStatus = ref<WorkbenchStatus>({ running: 0, waiting: 0, runningIds: [], waitingIds: [] })
const archiveStatus = ref<ArchiveStatus>({ running: false, waiting: 0 })

/** 搜索结果定位：切到归档、选中会话并滚动到命中消息。 */
const pendingJump = ref<{ sessionId: string; messageId: string } | null>(null)
const jumpTarget = computed(() =>
  pendingJump.value && pendingJump.value.sessionId === selectedSessionId.value
    ? pendingJump.value.messageId
    : null,
)

async function openSearchHit(hit: SearchHit): Promise<void> {
  setView('archive')
  if (selectedProjectId.value !== hit.projectId) {
    await selectProject(hit.projectId)
  }
  selectSession(hit.sessionId)
  pendingJump.value = hit.messageId
    ? { sessionId: hit.sessionId, messageId: hit.messageId }
    : null
}

function setView(next: AppView, options: { freshStart?: boolean } = {}) {
  // 点工作台（侧栏、设置里的快捷入口）一律先落在两栏「新建会话」页。
  // 「⇥ 工作台」这类明确要开列的，在 setView 之后再切到多列画布。
  if (next === 'workbench' && options.freshStart !== false) {
    workbenchRef.value?.showStartPage(null)
  }
  view.value = next
  try {
    localStorage.setItem(VIEW_KEY, next)
  } catch {
    // 存储失败不影响使用。
  }
}

/** 从设置里的快捷入口跳到对应工作区，先关闭设置。 */
function openSettingsView(next: AppView) {
  showSettings.value = false
  setView(next)
}

// ---- 通知中心（M11）：顶栏铃铛 + 跳转 ----

const notifyOpen = ref(false)

const KIND_ICONS: Record<NotifyItem['kind'], string> = {
  done: '✓',
  failed: '✕',
  permission: '🔐',
}

function toggleNotify() {
  notifyOpen.value = !notifyOpen.value
  if (notifyOpen.value) markAllRead()
}

/** 点击通知：切到归档并定位到对应会话。 */
async function openNotification(item: NotifyItem): Promise<void> {
  notifyOpen.value = false
  if (!item.projectId || !item.sessionId) return
  setView('archive')
  if (selectedProjectId.value !== item.projectId) {
    await selectProject(item.projectId)
  }
  selectSession(item.sessionId)
}

// ---- 例行任务事件（M11b）：运行结果 / 权限请求 → 通知中心 ----

/** 按引擎 + 工作目录定位项目（必要时刷新一次项目列表）。 */
async function resolveProjectId(engine: string, directory: string): Promise<string | null> {
  const find = () =>
    projects.value.find(item => item.engine === engine && item.worktree === directory)
  const known = find()
  if (known) return known.id
  await loadProjects()
  return find()?.id ?? null
}

async function handleRoutineEvent(payload: RoutineEventPayload): Promise<void> {
  const projectId = payload.directory
    ? await resolveProjectId(payload.engine, payload.directory)
    : null
  if (payload.kind === 'permission') {
    pushNotification({
      key: `rt:${payload.routineId}:perm:${payload.requestId ?? payload.startedAt}`,
      kind: 'permission',
      title: `「${payload.name}」等待审批`,
      body: '例行任务被权限请求阻塞，点开处理',
      sessionId: payload.sessionId,
      projectId,
      dedupe: 'key',
    })
    return
  }
  const failed = payload.kind === 'failed'
  pushNotification({
    key: `rt:${payload.routineId}:${payload.startedAt}`,
    kind: failed ? 'failed' : 'done',
    title: `「${payload.name}」例行${failed ? '失败' : '完成'}`,
    body: failed
      ? (payload.error ?? '执行失败')
      : `${engineLabel(payload.engine)}${payload.status === 'interrupted' ? ' · 已打断' : ''}`,
    sessionId: payload.sessionId,
    projectId,
    dedupe: 'key',
  })
}

/** 打开某引擎会话（例行日志「打开会话」）：归档视图定位到项目与会话。 */
async function openSessionByLocation(payload: {
  engine: string
  directory: string
  sessionId: string
}): Promise<void> {
  const projectId = await resolveProjectId(payload.engine, payload.directory)
  if (!projectId) {
    reportError('未找到该会话所在项目（例行工作区可能尚未被引擎收录）')
    return
  }
  setView('archive')
  if (selectedProjectId.value !== projectId) {
    await selectProject(projectId)
  }
  selectSession(payload.sessionId)
}

/** 从归档「⇥ 工作台」按钮打开：开出对应列并切到工作台（落到多列画布）。 */
function openInWorkbench(sessionId: string) {
  const summary = sessions.value.find(item => item.id === sessionId) ?? null
  // 先切视图（别重置当前页），开列会落到这个会话所属 Agent 的页面（没有就新开一页）。
  setView('workbench', { freshStart: false })
  workbenchRef.value?.openColumn(sessionId, summary ?? undefined)
}

const projects = ref<ProjectItem[]>([])
const projectsLoading = ref(false)
const refreshing = ref(false)
const selectedProjectId = ref<string | null>(null)

const sessions = ref<SessionSummary[]>([])
const sessionsLoading = ref(false)
const selectedSessionId = ref<string | null>(null)
const creating = ref(false)
const enginePickerOpen = ref(false)
/** 原生引擎 API 配置弹窗。 */
const nativeSetupOpen = ref(false)
/** 弹窗是从「新建原生会话」进来的：保存后自动继续建。 */
const pendingNativeCreate = ref(false)
const createError = ref<string | null>(null)

const selectedProject = computed(
  () => projects.value.find(project => project.id === selectedProjectId.value) ?? null,
)
const selectedSession = computed(
  () => sessions.value.find(session => session.id === selectedSessionId.value) ?? null,
)

// ---- 托盘状态（M12）：合并工作台 + 归档视图，实时推给菜单栏 ----

const trayCounts = computed(() =>
  mergeTrayCounts(wbStatus.value, archiveStatus.value, selectedSessionId.value),
)
watch(
  trayCounts,
  (counts, previous) => {
    if (previous && previous.running === counts.running && previous.waiting === counts.waiting) {
      return
    }
    void api.trayStatus(counts.running, counts.waiting).catch(() => {
      // 托盘只是状态瞭望窗，失败静默（例如旧内核不带该命令）。
    })
  },
  { immediate: true },
)

function reportError(cause: unknown) {
  error.value = cause instanceof Error ? cause.message : String(cause)
  window.setTimeout(() => {
    if (error.value) error.value = null
  }, 6000)
}

async function loadProjects() {
  projectsLoading.value = true
  try {
    projects.value = await api.listProjects()
    if (!projects.value.some(project => project.id === selectedProjectId.value)) {
      selectedProjectId.value = null
      selectedSessionId.value = null
      sessions.value = []
    }
  } catch (cause) {
    reportError(cause)
  } finally {
    projectsLoading.value = false
  }
}

/**
 * 会话里换了分支：刷新项目列表，让归档里的分支徽标跟着更新。
 * 会话本身属于原工作区，不重载时间线。
 */
function onBranchChanged() {
  // 目录 → 分支的缓存是不失效的正缓存，不主动清就会一直显示切换前的分支。
  void refreshGitInfo(selectedSession.value?.directory ?? selectedProject.value?.worktree)
  void loadProjects()
}

async function selectProject(projectId: string) {
  if (selectedProjectId.value === projectId) return
  selectedProjectId.value = projectId
  selectedSessionId.value = null
  sessions.value = []
  await loadSessions(projectId)
}

/** 首页「项目活跃」点击：切到归档并选中该项目。 */
async function openProjectFromHome(projectId: string) {
  setView('archive')
  if (selectedProjectId.value !== projectId) {
    await selectProject(projectId)
  }
}

async function loadSessions(projectId: string) {
  sessionsLoading.value = true
  try {
    const loaded = await api.listSessions(projectId)
    if (selectedProjectId.value !== projectId) return
    sessions.value = loaded
    const currentStillPresent = loaded.some(session => session.id === selectedSessionId.value)
    if (!currentStillPresent && loaded.length > 0) {
      selectSession(loaded[0].id)
    }
  } catch (cause) {
    reportError(cause)
  } finally {
    sessionsLoading.value = false
  }
}

const delay = (ms: number) => new Promise(resolve => window.setTimeout(resolve, ms))

/** 点「＋」先进入 Agent 选择页，不立刻创建会话。 */
function startCreateSession() {
  if (!selectedProject.value || creating.value) return
  enginePickerOpen.value = true
  createError.value = null
}

function cancelCreateSession() {
  if (creating.value) return
  enginePickerOpen.value = false
  createError.value = null
}

/** 选中 Agent 后，在当前项目目录新建空白会话并选中。 */
async function createSession(engine: string) {
  const project = selectedProject.value
  if (!project || creating.value) return
  // 原生引擎没配 API 时，先弹配置向导；保存后自动继续建会话。
  if (engine === 'native') {
    try {
      const config = await api.nativeConfig()
      if (!config.ready) {
        pendingNativeCreate.value = true
        nativeSetupOpen.value = true
        return
      }
    } catch (cause) {
      createError.value = cause instanceof Error ? cause.message : String(cause)
      return
    }
  }
  creating.value = true
  createError.value = null
  try {
    const created = await api.sessionCreate(project.worktree, engine)
    // 记住这次的选择：下次「新建会话」默认就用它（选择器里标「上次使用」）。
    rememberStart({ engine, directory: project.worktree })
    if (engine === 'native') {
      // 原生会话挂在 `native:<目录>` 这个项目下，和当前项目（可能来自别家）不同，
      // 所以要先把项目列表刷新出来，再切过去选中新会话。
      await loadProjects()
      const nativeProjectId = `native:${project.worktree}`
      if (projects.value.some(item => item.id === nativeProjectId)) {
        await selectProject(nativeProjectId)
      }
      selectedSessionId.value = created.sessionId
      enginePickerOpen.value = false
      return
    }
    // DB 写入可能稍有延迟：重试几次让新会话出现在列表里。
    for (let attempt = 0; attempt < 6; attempt += 1) {
      const loaded = await api.listSessions(project.id)
      if (selectedProjectId.value !== project.id) return
      sessions.value = loaded
      if (loaded.some(session => session.id === created.sessionId)) break
      await delay(250)
    }
    selectedSessionId.value = created.sessionId
    enginePickerOpen.value = false
  } catch (cause) {
    createError.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    creating.value = false
  }
}

/** 原生引擎配置保存后：刷新健康状态，若刚才是在建会话就继续。 */
async function onNativeSaved() {
  nativeSetupOpen.value = false
  const resume = pendingNativeCreate.value
  pendingNativeCreate.value = false
  await refreshEngines()
  if (resume) await createSession('native')
}

function closeNativeSetup() {
  nativeSetupOpen.value = false
  pendingNativeCreate.value = false
}

/** 只刷新引擎健康（原生引擎配置变了，卡片状态要跟着变）。 */
async function refreshEngines() {
  try {
    health.value = await api.engineHealth()
  } catch (cause) {
    reportError(cause)
  }
}

// 切项目时关闭选择页，避免把会话建到旧目录。
watch(selectedProjectId, () => {
  enginePickerOpen.value = false
  createError.value = null
})

function selectSession(sessionId: string) {
  selectedSessionId.value = sessionId
}

/** 一轮执行收口后刷新会话列表（标题 / 用量可能已更新）。 */
async function onSessionSettled() {
  const projectId = selectedProjectId.value
  if (projectId) await loadSessions(projectId)
  // 顶部导航的最近会话也跟着更新（内部有节流，不会每次收口都全量拉）。
  void loadNavSessions()
}

// ---- 顶部会话导航：`<Agent> 的会话` 一个入口 + 一个面板 ----
//
// 口径（用户拍的）：顶栏只有一句话——`<Agent> 的会话`，会话全收在面板里，不摊成一排标签；
// 面板里先挑 Agent，右栏才是它的会话；点会话只让左侧会话栏对应那条激活。

/** 最近会话（跨项目、时间倒序）；打开面板或收口时按节流刷新。 */
const navItems = ref<NavSessionItem[]>([])
const navLoading = ref(false)
let navFetchedAt = 0
const NAV_REFRESH_MS = 10_000

async function loadNavSessions(force = false) {
  if (navLoading.value) return
  if (!force && Date.now() - navFetchedAt < NAV_REFRESH_MS) return
  navLoading.value = true
  try {
    // 项目列表现取：刚用别的引擎 / 别的目录建出来的会话要能立刻出现在这里
    // （App 的 projects 只在启动、切引擎等时机刷新，可能落后一拍）。
    const list = await api.listProjects()
    const groups = await Promise.all(
      list.map(async project => {
        const projectName = project.name || basename(project.worktree)
        try {
          return { projectName, sessions: await api.listSessions(project.id) }
        } catch {
          // 单个项目读失败（引擎库被占等）不该拖垮整张列表：跳过它。
          return { projectName, sessions: [] }
        }
      }),
    )
    navItems.value = mergeRecentSessions(groups)
    navFetchedAt = Date.now()
  } catch {
    // 整体读不到就留着上一次的列表，不弹错。
  } finally {
    navLoading.value = false
  }
}

/** 面板左列：Agent 目录（沿用新会话页那套顺序）+ 各自最近会话数。 */
const navAgents = computed(() =>
  navAgentList(
    navItems.value,
    engineChoices(health.value).map(item => ({ id: item.id, available: item.available })),
  ),
)

/**
 * 顶栏那句 `<Agent> 的会话` 看谁：归档看选中的会话，工作台看当前页，
 * 其余视图就沿用「上次用的 Agent」，都没有就叫「会话」。
 */
const navAgent = computed<string | null>(() => {
  if (view.value === 'archive' && selectedSession.value) return selectedSession.value.engine
  if (view.value === 'workbench') return currentPage.value.agent
  return selectedSession.value?.engine ?? currentPage.value.agent ?? lastStartEngine.value ?? null
})

/** 左侧会话栏当前激活的那条：面板里标成「左侧已选中」。 */
const navSelectedId = computed<string | null>(() =>
  view.value === 'workbench' ? currentPage.value.focusedId : selectedSessionId.value,
)

/** 状态点跨页统计：后台页面里的会话在跑 / 等审批，顶栏与托盘也要显示。 */
const navRunningIds = computed(() => wbStatus.value.runningIds)
const navWaitingIds = computed(() => wbStatus.value.waitingIds)

/**
 * 点面板里的会话：
 * - 归档看会话 → 让左侧项目 / 会话栏那一条激活（右栏看它）；
 * - 工作台（含两栏「新建会话」页）→ 落进本页左栏并选中，**不切多列并排**。
 * 想在多列画布里并排跑，走行尾的 ⇥（显式动作，不会因为点了条会话就悄悄多出一列）。
 */
async function openNavSession(sessionId: string) {
  const summary = navItems.value.find(item => item.session.id === sessionId)?.session ?? null

  if (view.value === 'archive') {
    if (summary && selectedProjectId.value !== summary.projectId) {
      await selectProject(summary.projectId)
    }
    // 项目列表可能落后一拍：列表里没有这条就补进去，别让点了没反应。
    if (summary && !sessions.value.some(session => session.id === sessionId)) {
      sessions.value = [summary, ...sessions.value]
    }
    pendingJump.value = null
    selectSession(sessionId)
    return
  }

  setView('workbench', { freshStart: false })
  await workbenchRef.value?.openInStartPage(sessionId, summary ?? undefined)
}

/** 面板底部：用这个 Agent 开一个新会话（该 Agent 的页面 / 草稿框；没有页面就新开一页）。 */
function navNewSession(agent: string) {
  setView('workbench', { freshStart: false })
  // 新页沿用上次用过的目录（不然还得再挑一次）。
  ensurePageOfAgent(agent, lastStartDirectory.value)
  workbenchRef.value?.showStartPage(null)
}

/** 行尾 ⇥：显式在工作台开一列（多列并排）。 */
function openNavSessionInWorkbench(sessionId: string) {
  setView('workbench', { freshStart: false })
  void workbenchRef.value?.openColumn(sessionId)
}

async function reload() {
  if (refreshing.value) return
  refreshing.value = true
  try {
    try {
      health.value = await api.engineHealth()
    } catch (cause) {
      reportError(cause)
    }
    await loadProjects()
    const first = projects.value[0]
    if (!selectedProjectId.value && first) {
      await selectProject(first.id)
    } else if (selectedProjectId.value) {
      await loadSessions(selectedProjectId.value)
    }
  } finally {
    refreshing.value = false
  }
}

// ---- 三栏宽度：项目栏 / 会话栏可拖拽，宽度记在 localStorage ----

const panesEl = ref<HTMLElement | null>(null)
/** 起手时的宽度快照，拖动全程都以它为基准，避免累积误差。 */
let dragStart: PaneWidths | null = null

function availableWidth(): number {
  return panesEl.value?.clientWidth ?? 0
}

function beginResize(): void {
  dragStart = { ...paneWidths.value }
  document.body.classList.add('resizing')
}

function applyResize(kind: PaneKind, deltaX: number): void {
  setPaneWidths(resizeWidths(kind, dragStart ?? paneWidths.value, deltaX, availableWidth()))
}

function endResize(): void {
  dragStart = null
  document.body.classList.remove('resizing')
  savePaneWidths()
}

function resetResize(kind: PaneKind): void {
  resetPaneWidth(kind)
  // 恢复默认后仍要保证装得下（窗口很窄时按预算再收一次）。
  setPaneWidths(fitWidths(paneWidths.value, availableWidth()))
  savePaneWidths()
}

/** 窗口变窄时把两栏收回来，避免主区域被挤没。 */
function fitPanes(): void {
  const next = fitWidths(paneWidths.value, availableWidth())
  if (next.projects !== paneWidths.value.projects || next.sessions !== paneWidths.value.sessions) {
    setPaneWidths(next)
  }
}

onMounted(() => {
  void reload()
  // 首帧再量一次：窗口比记录值窄时要先把两栏收进来。
  void nextTick(() => fitPanes())
  window.addEventListener('resize', fitPanes)
})

// 例行任务事件（Rust 调度器广播）：通知中心 + 刷新由各视图自行订阅。
const stopRoutineEvents = onRoutineEvent(payload => {
  void handleRoutineEvent(payload).catch(reportError)
})

onBeforeUnmount(() => {
  window.removeEventListener('resize', fitPanes)
  stopRoutineEvents()
})
</script>

<template>
  <div class="app">
    <!-- 顶栏铺满整窗、直接压在 macOS 标题栏上（tauri.conf 的 titleBarStyle=Overlay）：
         左侧 78px 让给红绿灯，其余整条都是拖拽区——按钮 / 输入框自己会挡住拖拽，
         所以 deep 只影响空白处。双击空白处也会照常最大化 / 还原。 -->
    <header class="topbar" data-tauri-drag-region="deep">
      <SessionNav
        :agent="navAgent"
        :recent="navItems"
        :agents="navAgents"
        :selected-id="navSelectedId"
        :running-ids="navRunningIds"
        :waiting-ids="navWaitingIds"
        :loading="navLoading"
        @select="openNavSession"
        @open-in-workbench="openNavSessionInWorkbench"
        @new-session="navNewSession"
        @open="loadNavSessions(true)"
      />

      <div class="topbar-spacer" />

      <div class="notify-wrap">
        <button
          class="notify-btn"
          title="通知：回合完成 / 失败 / 待审批"
          aria-label="通知"
          :aria-expanded="notifyOpen"
          @click="toggleNotify"
        >
          <svg
            class="notify-bell"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="1.7"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <path d="M18 8a6 6 0 0 0-12 0c0 7-3 7-3 9h18c0-2-3-2-3-9" />
            <path d="M13.7 21a2 2 0 0 1-3.4 0" />
          </svg>
          <span v-if="unreadCount" class="notify-badge">
            {{ unreadCount > 99 ? '99+' : unreadCount }}
          </span>
        </button>
        <template v-if="notifyOpen">
          <div class="notify-backdrop" @click="notifyOpen = false" />
          <div class="notify-panel">
            <div class="notify-head">
              <span>通知</span>
              <button
                class="notify-clear"
                :disabled="!notifications.length"
                @click="clearNotifications"
              >
                清空
              </button>
            </div>
            <div v-if="!notifications.length" class="notify-empty">
              暂无通知——回合完成 / 失败 / 待审批会出现在这里
            </div>
            <button
              v-for="item in notifications"
              :key="item.id"
              class="notify-item"
              @click="openNotification(item)"
            >
              <span class="notify-icon" :class="`kind-${item.kind}`">
                {{ KIND_ICONS[item.kind] }}
              </span>
              <span class="notify-main">
                <span class="notify-title">{{ item.title }}</span>
                <span class="notify-body">{{ item.body }}</span>
              </span>
              <span class="notify-time">{{ relativeTime(item.time) }}</span>
            </button>
          </div>
        </template>
      </div>

      <button
        class="refresh-btn"
        :class="{ 'is-busy': refreshing }"
        :disabled="refreshing"
        :aria-busy="refreshing"
        aria-label="重新扫描引擎、项目与会话"
        title="重新扫描引擎、项目与会话"
        @click="reload"
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M20 11a8 8 0 1 1-2.3-5.6" />
          <path d="M20 4v7h-7" />
        </svg>
      </button>

      <button
        class="devtools-btn"
        aria-label="开发者工具（F12）"
        title="开发者工具（F12）"
        @click="toggleDevtools"
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M8 8l-4 4 4 4" />
          <path d="M16 8l4 4-4 4" />
          <path d="M13 6l-2 12" />
        </svg>
      </button>
    </header>

    <div class="app-body">
      <AppRail
        :view="view"
        :wb-status="wbStatus"
        :engines="health"
        @select="setView"
        @settings="showSettings = true"
      />

      <div class="app-main">
        <transition name="fade">
          <div v-if="error" class="error-banner">{{ error }}</div>
        </transition>

        <!-- 组件崩了才会出现：把原因摆出来，别只留一块白屏 -->
        <transition name="fade">
          <div v-if="renderError" class="error-banner render-error" role="alert">
            <span class="render-error-text">界面出错：{{ renderError }}</span>
            <button class="render-error-close" type="button" @click="clearRenderError()">知道了</button>
          </div>
        </transition>

        <div ref="panesEl" class="panes">
          <HomePane
            v-show="view === 'home'"
            :projects="projects"
            @error="reportError"
            @open-project="openProjectFromHome"
            @open-usage="setView('usage')"
          />
          <div v-show="view === 'archive'" class="archive-view">
            <ProjectsPane
              :style="{ width: `${paneWidths.projects}px` }"
              :projects="projects"
              :selected-id="selectedProjectId"
              :loading="projectsLoading"
              @select="selectProject"
            />
            <SplitHandle
              kind="projects"
              :width="paneWidths.projects"
              @start="beginResize"
              @drag="applyResize('projects', $event)"
              @commit="endResize"
              @reset="resetResize('projects')"
            />
            <SessionsPane
              :style="{ width: `${paneWidths.sessions}px` }"
              :sessions="sessions"
              :selected-id="selectedSessionId"
              :loading="sessionsLoading"
              :project="selectedProject"
              :creating="creating"
              @select="selectSession"
              @create="startCreateSession"
            />
            <SplitHandle
              kind="sessions"
              :width="paneWidths.sessions"
              @start="beginResize"
              @drag="applyResize('sessions', $event)"
              @commit="endResize"
              @reset="resetResize('sessions')"
            />
            <NewSessionPanel
              v-if="enginePickerOpen && selectedProject"
              :project="selectedProject"
              :engines="health"
              :busy="creating"
              :error="createError"
              :recent="lastStartEngine"
              @select="createSession"
              @cancel="cancelCreateSession"
            />
            <SessionView
              v-else
              :session="selectedSession"
              :has-project="!!selectedProject"
              :jump-message-id="jumpTarget"
              @error="reportError"
              @settled="onSessionSettled"
              @open-workbench="openInWorkbench"
              @jump-consumed="pendingJump = null"
              @status="archiveStatus = $event"
              @branch-changed="onBranchChanged"
            />
          </div>
          <WorkbenchPane
            v-show="view === 'workbench'"
            ref="workbenchRef"
            :projects="projects"
            :health="health"
            @error="reportError"
            @settled="onSessionSettled"
            @status="wbStatus = $event"
          />
          <SearchPane
            v-show="view === 'search'"
            :projects="projects"
            @error="reportError"
            @open-hit="openSearchHit"
          />
          <RacePane
            v-show="view === 'race'"
            :projects="projects"
            @error="reportError"
            @open-session="openSessionByLocation"
          />
          <AgentsPane v-show="view === 'agents'" @error="reportError" />
          <DagPane v-show="view === 'dag'" :projects="projects" @error="reportError" />
          <UsagePane v-show="view === 'usage'" @error="reportError" />
          <WorkshopPane v-show="view === 'workshop'" @error="reportError" />
          <AutomationPane
            v-show="view === 'automation'"
            @error="reportError"
            @open-session="openSessionByLocation"
          />
        </div>
      </div>
    </div>

    <SettingsDialog
      v-if="showSettings"
      @close="showSettings = false"
      @open-view="openSettingsView"
      @models-changed="refreshEngines"
    />

    <NativeSetup
      v-if="nativeSetupOpen"
      @close="closeNativeSetup"
      @saved="onNativeSaved"
    />
  </div>
</template>

<style scoped src="./styles/components/App.css"></style>
