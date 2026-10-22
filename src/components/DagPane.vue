<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { listen } from '@tauri-apps/api/event'
import { api } from '../api'
import { NODE_H, NODE_W, edgePath, withOverrides } from '../dagLayout'
import { executorBadge, permissionLabel, workspaceLabel } from '../agents'
import { formatDuration, formatTokens } from '../format'
import {
  isLive,
  nodeElapsed,
  nodeMeta,
  nodeRunOf,
  runBlocker,
  runElapsed,
  runMeta,
  runProgress,
  runSummary,
  waitingNodes,
} from '../dagRun'
import type {
  AgentProfile,
  DagIssue,
  DagNode,
  DagNodeRun,
  DagPlan,
  DagRun,
  DagSummary,
  ProjectItem,
} from '../types'

const props = defineProps<{
  /** 给 Planner 的项目目录候选（来自归档扫出来的项目）。 */
  projects: ProjectItem[]
}>()

const emit = defineEmits<{
  (e: 'error', message: string): void
}>()

const plans = ref<DagSummary[]>([])
const plan = ref<DagPlan | null>(null)
const agents = ref<AgentProfile[]>([])
const issues = ref<DagIssue[]>([])
const selectedNodeId = ref<string | null>(null)
const loading = ref(false)
const saving = ref(false)
const planning = ref(false)
const notice = ref<string | null>(null)
const tab = ref<'task' | 'deps' | 'policy' | 'run' | 'issues'>('task')

/** 上次保存时的快照（JSON 比较，不逐字段 watch）。 */
const saved = ref('')
const dirty = computed(() =>
  plan.value ? JSON.stringify(plan.value) !== saved.value : false,
)

const errors = computed(() => issues.value.filter(issue => issue.level === 'error'))
const warnings = computed(() => issues.value.filter(issue => issue.level === 'warning'))
const nodeIssues = computed(() => {
  const id = selectedNodeId.value
  return id ? issues.value.filter(issue => issue.nodeId === id) : []
})

const layout = computed(() =>
  plan.value
    ? withOverrides(
        plan.value.nodes.map(node => ({
          id: node.id,
          dependsOn: node.dependsOn,
          position: node.position,
        })),
        plan.value.layout,
        { nodeWidth: NODE_W, nodeHeight: NODE_H },
      )
    : null,
)

const selectedNode = computed<DagNode | null>(() => {
  const current = plan.value
  const id = selectedNodeId.value
  return current && id ? (current.nodes.find(node => node.id === id) ?? null) : null
})

const agentName = (id: string | null | undefined): string => {
  const target = id ?? plan.value?.defaults.agent ?? ''
  return agents.value.find(agent => agent.id === target)?.name ?? target ?? '—'
}

const defaultProject = computed(
  () => props.projects[0]?.worktree ?? '',
)

function report(cause: unknown): void {
  emit('error', cause instanceof Error ? cause.message : String(cause))
}

async function loadPlans(keep = true): Promise<void> {
  loading.value = true
  try {
    plans.value = await api.dagList()
    if (!keep || !plan.value) {
      const first = plans.value[0]
      if (first) await open(first.id)
      else plan.value = null
    }
  } catch (cause) {
    report(cause)
  } finally {
    loading.value = false
  }
}

async function open(id: string): Promise<void> {
  try {
    const loaded = await api.dagGet(id)
    plan.value = loaded
    saved.value = JSON.stringify(loaded)
    selectedNodeId.value = loaded.nodes[0]?.id ?? null
    tab.value = 'task'
    notice.value = null
    issues.value = await api.dagValidate(loaded)
    await nextTick()
    fitView()
  } catch (cause) {
    report(cause)
  }
}

async function save(): Promise<void> {
  const current = plan.value
  if (!current || saving.value) return
  saving.value = true
  try {
    const stored = await api.dagSave(current)
    plan.value = stored
    saved.value = JSON.stringify(stored)
    await loadPlans(true)
    issues.value = await api.dagValidate(stored)
    notice.value = '已保存'
  } catch (cause) {
    report(cause)
  } finally {
    saving.value = false
  }
}

async function removePlan(): Promise<void> {
  const current = plan.value
  if (!current || !plans.value.some(item => item.id === current.id)) return
  if (!window.confirm(`删除计划「${current.name}」？磁盘上的 JSON 会一起删掉。`)) return
  try {
    await api.dagDelete(current.id)
    plan.value = null
    saved.value = ''
    await loadPlans(false)
  } catch (cause) {
    report(cause)
  }
}

// ---- 运行 ----

/** 占位符写法。放在常量里而不是模板里：`{{ }}` 直接写在插值里会提前闭合插值。 */
const PLACEHOLDER_HINT = '{{nodes.<id>.output}}'

/** 与 Rust 侧 `dag::run::EVENT_NAME` 对齐；改名字两边都要改。 */
const EVENT_NAME = 'dag.run'

const run = ref<DagRun | null>(null)
const runBusy = ref(false)
/** 每秒推进一次，让「已跑 3m12s」自己走。只在有活运行时开表。 */
const now = ref(Date.now())
let ticker = 0
let unlisten: (() => void) | null = null

const live = computed(() => isLive(run.value))
const progress = computed(() => runProgress(run.value))
const gates = computed(() => waitingNodes(run.value))
const nodeRuns = computed(() => {
  const map = new Map<string, DagNodeRun>()
  for (const item of run.value?.nodes ?? []) map.set(item.id, item)
  return map
})
const selectedRun = computed(() =>
  selectedNodeId.value ? nodeRunOf(run.value, selectedNodeId.value) : null,
)
const elapsed = computed(() => (run.value ? runElapsed(run.value, now.value) : 0))
const selectedElapsed = computed(() =>
  selectedRun.value ? nodeElapsed(selectedRun.value, now.value) : 0,
)

const blocker = computed(() =>
  runBlocker({
    hasPlan: Boolean(plan.value),
    dirty: dirty.value,
    errors: errors.value.length,
    live: live.value,
    nodeCount: plan.value?.nodes.length ?? 0,
  }),
)

function syncTicker(): void {
  // 没在跑就别每秒醒一次：白白重算整块面板。
  if (live.value && !ticker) ticker = window.setInterval(() => (now.value = Date.now()), 1000)
  if (!live.value && ticker) {
    window.clearInterval(ticker)
    ticker = 0
  }
}

/** 只认这张计划的快照：别的计划推过来的事件直接丢掉，免得串台。 */
function adoptRun(next: DagRun | null | undefined): void {
  if (!next) return
  if (plan.value && next.planId !== plan.value.id) return
  run.value = next
  now.value = Date.now()
  syncTicker()
}

async function startRun(): Promise<void> {
  const current = plan.value
  if (!current || runBusy.value || blocker.value) return
  runBusy.value = true
  try {
    adoptRun(await api.dagRun(current.id))
    notice.value = null
  } catch (cause) {
    report(cause)
  } finally {
    runBusy.value = false
  }
}

async function stopRun(): Promise<void> {
  const current = run.value
  if (!current || runBusy.value) return
  runBusy.value = true
  try {
    adoptRun(await api.dagRunCancel(current.id))
  } catch (cause) {
    report(cause)
  } finally {
    runBusy.value = false
  }
}

/** 人工闸门的答复。拒绝时节点取消、下游跳过——不是报错，是"没跑"。 */
async function decideGate(nodeId: string, allowed: boolean): Promise<void> {
  const current = run.value
  if (!current || runBusy.value) return
  runBusy.value = true
  try {
    adoptRun(await api.dagRunApprove(current.id, nodeId, allowed))
  } catch (cause) {
    report(cause)
  } finally {
    runBusy.value = false
  }
}

/** 点运行条上的节点：选中它，并切到「运行」标签看产出。 */
function focusNode(id: string): void {
  selectedNodeId.value = id
  tab.value = 'run'
}

/** 节点实际会用哪个执行器（节点覆盖 → 计划默认）。 */
function effectiveExecutor(agentId: string | null | undefined): string {
  const target = agentId ?? plan.value?.defaults.agent ?? ''
  return agents.value.find(agent => agent.id === target)?.executor ?? '—'
}

/** 状态类名（`is-running` 这种）：样式表只认前缀，不认整串。 */
function nodeStatusClass(id: string): string {
  const item = nodeRuns.value.get(id)
  return item ? `is-${item.status}` : ''
}

/** 画布第二行：跑起来之后显示最后一步工具调用，比静态任务描述更有信息量。 */
function nodeTaskPreview(node: DagNode): string {
  const item = nodeRuns.value.get(node.id)
  if (item?.status === 'running') {
    return item.steps.length ? item.steps[item.steps.length - 1] : '正在思考…'
  }
  if (item?.status === 'failed' && item.error) return item.error
  return node.task || '还没有任务描述'
}

// ---- Planner ----

const plannerOpen = ref(false)
const plannerGoal = ref('')
const plannerProject = ref('')
const plannerMax = ref(6)

function openPlanner(): void {
  plannerGoal.value = plan.value?.goal ?? ''
  plannerProject.value = plan.value?.project || defaultProject.value
  plannerOpen.value = true
}

async function runPlanner(): Promise<void> {
  if (planning.value) return
  planning.value = true
  try {
    const draft = await api.dagPlan(plannerGoal.value, plannerProject.value, plannerMax.value)
    plan.value = draft
    // 草案没落盘：`saved` 留空，header 会显示"未保存"。
    saved.value = ''
    selectedNodeId.value = draft.nodes[0]?.id ?? null
    issues.value = await api.dagValidate(draft)
    plannerOpen.value = false
    notice.value = '草案已生成（未保存）：改完点「保存计划」'
    await nextTick()
    fitView()
  } catch (cause) {
    report(cause)
  } finally {
    planning.value = false
  }
}

async function newBlank(): Promise<void> {
  try {
    const blank = await api.dagNew(plannerProject.value || defaultProject.value)
    plan.value = blank
    saved.value = ''
    selectedNodeId.value = blank.nodes[0]?.id ?? null
    issues.value = await api.dagValidate(blank)
    notice.value = '空白计划（未保存）'
    await nextTick()
    fitView()
  } catch (cause) {
    report(cause)
  }
}

// ---- 节点编辑 ----

function uniqueId(base: string): string {
  const used = new Set(plan.value?.nodes.map(node => node.id) ?? [])
  if (!used.has(base)) return base
  let index = 2
  while (used.has(`${base}-${index}`)) index += 1
  return `${base}-${index}`
}

function addNode(): void {
  if (!plan.value) return
  const node: DagNode = {
    id: uniqueId(`node-${plan.value.nodes.length + 1}`),
    title: '新节点',
    task: '',
    dependsOn: [],
    agent: null,
    workspace: null,
    tools: null,
    permission: null,
    retry: { attempts: 0, backoffMs: 2000 },
    timeoutSecs: null,
    gate: null,
    position: null,
  }
  plan.value.nodes.push(node)
  selectedNodeId.value = node.id
  tab.value = 'task'
}

function removeNode(id: string): void {
  const current = plan.value
  if (!current) return
  current.nodes = current.nodes.filter(node => node.id !== id)
  for (const node of current.nodes) {
    node.dependsOn = node.dependsOn.filter(dep => dep !== id)
  }
  delete current.layout[id]
  if (selectedNodeId.value === id) {
    selectedNodeId.value = current.nodes[0]?.id ?? null
  }
}

/** 加这条依赖会不会成环：dep 是否已经（间接）依赖 node。 */
function wouldCycle(nodeId: string, depId: string): boolean {
  const current = plan.value
  if (!current || nodeId === depId) return true
  const seen = new Set<string>()
  const stack = [depId]
  while (stack.length) {
    const id = stack.pop() as string
    if (id === nodeId) return true
    if (seen.has(id)) continue
    seen.add(id)
    const node = current.nodes.find(item => item.id === id)
    for (const dep of node?.dependsOn ?? []) stack.push(dep)
  }
  return false
}

function toggleDep(node: DagNode, depId: string): void {
  if (node.dependsOn.includes(depId)) {
    node.dependsOn = node.dependsOn.filter(item => item !== depId)
  } else if (!wouldCycle(node.id, depId)) {
    node.dependsOn = [...node.dependsOn, depId]
  }
}

function toggleGate(node: DagNode): void {
  node.gate = node.gate?.before ? null : { before: true, after: false }
}

function autoLayout(): void {
  if (!plan.value) return
  plan.value.layout = {}
  for (const node of plan.value.nodes) node.position = null
  void nextTick(fitView)
  notice.value = '已重新分层：手工拖过的坐标一并清掉'
}

// ---- 校验（防抖：编辑时不要每次按键都过一遍后端） ----

let validateTimer = 0
watch(
  plan,
  () => {
    if (!plan.value) {
      issues.value = []
      return
    }
    window.clearTimeout(validateTimer)
    validateTimer = window.setTimeout(() => void revalidate(), 180)
  },
  { deep: true },
)

async function revalidate(): Promise<void> {
  const current = plan.value
  if (!current) return
  try {
    issues.value = await api.dagValidate(current)
  } catch {
    // 校验失败不弹错：编辑中的图本来就可能暂时不合法。
  }
}

// ---- 画布：平移 / 缩放 / 拖动节点 ----

const canvasEl = ref<HTMLElement | null>(null)
const view = ref({ scale: 1, x: 0, y: 0 })
const worldStyle = computed(() => ({
  transform: `translate(${view.value.x}px, ${view.value.y}px) scale(${view.value.scale})`,
}))

function fitView(): void {
  const element = canvasEl.value
  const size = layout.value
  if (!element || !size || !plan.value) return
  const rect = element.getBoundingClientRect()
  if (!rect.width || !rect.height) return
  const pad = 40
  const scale = Math.max(
    0.4,
    Math.min(
      (rect.width - pad * 2) / Math.max(1, size.width),
      (rect.height - pad * 2) / Math.max(1, size.height),
      1.1,
    ),
  )
  view.value = {
    scale,
    x: Math.round((rect.width - size.width * scale) / 2),
    y: Math.round((rect.height - size.height * scale) / 2),
  }
}

function zoomAt(factor: number, clientX: number, clientY: number): void {
  const element = canvasEl.value
  if (!element) return
  const rect = element.getBoundingClientRect()
  const px = clientX - rect.left
  const py = clientY - rect.top
  const next = Math.min(1.8, Math.max(0.4, view.value.scale * factor))
  if (Math.abs(next - view.value.scale) < 0.0001) return
  view.value = {
    scale: next,
    x: px - (px - view.value.x) * (next / view.value.scale),
    y: py - (py - view.value.y) * (next / view.value.scale),
  }
}

function onWheel(event: WheelEvent): void {
  event.preventDefault()
  zoomAt(Math.exp(-event.deltaY * 0.0016), event.clientX, event.clientY)
}

let pan: { x: number; y: number; vx: number; vy: number } | null = null
let nodeDrag: { id: string; x: number; y: number; ox: number; oy: number } | null = null

function onCanvasDown(event: PointerEvent): void {
  if (event.button !== 0) return
  const target = event.target as HTMLElement
  if (target.closest('.dag-node, .canvas-tools, .legend')) return
  pan = { x: event.clientX, y: event.clientY, vx: view.value.x, vy: view.value.y }
  ;(event.currentTarget as HTMLElement).setPointerCapture(event.pointerId)
}

function onNodeDown(event: PointerEvent, node: DagNode): void {
  if (event.button !== 0) return
  event.stopPropagation()
  selectedNodeId.value = node.id
  const box = layout.value?.boxes.get(node.id)
  if (!box) return
  nodeDrag = { id: node.id, x: event.clientX, y: event.clientY, ox: box.x, oy: box.y }
  ;(event.currentTarget as HTMLElement).setPointerCapture(event.pointerId)
}

function onPointerMove(event: PointerEvent): void {
  if (pan) {
    view.value = {
      ...view.value,
      x: pan.vx + event.clientX - pan.x,
      y: pan.vy + event.clientY - pan.y,
    }
    return
  }
  const current = plan.value
  if (!nodeDrag || !current) return
  const dx = (event.clientX - nodeDrag.x) / view.value.scale
  const dy = (event.clientY - nodeDrag.y) / view.value.scale
  current.layout[nodeDrag.id] = {
    x: Math.round(nodeDrag.ox + dx),
    y: Math.round(nodeDrag.oy + dy),
  }
}

function onPointerUp(): void {
  pan = null
  nodeDrag = null
}

onMounted(() => {
  void loadPlans()
  void api
    .agentsList()
    .then(list => (agents.value = list))
    .catch(() => {
      // Agent 目录只影响下拉里的名字，取不到就显示 id。
    })
  // 进度靠事件推：一次运行会推几十次快照，轮询会白白打满 invoke。
  void listen<DagRun>(EVENT_NAME, envelope => adoptRun(envelope.payload)).then(off => {
    unlisten = off
    // 监听挂上之前可能已经推过一轮：补一次拉取，避免"停在旧快照"。
    if (plan.value) void refreshRun(plan.value.id)
  })
})

onBeforeUnmount(() => {
  unlisten?.()
  unlisten = null
  if (ticker) {
    window.clearInterval(ticker)
    ticker = 0
  }
})

/** 切计划时把运行快照一起换掉：不换会显示上一张图的进度。 */
watch(
  () => plan.value?.id ?? null,
  id => {
    run.value = null
    syncTicker()
    if (id) void refreshRun(id)
  },
)

async function refreshRun(planId: string): Promise<void> {
  try {
    const latest = await api.dagRunLatest(planId)
    // 元数据还没切完就丢：`adoptRun` 自己会核对 planId。
    if (latest) adoptRun(latest)
  } catch {
    // 没有历史运行不是错误：新计划本来就没跑过。
  }
}

defineExpose({ reload: () => loadPlans(true) })
</script>

<template>
  <div class="dag-pane">
    <aside class="pane plans">
      <div class="pane-head">
        <div class="pane-title">编排</div>
        <span class="pane-count">{{ plans.length }}</span>
        <button class="ghost-btn" :disabled="loading" title="重新读取计划" @click="loadPlans(true)">
          刷新
        </button>
      </div>
      <div class="pane-body">
        <p v-if="loading && !plans.length" class="hint">正在读取计划…</p>
        <p v-else-if="!plans.length" class="hint">
          还没有计划。点下面「发起需求分析」，让 Planner 从一句目标拆一张图。
        </p>
        <button
          v-for="item in plans"
          :key="item.id"
          class="plan-row"
          :class="{ active: item.id === plan?.id }"
          @click="open(item.id)"
        >
          <span class="plan-row-top">
            <span class="plan-name">{{ item.name }}</span>
            <span class="plan-time">{{ new Date(item.updatedAt).toLocaleDateString() }}</span>
          </span>
          <span class="plan-row-sub">
            {{ item.nodeCount }} 节点
            <template v-if="item.gateCount">· {{ item.gateCount }} 需确认</template>
          </span>
          <span class="plan-goal">{{ item.goal || '（没有写目标）' }}</span>
        </button>
      </div>
      <div class="pane-foot">
        <button class="primary-btn" @click="openPlanner">发起需求分析</button>
        <button class="ghost-btn wide" @click="newBlank">空白计划</button>
      </div>
    </aside>

    <section class="pane main">
      <div class="main-head">
        <template v-if="plan">
          <div class="title-row">
            <input v-model="plan.name" class="plan-title-input" spellcheck="false" />
            <span class="pill" :class="dirty ? 'draft' : 'saved'">
              {{ dirty ? '未保存' : '已保存' }}
            </span>
            <span v-if="errors.length" class="pill error">{{ errors.length }} 个问题</span>
            <span v-else-if="warnings.length" class="pill warn">{{ warnings.length }} 条提醒</span>
          </div>
          <div class="sub-row">
            <span class="mono">{{ plan.nodes.length }} 节点</span>
            <span class="sep">·</span>
            <span class="mono">{{ plan.id }}</span>
            <span class="sep">·</span>
            <span class="mono">并发 {{ plan.defaults.maxParallel }}</span>
          </div>
        </template>
        <div v-else class="title-row">
          <span class="empty-title">没有打开的计划</span>
        </div>
        <div class="head-actions">
          <button class="ghost-btn" title="按依赖重新分层，清掉手工坐标" @click="autoLayout">
            自动排版
          </button>
          <button class="ghost-btn" :disabled="!plan" title="加一个节点" @click="addNode">
            加节点
          </button>
          <button
            class="ghost-btn"
            :disabled="!plan || dirty"
            :title="dirty ? '先保存再删除' : '删除这张计划'"
            @click="removePlan"
          >
            删除
          </button>
          <button class="primary-btn compact" :disabled="!plan || saving" @click="save">
            {{ saving ? '保存中…' : '保存计划' }}
          </button>
          <button
            v-if="live"
            class="ghost-btn danger"
            :disabled="runBusy"
            title="停止：正在跑的节点会在下一次工具/模型调用前收手"
            @click="stopRun"
          >
            停止
          </button>
          <button
            v-else
            class="primary-btn compact run"
            :disabled="runBusy || Boolean(blocker)"
            :title="blocker ?? '按依赖顺序把节点放出去跑'"
            @click="startRun"
          >
            {{ runBusy ? '启动中…' : '▶ 运行' }}
          </button>
        </div>
      </div>

      <p v-if="notice" class="canvas-notice">{{ notice }}</p>

      <div v-if="run" class="run-strip">
        <div class="run-head">
          <span class="run-state" :class="runMeta(run.status).tone">
            <i class="run-dot" />
            {{ runMeta(run.status).label }}
          </span>
          <span class="run-track" :title="`${progress.settled}/${progress.total} 已落定`">
            <i :class="runMeta(run.status).tone" :style="{ width: `${progress.percent}%` }" />
          </span>
          <span class="run-count mono">{{ runSummary(run) }}</span>
          <span v-if="run.nodes.some(node => node.tokensIn || node.tokensOut)" class="run-tokens mono">
            ↑{{ formatTokens(run.nodes.reduce((sum, node) => sum + node.tokensIn, 0)) }}
            ↓{{ formatTokens(run.nodes.reduce((sum, node) => sum + node.tokensOut, 0)) }}
          </span>
          <span v-if="elapsed" class="run-clock mono">{{ formatDuration(elapsed) }}</span>
          <span class="spacer" />
          <button class="ghost-btn tiny" title="收起运行条（运行本身不受影响）" @click="run = null">
            收起
          </button>
        </div>

        <div class="run-nodes">
          <button
            v-for="item in run.nodes"
            :key="item.id"
            class="run-chip"
            :class="[nodeMeta(item.status).tone, { active: item.id === selectedNodeId }]"
            :title="item.error ?? item.title"
            @click="focusNode(item.id)"
          >
            <span class="run-chip-glyph">{{ nodeMeta(item.status).glyph }}</span>
            <span class="run-chip-title">{{ item.title || item.id }}</span>
            <span class="run-chip-state">{{ nodeMeta(item.status).label }}</span>
          </button>
        </div>

        <div v-for="gate in gates" :key="`gate-${gate.id}`" class="run-gate">
          <span class="run-gate-text">
            <b>{{ gate.title || gate.id }}</b> 依赖已齐，等你放行。拒绝的话：这个节点取消，下游跳过。
          </span>
          <button class="ghost-btn tiny" :disabled="runBusy" @click="decideGate(gate.id, false)">
            拒绝
          </button>
          <button class="primary-btn compact" :disabled="runBusy" @click="decideGate(gate.id, true)">
            放行
          </button>
        </div>

        <p v-if="run.message" class="run-message">{{ run.message }}</p>
      </div>

      <div
        ref="canvasEl"
        class="canvas"
        @wheel="onWheel"
        @pointerdown="onCanvasDown"
        @pointermove="onPointerMove"
        @pointerup="onPointerUp"
        @pointercancel="onPointerUp"
      >
        <template v-if="plan && layout">
          <div class="world" :style="[worldStyle, { width: `${layout.width}px`, height: `${layout.height}px` }]">
            <svg
              class="edges"
              :width="layout.width"
              :height="layout.height"
              :viewBox="`0 0 ${layout.width} ${layout.height}`"
              aria-hidden="true"
            >
              <defs>
                <marker
                  id="dag-arrow"
                  viewBox="0 0 8 8"
                  refX="6.5"
                  refY="4"
                  markerWidth="6"
                  markerHeight="6"
                  orient="auto-start-reverse"
                >
                  <path d="M0 0 8 4 0 8z" fill="var(--border-strong)" />
                </marker>
              </defs>
              <template v-for="node in plan.nodes" :key="`e-${node.id}`">
                <path
                  v-for="dep in node.dependsOn"
                  :key="`${dep}-${node.id}`"
                  :d="
                    layout.boxes.get(dep) && layout.boxes.get(node.id)
                      ? edgePath(layout.boxes.get(dep)!, layout.boxes.get(node.id)!, NODE_W, NODE_H)
                      : ''
                  "
                  marker-end="url(#dag-arrow)"
                />
              </template>
            </svg>

            <button
              v-for="node in plan.nodes"
              :key="node.id"
              class="dag-node"
              :class="[
                nodeStatusClass(node.id),
                { active: node.id === selectedNodeId, gated: node.gate?.before },
              ]"
              :style="{
                left: `${layout.boxes.get(node.id)?.x ?? 0}px`,
                top: `${layout.boxes.get(node.id)?.y ?? 0}px`,
                width: `${NODE_W}px`,
                height: `${NODE_H}px`,
              }"
              @pointerdown="onNodeDown($event, node)"
            >
              <span class="node-top">
                <span class="node-dot" :class="{ gated: node.gate?.before }" />
                <span class="node-title">{{ node.title || '（无标题）' }}</span>
                <span
                  v-if="nodeRuns.get(node.id)"
                  class="node-badge"
                  :class="nodeMeta(nodeRuns.get(node.id)!.status).tone"
                >
                  {{ nodeMeta(nodeRuns.get(node.id)!.status).glyph }}
                </span>
                <span v-else-if="node.gate?.before" class="gate-chip">需确认</span>
              </span>
              <span class="node-task">{{ nodeTaskPreview(node) }}</span>
              <span class="node-foot">
                <span class="node-avatar">{{ agentName(node.agent).slice(0, 1) }}</span>
                <span class="node-agent">{{ agentName(node.agent) }}</span>
                <span class="spacer" />
                <span class="eng">{{ executorBadge(agents.find(a => a.id === (node.agent ?? plan?.defaults.agent))?.executor ?? 'native') }}</span>
              </span>
            </button>
          </div>

          <div class="canvas-tools">
            <button title="缩小" @click="zoomAt(1 / 1.16, 0, 0)">−</button>
            <button title="放大" @click="zoomAt(1.16, 0, 0)">＋</button>
            <span class="sep" />
            <button title="适应窗口" @click="fitView">适应</button>
          </div>
          <div class="legend">
            <span><i class="dot-plain" />节点</span>
            <span><i class="dot-gate" />需人工确认</span>
            <span class="legend-note">拖节点改坐标 · 拖空白平移 · 滚轮缩放</span>
          </div>
        </template>

        <div v-else class="empty-state">
          <div class="empty-icon" aria-hidden="true">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.4">
              <circle cx="5.5" cy="6" r="2.2" />
              <circle cx="18.5" cy="6" r="2.2" />
              <circle cx="12" cy="18" r="2.2" />
              <path d="M7.7 6h8.6M6.8 8.1l4.2 7.8M17.2 8.1l-4.2 7.8" />
            </svg>
          </div>
          <p class="empty-lead">一句目标，拆成有依赖的图</p>
          <p class="empty-sub">
            Planner 会读 Agent 注册表挑人，按依赖分层；每个节点交给一个 Agent 独立跑。
            草案需要你确认后才保存。
          </p>
          <button class="primary-btn compact" @click="openPlanner">发起需求分析</button>
        </div>
      </div>
    </section>

    <aside class="pane insp">
      <template v-if="selectedNode && plan">
        <div class="pane-head">
          <div class="pane-title">节点</div>
          <span class="node-id">{{ selectedNode.id }}</span>
          <button class="ghost-btn" title="删除这个节点" @click="removeNode(selectedNode.id)">
            删除
          </button>
        </div>
        <div class="insp-tabs">
          <button
            v-for="item in ([
              ['task', '任务'],
              ['deps', '依赖'],
              ['policy', '策略'],
              ['run', '运行'],
              ['issues', '校验'],
            ] as const)"
            :key="item[0]"
            class="insp-tab"
            :class="{ active: tab === item[0] }"
            @click="tab = item[0]"
          >
            {{ item[1] }}
            <span v-if="item[0] === 'issues' && nodeIssues.length" class="tab-count">
              {{ nodeIssues.length }}
            </span>
          </button>
        </div>
        <div class="pane-body insp-body">
          <template v-if="tab === 'task'">
            <label class="field">
              <span class="field-label">标题</span>
              <input v-model="selectedNode.title" />
            </label>
            <label class="field">
              <span class="field-label">任务 <em>子 Agent 只能看到这段</em></span>
              <textarea v-model="selectedNode.task" rows="8" placeholder="做什么、边界、验收标准" />
            </label>
            <p class="hint small">
              需要上游结论时用 <code>{{ PLACEHOLDER_HINT }}</code>，
              只能引用自己的上游（祖先）。
            </p>
            <label class="field checkbox">
              <input
                type="checkbox"
                :checked="Boolean(selectedNode.gate?.before)"
                @change="toggleGate(selectedNode)"
              />
              <span>运行前需要人工确认</span>
            </label>
          </template>

          <template v-else-if="tab === 'deps'">
            <p class="hint small">勾上表示「等它跑完再开始」。加会成环的依赖会被拦住。</p>
            <div class="dep-list">
              <button
                v-for="other in plan.nodes.filter(node => node.id !== selectedNode!.id)"
                :key="other.id"
                class="dep-row"
                :class="{
                  checked: selectedNode.dependsOn.includes(other.id),
                  blocked: !selectedNode.dependsOn.includes(other.id) && wouldCycle(selectedNode.id, other.id),
                }"
                @click="toggleDep(selectedNode!, other.id)"
              >
                <span class="check" />
                <span class="dep-copy">
                  <span class="dep-title">{{ other.title }}</span>
                  <span class="dep-id">{{ other.id }}</span>
                </span>
              </button>
              <p v-if="plan.nodes.length === 1" class="hint small">图里只有这一个节点</p>
            </div>
          </template>

          <template v-else-if="tab === 'policy'">
            <label class="field">
              <span class="field-label">Agent</span>
              <select v-model="selectedNode.agent">
                <option :value="null">跟随默认（{{ agentName(plan.defaults.agent) }}）</option>
                <option v-for="agent in agents" :key="agent.id" :value="agent.id">
                  {{ agent.name }} · {{ agent.id }}
                </option>
              </select>
            </label>
            <label class="field">
              <span class="field-label">工作区</span>
              <select v-model="selectedNode.workspace">
                <option :value="null">跟随 Agent</option>
                <option value="shared">{{ workspaceLabel('shared') }}</option>
                <option value="worktree">{{ workspaceLabel('worktree') }}</option>
                <option value="scratch">{{ workspaceLabel('scratch') }}</option>
              </select>
            </label>
            <label class="field">
              <span class="field-label">权限</span>
              <select v-model="selectedNode.permission">
                <option :value="null">跟随计划（{{ permissionLabel(plan.defaults.permission) }}）</option>
                <option value="read-only">{{ permissionLabel('read-only') }}</option>
                <option value="workspace-write">{{ permissionLabel('workspace-write') }}</option>
                <option value="danger-full-access">{{ permissionLabel('danger-full-access') }}</option>
              </select>
            </label>
            <div class="field">
              <span class="field-label">生效值</span>
              <div class="kv"><span>Agent</span><b>{{ agentName(selectedNode.agent) }}</b></div>
              <div class="kv">
                <span>执行器</span>
                <b>{{ effectiveExecutor(selectedNode.agent) }}</b>
              </div>
              <div class="kv">
                <span>工作区</span>
                <b>{{ selectedNode.workspace ? workspaceLabel(selectedNode.workspace) : '跟随 Agent' }}</b>
              </div>
              <div class="kv">
                <span>权限</span>
                <b>{{ permissionLabel(selectedNode.permission ?? plan.defaults.permission) }}</b>
              </div>
            </div>
          </template>

          <template v-else-if="tab === 'run'">
            <p v-if="!run" class="hint small">
              这张计划还没跑过。点右上角「运行」，节点会按依赖顺序放出去。
            </p>
            <p v-else-if="!selectedRun" class="hint small">这次运行里没有这个节点。</p>
            <template v-else>
              <div class="run-kv">
                <span class="kv"><span>状态</span><b :class="nodeMeta(selectedRun.status).tone">{{ nodeMeta(selectedRun.status).label }}</b></span>
                <span class="kv"><span>尝试</span><b>{{ selectedRun.attempt || '—' }}</b></span>
                <span class="kv"><span>耗时</span><b>{{ formatDuration(selectedElapsed) || '—' }}</b></span>
                <span class="kv"><span>Token</span><b>↑{{ formatTokens(selectedRun.tokensIn) }} ↓{{ formatTokens(selectedRun.tokensOut) }}</b></span>
              </div>

              <div v-if="selectedRun.status === 'waiting'" class="run-gate inline">
                <span class="run-gate-text">等你放行。</span>
                <button class="ghost-btn tiny" :disabled="runBusy" @click="decideGate(selectedRun.id, false)">拒绝</button>
                <button class="primary-btn compact" :disabled="runBusy" @click="decideGate(selectedRun.id, true)">放行</button>
              </div>

              <p v-if="selectedRun.error" class="run-error">{{ selectedRun.error }}</p>

              <div v-if="selectedRun.steps.length" class="run-steps">
                <div class="run-steps-title">过程</div>
                <div v-for="(step, index) in selectedRun.steps" :key="index" class="run-step">
                  <span class="run-step-index mono">{{ index + 1 }}</span>
                  <span class="run-step-text mono">{{ step }}</span>
                </div>
              </div>

              <div v-if="selectedRun.output" class="run-output">
                <div class="run-steps-title">产出</div>
                <pre class="run-pre">{{ selectedRun.output }}</pre>
              </div>
              <p v-else-if="selectedRun.status === 'running'" class="hint small">正在跑，产出会陆续出现。</p>
            </template>
          </template>

          <template v-else>
            <p v-if="!issues.length" class="hint small">没有发现问题。</p>
            <div v-for="(issue, index) in issues" :key="index" class="issue" :class="issue.level">
              <span class="issue-node">{{ issue.nodeId ?? '计划' }}</span>
              <span class="issue-text">{{ issue.message }}</span>
            </div>
          </template>
        </div>
      </template>
      <p v-else class="hint">左侧选一个节点查看与修改</p>
    </aside>

    <div v-if="plannerOpen" class="planner-backdrop" @click.self="plannerOpen = false">
      <div class="planner-card">
        <div class="planner-head">
          <div class="planner-kicker">DAG Planner · 需求分析</div>
          <div class="planner-title">发起一次编排</div>
          <p class="planner-sub">
            描述目标，Planner 从 Agent 注册表挑人、按依赖生成草案。草案不落盘，确认后再保存。
          </p>
        </div>
        <div class="planner-body">
          <label class="field">
            <span class="field-label">目标</span>
            <textarea v-model="plannerGoal" rows="4" placeholder="给设置页加自动更新：检查 / 下载进度 / 重启安装" />
          </label>
          <label class="field">
            <span class="field-label">项目目录</span>
            <input v-model="plannerProject" :placeholder="defaultProject || '/path/to/project'" spellcheck="false" />
          </label>
          <label class="field">
            <span class="field-label">节点上限 <em>2–24，默认 6</em></span>
            <input v-model.number="plannerMax" type="number" min="2" max="24" />
          </label>
          <p class="hint small">
            用的是设置里「原生引擎」选中的模型服务。没配置会直接在这里报错。
          </p>
        </div>
        <div class="planner-foot">
          <button class="ghost-btn" @click="plannerOpen = false">取消</button>
          <button
            class="primary-btn compact"
            :disabled="planning || !plannerGoal.trim()"
            @click="runPlanner"
          >
            {{ planning ? '规划中…' : '生成草案' }}
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped src="../styles/components/DagPane.css"></style>
