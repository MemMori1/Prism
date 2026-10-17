<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, computed, watch } from 'vue'
import { api } from './api'
import { relativeTime } from './format'
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
import type {
  EngineHealth,
  ProjectItem,
  RoutineEventPayload,
  SearchHit,
  SessionSummary,
} from './types'
import ProjectsPane from './components/ProjectsPane.vue'
import SessionsPane from './components/SessionsPane.vue'
import SessionView from './components/SessionView.vue'
import SettingsDialog from './components/SettingsDialog.vue'
import WorkbenchPane from './components/WorkbenchPane.vue'
import SearchPane from './components/SearchPane.vue'
import WorkshopPane from './components/WorkshopPane.vue'
import AutomationPane from './components/AutomationPane.vue'
import UsagePane from './components/UsagePane.vue'
import RacePane from './components/RacePane.vue'
import HomePane from './components/HomePane.vue'
import AppRail from './components/AppRail.vue'
import { VIEW_KEY, isAppView, type AppView } from './nav'

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

function setView(next: AppView) {
  view.value = next
  try {
    localStorage.setItem(VIEW_KEY, next)
  } catch {
    // 存储失败不影响使用。
  }
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

/** 从归档「⇥ 工作台」按钮打开：开出对应列并切到工作台。 */
function openInWorkbench(sessionId: string) {
  const summary = sessions.value.find(item => item.id === sessionId) ?? null
  workbenchRef.value?.openColumn(sessionId, summary ?? undefined)
  setView('workbench')
}

const projects = ref<ProjectItem[]>([])
const projectsLoading = ref(false)
const selectedProjectId = ref<string | null>(null)

const sessions = ref<SessionSummary[]>([])
const sessionsLoading = ref(false)
const selectedSessionId = ref<string | null>(null)
const creating = ref(false)

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

type EngineTone = 'muted' | 'ok' | 'warn' | 'error'

function engineStatus(item: EngineHealth): { label: string; tone: EngineTone } {
  if (item.ready) return { label: '已就绪', tone: 'ok' }
  if (item.databasePath) return { label: '只读（未找到 CLI）', tone: 'warn' }
  return { label: '未安装', tone: 'error' }
}

const engineChips = computed(() =>
  health.value.map(item => ({ ...item, status: engineStatus(item) })),
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

/** 在当前项目目录新建会话并选中（首次会顺带启动运行轴）。 */
async function createSession() {
  const project = selectedProject.value
  if (!project || creating.value) return
  creating.value = true
  try {
    const created = await api.sessionCreate(project.worktree, project.engine)
    // DB 写入可能稍有延迟：重试几次让新会话出现在列表里。
    for (let attempt = 0; attempt < 6; attempt += 1) {
      const loaded = await api.listSessions(project.id)
      if (selectedProjectId.value !== project.id) return
      sessions.value = loaded
      if (loaded.some(session => session.id === created.sessionId)) break
      await delay(250)
    }
    selectedSessionId.value = created.sessionId
  } catch (cause) {
    reportError(cause)
  } finally {
    creating.value = false
  }
}

function selectSession(sessionId: string) {
  selectedSessionId.value = sessionId
}

/** 一轮执行收口后刷新会话列表（标题 / 用量可能已更新）。 */
async function onSessionSettled() {
  const projectId = selectedProjectId.value
  if (projectId) await loadSessions(projectId)
}

async function reload() {
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
}

onMounted(() => {
  void reload()
})

// 例行任务事件（Rust 调度器广播）：通知中心 + 刷新由各视图自行订阅。
const stopRoutineEvents = onRoutineEvent(payload => {
  void handleRoutineEvent(payload).catch(reportError)
})

onBeforeUnmount(stopRoutineEvents)
</script>

<template>
  <div class="app">
    <AppRail
      :view="view"
      :wb-status="wbStatus"
      @select="setView"
      @settings="showSettings = true"
    />

    <div class="app-main">
      <header class="topbar">
        <div class="brand">
          <svg class="brand-mark" viewBox="0 0 64 64" aria-hidden="true">
            <path d="M22 14 L42 50 L2 50 Z" fill="none" stroke="currentColor" stroke-width="4" stroke-linejoin="round" />
            <path d="M0 33 H21" stroke="currentColor" stroke-width="4" stroke-linecap="round" />
            <path d="M39 27 L62 16" stroke="#8b5cf6" stroke-width="3.6" stroke-linecap="round" />
            <path d="M41 33 H62" stroke="#22d3ee" stroke-width="3.6" stroke-linecap="round" />
            <path d="M39 39 L62 50" stroke="#f472b6" stroke-width="3.6" stroke-linecap="round" />
          </svg>
          <span class="brand-name">Prism</span>
        </div>

        <button
          v-for="item in engineChips"
          :key="item.engineId"
          class="engine-chip"
          :class="`tone-${item.status.tone}`"
          :title="`${item.cliPath ?? '未找到 CLI'}\n点击查看引擎详情`"
          @click="showSettings = true"
        >
          <span class="dot" />
          <span>{{ item.displayName }} · {{ item.status.label }}</span>
        </button>

        <div class="topbar-spacer" />

        <div class="notify-wrap">
          <button
            class="notify-btn"
            title="通知：回合完成 / 失败 / 待审批"
            @click="toggleNotify"
          >
            🔔
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

        <button class="refresh-btn" :disabled="projectsLoading" @click="reload">刷新</button>
      </header>

      <transition name="fade">
        <div v-if="error" class="error-banner">{{ error }}</div>
      </transition>

      <div class="panes">
        <HomePane
          v-show="view === 'home'"
          :projects="projects"
          @error="reportError"
          @open-project="openProjectFromHome"
          @open-usage="setView('usage')"
        />
        <div v-show="view === 'archive'" class="archive-view">
          <ProjectsPane
            :projects="projects"
            :selected-id="selectedProjectId"
            :loading="projectsLoading"
            @select="selectProject"
          />
          <SessionsPane
            :sessions="sessions"
            :selected-id="selectedSessionId"
            :loading="sessionsLoading"
            :project="selectedProject"
            :creating="creating"
            @select="selectSession"
            @create="createSession"
          />
          <SessionView
            :session="selectedSession"
            :has-project="!!selectedProject"
            :jump-message-id="jumpTarget"
            @error="reportError"
            @settled="onSessionSettled"
            @open-workbench="openInWorkbench"
            @jump-consumed="pendingJump = null"
            @status="archiveStatus = $event"
          />
        </div>
        <WorkbenchPane
          v-show="view === 'workbench'"
          ref="workbenchRef"
          :projects="projects"
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
        <UsagePane v-show="view === 'usage'" @error="reportError" />
        <WorkshopPane v-show="view === 'workshop'" @error="reportError" />
        <AutomationPane
          v-show="view === 'automation'"
          @error="reportError"
          @open-session="openSessionByLocation"
        />
      </div>
    </div>

    <SettingsDialog v-if="showSettings" @close="showSettings = false" />
  </div>
</template>

<style scoped>
.app {
  display: flex;
  flex-direction: row;
  height: 100%;
  min-height: 0;
}

/* 图标栏贯穿整个窗口高度；顶栏与内容在右侧同列。 */
.app-main {
  display: flex;
  flex: 1;
  flex-direction: column;
  min-width: 0;
  min-height: 0;
}

.topbar {
  display: flex;
  align-items: center;
  gap: 12px;
  height: 46px;
  padding: 0 14px;
  border-bottom: 1px solid var(--border);
  background: var(--panel);
  flex: none;
}

.brand {
  display: flex;
  align-items: center;
  gap: 8px;
}

.brand-mark {
  width: 20px;
  height: 20px;
  color: var(--text);
}

.brand-name {
  font-weight: 650;
  font-size: 14px;
  letter-spacing: 0.2px;
}


.engine-chip {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 3px 10px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: var(--panel-2);
  color: var(--muted);
  font-size: 12px;
  transition: all 0.15s ease;
}

.engine-chip:hover {
  border-color: var(--border-strong);
  color: var(--text);
}

.engine-chip .dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--faint);
}

.engine-chip.tone-ok .dot {
  background: var(--green);
}

.engine-chip.tone-warn .dot {
  background: var(--amber);
}

.engine-chip.tone-error .dot {
  background: var(--red);
}

.topbar-spacer {
  flex: 1;
}

.refresh-btn {
  padding: 4px 12px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--border);
  background: var(--panel-2);
  color: var(--muted);
  transition: all 0.15s ease;
}

.refresh-btn:hover:not(:disabled) {
  color: var(--text);
  border-color: var(--border-strong);
}

.refresh-btn:disabled {
  opacity: 0.5;
  cursor: default;
}


/* ---- 通知中心（M11） ---- */

.notify-wrap {
  position: relative;
}

.notify-btn {
  position: relative;
  padding: 4px 10px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--border);
  background: var(--panel-2);
  color: var(--muted);
  font-size: 12px;
  line-height: 1.4;
  transition: all 0.15s ease;
}

.notify-btn:hover {
  color: var(--text);
  border-color: var(--border-strong);
}

.notify-badge {
  position: absolute;
  top: -6px;
  right: -6px;
  min-width: 15px;
  padding: 0 4px;
  border-radius: 999px;
  background: var(--red);
  color: #fff;
  font-size: 10px;
  line-height: 15px;
  font-weight: 700;
}

.notify-backdrop {
  position: fixed;
  inset: 0;
  z-index: 30;
}

.notify-panel {
  position: absolute;
  top: calc(100% + 8px);
  right: 0;
  z-index: 31;
  width: 330px;
  max-height: 60vh;
  overflow-y: auto;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--panel);
  box-shadow: var(--shadow);
}

.notify-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 8px 12px;
  border-bottom: 1px solid var(--border);
  font-size: 12px;
  font-weight: 650;
  color: var(--text);
}

.notify-clear {
  color: var(--muted);
  font-size: 11px;
}

.notify-clear:hover:not(:disabled) {
  color: var(--text);
}

.notify-clear:disabled {
  opacity: 0.45;
  cursor: default;
}

.notify-empty {
  padding: 14px 12px;
  color: var(--faint);
  font-size: 12px;
  text-align: center;
}

.notify-item {
  display: flex;
  align-items: flex-start;
  gap: 8px;
  width: 100%;
  padding: 8px 12px;
  text-align: left;
  border-bottom: 1px solid color-mix(in srgb, var(--border) 55%, transparent);
  transition: background 0.12s ease;
}

.notify-item:hover {
  background: var(--panel-2);
}

.notify-item:last-child {
  border-bottom: none;
}

.notify-icon {
  flex: none;
  width: 16px;
  text-align: center;
  font-size: 12px;
}

.notify-icon.kind-done {
  color: var(--green);
}

.notify-icon.kind-failed {
  color: var(--red);
}

.notify-icon.kind-permission {
  color: var(--amber);
}

.notify-main {
  display: flex;
  flex-direction: column;
  gap: 2px;
  flex: 1;
  min-width: 0;
}

.notify-title {
  font-size: 12px;
  color: var(--text);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.notify-body {
  font-size: 11px;
  color: var(--muted);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.notify-time {
  flex: none;
  padding-top: 1px;
  font-size: 11px;
  color: var(--faint);
}

.error-banner {
  position: absolute;
  top: 54px;
  left: 50%;
  transform: translateX(-50%);
  z-index: 20;
  max-width: 70%;
  padding: 8px 16px;
  border-radius: var(--radius);
  background: var(--red-soft);
  color: var(--red);
  border: 1px solid color-mix(in srgb, var(--red) 30%, transparent);
  box-shadow: var(--shadow);
  font-size: 12px;
}

.panes {
  display: flex;
  flex: 1;
  min-width: 0;
  min-height: 0;
}

.archive-view {
  display: flex;
  flex: 1;
  min-width: 0;
  min-height: 0;
}

.fade-enter-active,
.fade-leave-active {
  transition: opacity 0.2s ease;
}

.fade-enter-from,
.fade-leave-to {
  opacity: 0;
}
</style>
