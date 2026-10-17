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
import type { ProjectItem, SessionSummary } from '../types'
import SessionComposer from './SessionComposer.vue'
import SessionView from './SessionView.vue'

const props = defineProps<{
  projects: ProjectItem[]
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

// ---- 新建会话 ----

async function onComposerCreated(sessionId: string) {
  pickerOpen.value = false
  const summary = await fetchSummaryWithRetry(sessionId)
  if (summary) openColumn(sessionId, summary)
  else openColumn(sessionId)
  emit('settled')
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
    <div ref="scroller" class="wb-scroll">
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

      <div v-if="!columns.length" class="wb-empty">
        <div class="wb-empty-mark">▦</div>
        <div class="wb-empty-title">工作台还没有会话</div>
        <div class="wb-empty-hint">
          一屏多列并行盯盘：这里新建一个，或去归档里点会话右上角「⇥ 工作台」
        </div>
        <button class="new-session-btn" @click="pickerOpen = true">新建会话</button>
      </div>
    </div>

    <button
      v-if="columns.length"
      class="wb-add"
      title="新建会话并开一列"
      @click="pickerOpen = true"
    >
      ＋ 新建会话
    </button>

    <SessionComposer
      v-if="pickerOpen"
      :projects="projects"
      @close="pickerOpen = false"
      @created="onComposerCreated"
      @error="report"
    />
  </section>
</template>

<style scoped>
.workbench {
  position: relative;
  display: flex;
  flex: 1;
  min-width: 0;
  min-height: 0;
  background: var(--bg);
}

.wb-scroll {
  display: flex;
  align-items: stretch;
  gap: 10px;
  flex: 1;
  min-height: 0;
  overflow-x: auto;
  padding: 10px;
}

.wb-column {
  position: relative;
  flex: none;
  display: flex;
  flex-direction: column;
  min-height: 0;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--panel);
  overflow: hidden;
  transition: box-shadow 0.25s ease, border-color 0.25s ease;
}

.wb-column.flash {
  border-color: var(--accent);
  box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent) 30%, transparent);
}

.wb-head {
  display: flex;
  align-items: center;
  gap: 8px;
  flex: none;
  padding: 7px 10px;
  border-bottom: 1px solid var(--border);
  background: var(--panel-2);
}

.wb-dot {
  flex: none;
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--faint);
}

.wb-dot.tone-run {
  background: var(--green);
  animation: wb-pulse 1.6s ease infinite;
}

.wb-dot.tone-wait {
  background: var(--amber);
  animation: wb-pulse 1.1s ease infinite;
}

@keyframes wb-pulse {
  50% {
    opacity: 0.4;
  }
}

.wb-title {
  flex: 1;
  min-width: 0;
  font-size: 12px;
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.wb-waiting {
  flex: none;
  padding: 0 7px;
  border-radius: 999px;
  border: 1px solid color-mix(in srgb, var(--amber) 40%, transparent);
  background: var(--amber-soft);
  color: var(--amber);
  font-size: 10.5px;
  line-height: 16px;
}

.wb-btn {
  flex: none;
  width: 20px;
  height: 20px;
  border-radius: 5px;
  color: var(--faint);
  font-size: 11px;
  line-height: 1;
}

.wb-btn:hover {
  background: var(--panel);
  color: var(--text);
}

.wb-btn.wide {
  width: auto;
  height: auto;
  padding: 4px 12px;
  border: 1px solid var(--border);
  font-size: 11.5px;
}

.wb-missing {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 10px;
  color: var(--faint);
  font-size: 12px;
}

.wb-resize {
  position: absolute;
  top: 0;
  right: -5px;
  bottom: 0;
  width: 10px;
  cursor: col-resize;
  z-index: 6;
}

.wb-resize:hover {
  background: color-mix(in srgb, var(--accent) 25%, transparent);
}

.wb-empty {
  margin: auto;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  padding: 40px;
  color: var(--faint);
}

.wb-empty-mark {
  font-size: 30px;
  color: var(--border-strong);
}

.wb-empty-title {
  font-size: 14px;
  font-weight: 600;
  color: var(--muted);
}

.wb-empty-hint {
  max-width: 320px;
  text-align: center;
  font-size: 11.5px;
  line-height: 1.6;
}

.new-session-btn {
  margin-top: 6px;
  padding: 6px 16px;
  border-radius: var(--radius-sm);
  background: var(--accent);
  color: #fff;
  font-size: 12px;
  font-weight: 600;
}

.new-session-btn:hover:not(:disabled) {
  opacity: 0.9;
}

.new-session-btn:disabled {
  opacity: 0.5;
  cursor: default;
}

.wb-add {
  position: absolute;
  top: 18px;
  right: 20px;
  z-index: 5;
  padding: 5px 12px;
  border-radius: var(--radius-sm);
  background: var(--accent);
  color: #fff;
  font-size: 12px;
  font-weight: 600;
  box-shadow: var(--shadow);
}

.wb-add:hover:not(:disabled) {
  opacity: 0.9;
}

.wb-add:disabled {
  opacity: 0.5;
  cursor: default;
}
</style>
