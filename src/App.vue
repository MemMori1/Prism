<script setup lang="ts">
import { onMounted, ref, computed } from 'vue'
import { api } from './api'
import type { EngineHealth, ProjectItem, SearchHit, SessionSummary } from './types'
import ProjectsPane from './components/ProjectsPane.vue'
import SessionsPane from './components/SessionsPane.vue'
import SessionView from './components/SessionView.vue'
import SettingsDialog from './components/SettingsDialog.vue'
import WorkbenchPane from './components/WorkbenchPane.vue'
import SearchPane from './components/SearchPane.vue'

const health = ref<EngineHealth[]>([])
const error = ref<string | null>(null)
const showSettings = ref(false)

type AppView = 'archive' | 'workbench' | 'search'
const VIEW_KEY = 'prism-view'
const storedView = localStorage.getItem(VIEW_KEY)
const view = ref<AppView>(
  storedView === 'workbench' || storedView === 'search' ? storedView : 'archive',
)
const workbenchRef = ref<InstanceType<typeof WorkbenchPane> | null>(null)
const wbStatus = ref<{ running: number; waiting: number }>({ running: 0, waiting: 0 })

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

onMounted(reload)
</script>

<template>
  <div class="app">
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

      <div class="view-switch">
        <button
          class="view-btn"
          :class="{ active: view === 'archive' }"
          title="历史浏览：项目 / 会话 / 时间线"
          @click="setView('archive')"
        >
          归档
        </button>
        <button
          class="view-btn"
          :class="{ active: view === 'workbench' }"
          title="多列并行指挥：同时盯着多个跑着的会话"
          @click="setView('workbench')"
        >
          工作台
          <span
            v-if="wbStatus.waiting"
            class="switch-badge"
            :title="`${wbStatus.waiting} 个权限请求等待审批`"
          >
            {{ wbStatus.waiting }}
          </span>
          <span
            v-else-if="wbStatus.running"
            class="switch-dot"
            :title="`${wbStatus.running} 个会话执行中`"
          />
        </button>
        <button
          class="view-btn"
          :class="{ active: view === 'search' }"
          title="全文搜索：检索全部会话内容（本地索引，支持中文子串）"
          @click="setView('search')"
        >
          搜索
        </button>
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

      <button class="settings-btn" title="设置" @click="showSettings = true">⚙</button>
      <button class="refresh-btn" :disabled="projectsLoading" @click="reload">刷新</button>
    </header>

    <transition name="fade">
      <div v-if="error" class="error-banner">{{ error }}</div>
    </transition>

    <div class="panes">
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
    </div>

    <SettingsDialog v-if="showSettings" @close="showSettings = false" />
  </div>
</template>

<style scoped>
.app {
  display: flex;
  flex-direction: column;
  height: 100%;
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

.view-switch {
  display: flex;
  align-items: center;
  gap: 2px;
  padding: 2px;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--panel-2);
}

.view-btn {
  display: flex;
  align-items: center;
  gap: 5px;
  padding: 3px 12px;
  border-radius: 5px;
  color: var(--muted);
  font-size: 12px;
  font-weight: 600;
  transition: all 0.15s ease;
}

.view-btn:hover {
  color: var(--text);
}

.view-btn.active {
  background: var(--panel);
  color: var(--text);
  box-shadow: 0 1px 2px rgb(18 16 34 / 0.08);
}

.switch-badge {
  padding: 0 6px;
  border-radius: 999px;
  background: var(--amber);
  color: #fff;
  font-size: 10px;
  line-height: 15px;
  font-weight: 700;
}

.switch-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--green);
  animation: switch-pulse 1.6s ease infinite;
}

@keyframes switch-pulse {
  50% {
    opacity: 0.35;
  }
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

.settings-btn {
  padding: 4px 10px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--border);
  background: var(--panel-2);
  color: var(--muted);
  font-size: 12px;
  line-height: 1.4;
  transition: all 0.15s ease;
}

.settings-btn:hover {
  color: var(--text);
  border-color: var(--border-strong);
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
