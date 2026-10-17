<script setup lang="ts">
/**
 * 工作台：一屏多列，每列一个会话（完整指挥能力：发送 / 插话 / 停止 / 审批）。
 * 布局持久化在 localStorage；列内会话概要按需拉取、收口后刷新。
 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { api } from '../api'
import {
  clampColumnWidth,
  parseWorkbenchLayout,
  serializeWorkbenchLayout,
  WORKBENCH_DEFAULT_WIDTH,
} from '../workbench'
import type { EngineHealth, ProjectItem, SessionSummary } from '../types'
import { newestProject } from '../sessionStart'
import NewSessionPanel from './NewSessionPanel.vue'
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

interface WorkbenchColumn {
  sessionId: string
  width: number
  session: SessionSummary | null
  status: { running: boolean; waiting: number }
}

const STORAGE_KEY = 'prism-workbench-v1'

const scroller = ref<HTMLElement | null>(null)
const columns = ref<WorkbenchColumn[]>([])
const flashId = ref<string | null>(null)
const pickerOpen = ref(false)
const activeProjectId = ref<string | null>(null)
const creatingSession = ref(false)
const sessionError = ref<string | null>(null)

/** 没有显式选择时，默认用最近活跃的项目。 */
const activeProject = computed(
  () =>
    props.projects.find(project => project.id === activeProjectId.value) ??
    newestProject(props.projects),
)

watch(
  () => props.projects.map(project => project.id).join('\n'),
  () => {
    if (activeProjectId.value && !props.projects.some(item => item.id === activeProjectId.value)) {
      activeProjectId.value = null
    }
  },
)

const runningCount = computed(() => columns.value.filter(column => column.status.running).length)
const waitingCount = computed(() =>
  columns.value.reduce((sum, column) => sum + column.status.waiting, 0),
)
const runningIds = computed(() =>
  columns.value.filter(column => column.status.running).map(column => column.sessionId),
)
const waitingIds = computed(() =>
  columns.value.filter(column => column.status.waiting > 0).map(column => column.sessionId),
)

// 状态汇总上报（顶栏角标 + 托盘用；id 列表供托盘与归档视图去重）。
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

function sleep(ms: number) {
  return new Promise(resolve => window.setTimeout(resolve, ms))
}

// ---- 布局持久化 ----

function saveLayout() {
  const data = columns.value.map(column => ({
    sessionId: column.sessionId,
    width: column.width,
  }))
  try {
    localStorage.setItem(STORAGE_KEY, serializeWorkbenchLayout(data))
  } catch {
    // 存储失败不影响使用。
  }
}

function loadLayout() {
  try {
    return parseWorkbenchLayout(localStorage.getItem(STORAGE_KEY))
  } catch {
    return []
  }
}

// ---- 会话概要 ----

async function fetchSummary(sessionId: string): Promise<SessionSummary | null> {
  try {
    return await api.sessionSummary(sessionId)
  } catch (cause) {
    report(cause)
    return null
  }
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

// ---- 打开 / 关闭列 ----

function openColumn(sessionId: string, session?: SessionSummary) {
  const existing = columns.value.find(column => column.sessionId === sessionId)
  if (existing) {
    if (session) existing.session = session
    focusColumn(sessionId)
    return
  }
  columns.value.push({
    sessionId,
    width: WORKBENCH_DEFAULT_WIDTH,
    session: session ?? null,
    status: { running: false, waiting: 0 },
  })
  saveLayout()
  focusColumn(sessionId)
  if (!session) {
    void fetchSummary(sessionId).then(summary => {
      const column = columns.value.find(item => item.sessionId === sessionId)
      if (column) column.session = summary
    })
  }
}

function closeColumn(sessionId: string) {
  columns.value = columns.value.filter(column => column.sessionId !== sessionId)
  saveLayout()
}

/** 让某列滚进视野并闪一下，提示「就是这一列」。 */
function focusColumn(sessionId: string) {
  flashId.value = sessionId
  window.setTimeout(() => {
    if (flashId.value === sessionId) flashId.value = null
  }, 1200)
  void nextTick(() => {
    const el = scroller.value?.querySelector(`[data-session-id="${sessionId}"]`)
    el?.scrollIntoView({ behavior: 'smooth', inline: 'center', block: 'nearest' })
  })
}

// ---- 新建会话：先选 Agent，再建空白会话 ----

function chooseProject(projectId: string) {
  activeProjectId.value = projectId
  sessionError.value = null
}

async function onEngineSelected(engine: string) {
  const project = activeProject.value
  if (!project || creatingSession.value) return
  creatingSession.value = true
  sessionError.value = null
  try {
    const created = await api.sessionCreate(project.worktree, engine)
    const summary = await fetchSummaryWithRetry(created.sessionId)
    if (summary) openColumn(created.sessionId, summary)
    else openColumn(created.sessionId)
    pickerOpen.value = false
    emit('settled')
  } catch (cause) {
    sessionError.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    creatingSession.value = false
  }
}

// ---- 列宽拖拽 ----

let resizeState: { sessionId: string; startX: number; startWidth: number } | null = null

function startResize(event: PointerEvent, column: WorkbenchColumn) {
  event.preventDefault()
  resizeState = { sessionId: column.sessionId, startX: event.clientX, startWidth: column.width }
  window.addEventListener('pointermove', onResizeMove)
  window.addEventListener('pointerup', stopResize)
  document.body.style.cursor = 'col-resize'
  document.body.style.userSelect = 'none'
}

function onResizeMove(event: PointerEvent) {
  if (!resizeState) return
  const column = columns.value.find(item => item.sessionId === resizeState?.sessionId)
  if (!column) return
  column.width = clampColumnWidth(resizeState.startWidth + (event.clientX - resizeState.startX))
}

function stopResize() {
  resizeState = null
  window.removeEventListener('pointermove', onResizeMove)
  window.removeEventListener('pointerup', stopResize)
  document.body.style.cursor = ''
  document.body.style.userSelect = ''
  saveLayout()
}

// ---- 列内事件 ----

function onColumnStatus(sessionId: string, status: { running: boolean; waiting: number }) {
  const column = columns.value.find(item => item.sessionId === sessionId)
  if (column) column.status = status
}

async function onColumnSettled(sessionId: string) {
  const column = columns.value.find(item => item.sessionId === sessionId)
  if (column) {
    const summary = await fetchSummary(sessionId)
    if (summary) column.session = summary
  }
  emit('settled')
}

function dotClass(column: WorkbenchColumn) {
  if (column.status.waiting > 0) return 'tone-wait'
  if (column.status.running) return 'tone-run'
  return 'tone-idle'
}

function dotTitle(column: WorkbenchColumn) {
  if (column.status.waiting > 0) return '等待审批'
  if (column.status.running) return '执行中'
  return '空闲'
}

// ---- 恢复布局 ----

onMounted(() => {
  const saved = loadLayout()
  if (!saved.length) return
  void Promise.all(
    saved.map(async item => ({
      ...item,
      summary: await fetchSummary(item.sessionId),
    })),
  ).then(restored => {
    for (const item of restored) {
      if (!item.summary) continue // 会话已不存在：静默丢弃。
      if (columns.value.some(column => column.sessionId === item.sessionId)) continue
      columns.value.push({
        sessionId: item.sessionId,
        width: item.width,
        session: item.summary,
        status: { running: false, waiting: 0 },
      })
    }
    saveLayout()
  })
})

onBeforeUnmount(() => {
  stopResize()
})

defineExpose({ openColumn })
</script>

<template>
  <section class="workbench">
    <div v-if="columns.length && !pickerOpen" ref="scroller" class="wb-scroll">
      <div
        v-for="column in columns"
        :key="column.sessionId"
        class="wb-column"
        :class="{ flash: flashId === column.sessionId }"
        :data-session-id="column.sessionId"
        :style="{ width: `${column.width}px` }"
      >
        <header class="wb-head">
          <span class="wb-dot" :class="dotClass(column)" :title="dotTitle(column)" />
          <span class="wb-title" :title="column.session?.title || '未命名会话'">
            {{ column.session?.title || '未命名会话' }}
          </span>
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
          embedded
          :session="column.session"
          :has-project="true"
          @error="report"
          @settled="onColumnSettled(column.sessionId)"
          @status="onColumnStatus(column.sessionId, $event)"
        />
        <div v-else class="wb-missing">
          <div>会话加载中或已不存在</div>
          <button class="wb-btn wide" @click="closeColumn(column.sessionId)">关闭这一列</button>
        </div>

        <div class="wb-resize" title="拖动调整宽度" @pointerdown="startResize($event, column)" />
      </div>
    </div>

    <div v-else class="wb-picker">
      <NewSessionPanel
        :project="activeProject"
        :projects="projects"
        :engines="health"
        :busy="creatingSession"
        :error="sessionError"
        :cancelable="columns.length > 0"
        @select="onEngineSelected"
        @project="chooseProject"
        @cancel="pickerOpen = false"
      />
    </div>

    <button
      v-if="columns.length && !pickerOpen"
      class="wb-add"
      title="新建会话并开一列"
      @click="pickerOpen = true"
    >
      ＋ 新建会话
    </button>
  </section>
</template>

<style scoped src="../styles/components/WorkbenchPane.css"></style>
