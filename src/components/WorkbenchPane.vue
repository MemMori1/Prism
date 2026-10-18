<script setup lang="ts">
/**
 * 工作台：两种状态。
 * 1) 多列并排：一屏多列，每列一个会话（完整指挥能力：发送 / 插话 / 停止 / 审批）。
 * 2) 两栏「新建会话」页：左＝目录选择 + 当前进行的会话，右＝选 Agent → 原地变聊天框。
 * 多列始终挂载（两栏页里也照旧收事件），这样左栏列出的会话状态是活的。
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
import { engineChoices, newestProject } from '../sessionStart'
import { basename } from '../format'
import type { EngineHealth, ProjectItem, SessionSummary } from '../types'
import EngineChoicePanel from './EngineChoicePanel.vue'
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
/** 两栏「新建会话」页是否在前台（点＋才切过去；没开列时无论如何都在前台）。 */
const startOpen = ref(false)
/** 右栏显示的会话；null＝选 Agent。 */
const focusedId = ref<string | null>(null)
const activeProjectId = ref<string | null>(null)
const creatingSession = ref(false)
const choosingEngine = ref<string | null>(null)
const sessionError = ref<string | null>(null)

/** 没有显式选择时，默认用最近活跃的项目。 */
const activeProject = computed(
  () =>
    props.projects.find(project => project.id === activeProjectId.value) ??
    newestProject(props.projects),
)

/** 两栏「新建会话」页在前台吗；不在就画多列并排。 */
const startPage = computed(() => startOpen.value || !columns.value.length)

/** 多列里画哪些列：两栏页聚焦的那一条由右栏画，同一个会话不会出现两个 SessionView。 */
const canvasColumns = computed(() =>
  startPage.value
    ? columns.value.filter(column => column.sessionId !== focusedId.value)
    : columns.value,
)

/** 右栏那一条；null 表示右栏显示选 Agent。 */
const focusedColumn = computed(
  () => columns.value.find(column => column.sessionId === focusedId.value) ?? null,
)

const choices = computed(() => engineChoices(props.health))

const pickerHint = computed(() =>
  creatingSession.value ? '正在创建空白会话…' : '创建后右栏直接就是聊天框，可以发第一条消息',
)

/** 多项目时左栏可切换工作目录。 */
const projectOptions = computed(() => [
  ...(activeProject.value ? [] : [{ value: '', label: '选择项目', disabled: true }]),
  ...props.projects.map(item => ({
    value: item.id,
    label: item.name || basename(item.worktree),
  })),
])

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

// ---- 列的开合 ----

/** 加一列（已存在则只刷新概要），不改变当前看到的是哪一屏。 */
function addColumn(sessionId: string, session?: SessionSummary) {
  const existing = columns.value.find(column => column.sessionId === sessionId)
  if (existing) {
    if (session) existing.session = session
    return
  }
  columns.value.push({
    sessionId,
    width: WORKBENCH_DEFAULT_WIDTH,
    session: session ?? null,
    status: { running: false, waiting: 0 },
  })
  saveLayout()
  if (!session) {
    void fetchSummary(sessionId).then(summary => {
      const column = columns.value.find(item => item.sessionId === sessionId)
      if (column) column.session = summary
    })
  }
}

function closeColumn(sessionId: string) {
  columns.value = columns.value.filter(column => column.sessionId !== sessionId)
  if (focusedId.value === sessionId) focusedId.value = null
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

/** 归档「⇥ 工作台」调用：开一列并直接看多列并排。 */
function openColumn(sessionId: string, session?: SessionSummary) {
  addColumn(sessionId, session)
  startOpen.value = false
  focusedId.value = null
  focusColumn(sessionId)
}

// ---- 两栏「新建会话」页 ----

/** 切到两栏页；focus 为 null 时右栏是选 Agent。 */
function showStartPage(focus: string | null) {
  startOpen.value = true
  focusedId.value = focus
  sessionError.value = null
}

/** 回到多列并排。 */
function showColumns() {
  startOpen.value = false
  focusedId.value = null
  sessionError.value = null
}

/** 左栏点一条会话：右栏换成它的聊天框。 */
function focusSession(sessionId: string) {
  focusedId.value = sessionId
  sessionError.value = null
}

function chooseProject(projectId: string) {
  activeProjectId.value = projectId
  sessionError.value = null
}

async function onEngineSelected(engine: string) {
  const project = activeProject.value
  if (!project || creatingSession.value) return
  creatingSession.value = true
  choosingEngine.value = engine
  sessionError.value = null
  try {
    const created = await api.sessionCreate(project.worktree, engine)
    const summary = await fetchSummaryWithRetry(created.sessionId)
    addColumn(created.sessionId, summary ?? undefined)
    // 右栏原地变成新会话的聊天框，两栏布局保持不变。
    focusedId.value = created.sessionId
    emit('settled')
  } catch (cause) {
    sessionError.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    creatingSession.value = false
    choosingEngine.value = null
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

/** 左栏列表里的状态文案。 */
function statusText(column: WorkbenchColumn) {
  if (column.status.waiting > 0) return `等审批 ${column.status.waiting}`
  if (column.status.running) return '执行中'
  return '空闲'
}

function columnTitle(column: WorkbenchColumn) {
  return column.session?.title || '未命名会话'
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
    <!-- 多列并排：一直挂载，两栏页里也继续收引擎事件，左栏的状态才是活的。 -->
    <div v-show="!startPage" ref="scroller" class="wb-scroll">
      <div
        v-for="column in canvasColumns"
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

    <!-- 两栏「新建会话」页：左＝目录 + 当前进行的会话，右＝选 Agent → 聊天框。 -->
    <div v-if="startPage" class="wb-start">
      <aside class="wb-start-left" aria-label="目录与当前进行的会话">
        <div class="wb-start-field">
          <span class="wb-start-label">目录</span>
          <SelectMenu
            v-if="projects.length > 1"
            :model-value="activeProject?.id ?? ''"
            :options="projectOptions"
            size="sm"
            inline
            title="选择工作目录"
            max-width="100%"
            @update:model-value="chooseProject"
          />
          <span v-else-if="activeProject" class="wb-start-project" :title="activeProject.worktree">
            <span class="wb-dot tone-idle" />
            {{ activeProject.name || basename(activeProject.worktree) }}
          </span>
        </div>

        <button
          class="wb-start-new"
          :class="{ on: !focusedId }"
          title="选一个 Agent 新建会话"
          @click="showStartPage(null)"
        >
          ＋ 新建会话
        </button>

        <div class="wb-start-head">
          <span>当前进行的会话</span>
          <span v-if="columns.length" class="wb-start-count">{{ columns.length }}</span>
        </div>

        <div class="wb-start-list">
          <p v-if="!columns.length" class="wb-start-empty">
            还没有打开的会话——选一个 Agent 建一个，或在归档里点「工作台」
          </p>
          <button
            v-for="column in columns"
            :key="column.sessionId"
            class="wb-start-item"
            :class="{ active: column.sessionId === focusedId }"
            :title="columnTitle(column)"
            @click="focusSession(column.sessionId)"
          >
            <span class="wb-dot" :class="dotClass(column)" :title="dotTitle(column)" />
            <span class="wb-start-item-main">
              <span class="wb-start-item-title">{{ columnTitle(column) }}</span>
              <span class="wb-start-item-sub" :class="dotClass(column)">
                {{ statusText(column) }}
              </span>
            </span>
          </button>
        </div>

        <button
          v-if="columns.length"
          class="wb-start-back"
          title="回到多列并排视图"
          @click="showColumns"
        >
          ⊞ 并排看
        </button>
      </aside>

      <div class="wb-start-right">
        <template v-if="focusedColumn">
          <SessionView
            v-if="focusedColumn.session"
            embedded
            :session="focusedColumn.session"
            :has-project="true"
            @error="report"
            @settled="onColumnSettled(focusedColumn.sessionId)"
            @status="onColumnStatus(focusedColumn.sessionId, $event)"
          />
          <div v-else class="wb-missing">会话加载中或已不存在</div>
        </template>

        <EngineChoicePanel
          v-else
          :choices="choices"
          :selecting="choosingEngine"
          :hint="pickerHint"
          :error="sessionError"
          @select="onEngineSelected"
        />
      </div>
    </div>

    <button
      v-if="!startPage"
      class="wb-add"
      title="新建会话并开一列"
      @click="showStartPage(null)"
    >
      ＋ 新建会话
    </button>
  </section>
</template>

<style scoped src="../styles/components/WorkbenchPane.css"></style>
