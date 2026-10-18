<script setup lang="ts">
/**
 * 工作台：两种状态。
 * 1) 多列并排：一屏多列，每列一个会话（完整指挥能力：发送 / 插话 / 停止 / 审批）。
 * 2) 两栏「新建会话」页：左＝当前进行的会话 + 底部目录选择（可新增本机文件夹），右＝选 Agent → 原地变聊天框。
 *    没选目录前右侧 Agent 锁住（工作目录要用户自己挑，不默认替用户选）。
 * 点进工作台（侧栏 / 恢复上次视图）默认落在 2)；1) 只在点「⊞ 并排看」或从归档「⇥ 工作台」开列时出现。
 * 多列始终挂载（两栏页里也照旧收事件），这样左栏列出的会话状态是活的。
 * 布局持久化在 localStorage；列内会话概要按需拉取、收口后刷新。
 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { open as openDirectoryDialog } from '@tauri-apps/plugin-dialog'
import { api } from '../api'
import {
  clampColumnWidth,
  parseWorkbenchLayout,
  restoreColumnAction,
  serializeWorkbenchLayout,
  workbenchShowsStartPage,
  WORKBENCH_DEFAULT_WIDTH,
} from '../workbench'
import { engineChoices } from '../sessionStart'
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

/** 手动新增的目录：还没被引擎收录成项目，选中后照样能在里面建会话。 */
interface ExtraDir {
  id: string
  label: string
  worktree: string
}

const EXTRA_DIR_PREFIX = 'dir:'

const STORAGE_KEY = 'prism-workbench-v1'

const scroller = ref<HTMLElement | null>(null)
const columns = ref<WorkbenchColumn[]>([])
const flashId = ref<string | null>(null)
/**
 * 两栏「新建会话」页是否在前台。
 * 默认 true：点进工作台先看到的就是它（哪怕 localStorage 里存着列）。
 * 只有点「⊞ 并排看」或从归档「⇥ 工作台」开列时才落到多列画布。
 */
const startOpen = ref(true)
/** 右栏显示的会话；null＝选 Agent。 */
const focusedId = ref<string | null>(null)
/** 选中的工作目录：项目 id 或手动新增目录的 id；null＝还没选（右侧 Agent 锁住）。 */
const selectedDirId = ref<string | null>(null)
/** 手动新增、还没被引擎收录成项目的目录。 */
const extraDirs = ref<ExtraDir[]>([])
const creatingSession = ref(false)
const choosingEngine = ref<string | null>(null)
const sessionError = ref<string | null>(null)

/** 选中的目录（项目或手动新增的）；没选就是 null。 */
const selectedDir = computed<ExtraDir | null>(() => {
  const id = selectedDirId.value
  if (!id) return null
  const extra = extraDirs.value.find(item => item.id === id)
  if (extra) return extra
  const project = props.projects.find(item => item.id === id)
  if (!project) return null
  return {
    id: project.id,
    label: project.name || basename(project.worktree),
    worktree: project.worktree,
  }
})

/** 两栏「新建会话」页在前台吗；不在就画多列并排。 */
const startPage = computed(() => workbenchShowsStartPage(startOpen.value, columns.value.length))

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

const pickerHint = computed(() => {
  if (!selectedDir.value) return '先在左边选一个工作目录，或用「＋ 新增」在本机挑一个文件夹'
  if (creatingSession.value) return '正在创建空白会话…'
  return '创建后右栏直接就是聊天框，可以发第一条消息'
})

const pickerDescription = computed(() =>
  selectedDir.value
    ? '工作目录已选好。挑一个 Agent，选完右栏原地变成聊天框。'
    : '还没选工作目录。先在左边选一个，右边才会解锁。',
)

/** 目录下拉：手动新增的排在前面；同路径的项目优先，避免出现两份。 */
const dirOptions = computed(() => [
  ...extraDirs.value
    .filter(extra => !props.projects.some(item => samePath(item.worktree, extra.worktree)))
    .map(extra => ({ value: extra.id, label: extra.label })),
  ...props.projects.map(item => ({
    value: item.id,
    label: item.name || basename(item.worktree),
  })),
])

watch(
  () => props.projects.map(project => `${project.id}\u0000${project.worktree}`).join('\n'),
  () => {
    const id = selectedDirId.value
    if (!id) return
    if (id.startsWith(EXTRA_DIR_PREFIX)) {
      // 新增的目录一旦被引擎收录成项目，就跟着切到项目 id：下拉里不会出现两份。
      const extra = extraDirs.value.find(item => item.id === id)
      const known = extra
        ? props.projects.find(item => samePath(item.worktree, extra.worktree))
        : undefined
      if (known) {
        extraDirs.value = extraDirs.value.filter(item => item.id !== id)
        selectedDirId.value = known.id
      }
      return
    }
    if (!props.projects.some(item => item.id === id)) selectedDirId.value = null
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

/** 目录比路径：忽略结尾的斜杠。 */
function samePath(a: string, b: string): boolean {
  return a.replace(/\/+$/, '') === b.replace(/\/+$/, '')
}

/** 路径提示：太长就只留尾部三段，前面用 … 代替。 */
function shortPath(path: string): string {
  const parts = path.replace(/\/+$/, '').split('/')
  return parts.length <= 4 ? path : `…/${parts.slice(-3).join('/')}`
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

/** 列里会话概要没拉到时原地重试；确认不存在只提示用户关闭，不自动删列。 */
async function retryColumn(sessionId: string) {
  const column = columns.value.find(item => item.sessionId === sessionId)
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

function chooseDir(dirId: string) {
  selectedDirId.value = dirId || null
  sessionError.value = null
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
      selectedDirId.value = known.id
      sessionError.value = null
      return
    }
    const id = `${EXTRA_DIR_PREFIX}${picked}`
    if (!extraDirs.value.some(item => item.id === id)) {
      extraDirs.value = [
        ...extraDirs.value,
        { id, label: basename(picked) || picked, worktree: picked },
      ]
    }
    selectedDirId.value = id
    sessionError.value = null
  } catch (cause) {
    report(cause)
  }
}

async function onEngineSelected(engine: string) {
  const dir = selectedDir.value
  if (!dir || creatingSession.value) return
  creatingSession.value = true
  choosingEngine.value = engine
  sessionError.value = null
  try {
    const created = await api.sessionCreate(dir.worktree, engine)
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

/** 左栏列表第二行：概要还没到手（正在拉 / 这次没拉到）就别说「空闲」。 */
function rowSubText(column: WorkbenchColumn) {
  return column.session ? statusText(column) : '加载中…'
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
      lookup: await lookupSummaryForRestore(item.sessionId),
    })),
  ).then(restored => {
    let dropped = false
    for (const item of restored) {
      // 后端确认这个会话不存在：丢这一条。
      if (restoreColumnAction(item.lookup.kind) === 'drop') {
        dropped = true
        continue
      }
      if (item.lookup.kind === 'error') {
        // 只是这次没拉到：列留着（下面会渲染成「加载中/重试」），原因报到顶栏。
        report(item.lookup.error)
      }
      if (columns.value.some(column => column.sessionId === item.sessionId)) continue
      columns.value.push({
        sessionId: item.sessionId,
        width: item.width,
        session: item.lookup.kind === 'ok' ? item.lookup.summary : null,
        status: { running: false, waiting: 0 },
      })
    }
    // 只有确实丢过列才写回布局；查询失败绝不写——写下去等于替用户把列删了。
    if (dropped) saveLayout()
  })
})

onBeforeUnmount(() => {
  stopResize()
})

defineExpose({ openColumn, showStartPage })
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
          <div class="wb-missing-acts">
            <button class="wb-btn wide" @click="retryColumn(column.sessionId)">重试</button>
            <button class="wb-btn wide" @click="closeColumn(column.sessionId)">关闭这一列</button>
          </div>
        </div>

        <div class="wb-resize" title="拖动调整宽度" @pointerdown="startResize($event, column)" />
      </div>
    </div>

    <!-- 两栏「新建会话」页：左＝目录 + 当前进行的会话，右＝选 Agent → 聊天框。 -->
    <div v-if="startPage" class="wb-start">
      <aside class="wb-start-left" aria-label="当前进行的会话与目录">
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
                {{ rowSubText(column) }}
              </span>
            </span>
          </button>
        </div>

        <div class="wb-start-foot">
          <button
            v-if="columns.length"
            class="wb-start-back"
            title="回到多列并排视图"
            @click="showColumns"
          >
            ⊞ 并排看
          </button>

          <div class="wb-start-dir">
            <div class="wb-start-dir-head">
              <span class="wb-start-label">目录</span>
              <button
                class="wb-start-add"
                title="新增目录：在本机挑一个文件夹"
                @click="addDirectory"
              >
                ＋ 新增
              </button>
            </div>
            <SelectMenu
              :model-value="selectedDirId ?? ''"
              :options="dirOptions"
              placeholder="选择目录…"
              size="sm"
              title="选择工作目录"
              @update:model-value="chooseDir"
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
          <div v-else class="wb-missing">
            <div>会话加载中或已不存在</div>
            <div class="wb-missing-acts">
              <button class="wb-btn wide" @click="retryColumn(focusedColumn.sessionId)">重试</button>
            </div>
          </div>
        </template>

        <EngineChoicePanel
          v-else
          :choices="choices"
          :selecting="choosingEngine"
          :locked="!selectedDir"
          :description="pickerDescription"
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
