<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { listen } from '@tauri-apps/api/event'
import { open as openDirectoryDialog } from '@tauri-apps/plugin-dialog'
import { api } from '../api'
import { NODE_H, NODE_W, edgePath, withOverrides } from '../dagLayout'
import { executorBadge, permissionLabel, workspaceLabel } from '../agents'
import { basename, formatBytes, formatDuration, formatTokens } from '../format'
import { useBackdropDismiss } from '../dialogDismiss'
import { renderMarkdown } from '../markdown'
import SelectMenu from './SelectMenu.vue'
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
  AgentPermission,
  AgentProfile,
  ArtifactRef,
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
/** 制品占位符写法（同上，别直接写进模板）。 */
const ARTIFACT_HINT = '{{artifacts.<上游>.summary}}'

/** 与 Rust 侧 `dag::run::EVENT_NAME` 对齐；改名字两边都要改。 */
const EVENT_NAME = 'dag.run'
/** 节点增量事件（正文 / 步骤 / token）。与 `dag::run::LIVE_EVENT_NAME` 对齐。 */
const LIVE_EVENT_NAME = 'dag.node.live'

/**
 * 一条增量事件。字段与 Rust 侧 `NodeLive` 一一对应：`delta` 是正文片段，
 * `step` 是新落的一步工具调用，token 是"到现在为止"的累计值。
 */
interface NodeLiveEvent {
  runId: string
  nodeId: string
  delta: string
  step: string | null
  tokensIn: number
  tokensOut: number
  /** 正在跑第几轮 / 上限。模型久不吐字时，靠它说明进度。 */
  turn: number
  maxTurns: number
}

/** 前端侧叠在快照上的直播态。节点落定时丢弃，改用快照里的完整正文。 */
interface LiveState {
  text: string
  steps: string[]
  tokensIn: number
  tokensOut: number
  /** 最后一次收到增量的时刻：用来判断"是在慢慢跑，还是真卡住了"。 */
  at: number
  turn: number
  maxTurns: number
}

const run = ref<DagRun | null>(null)
const runBusy = ref(false)
/** 每秒推进一次，让「已跑 3m12s」自己走。只在有活运行时开表。 */
const now = ref(Date.now())
let ticker = 0
/** 轮询兜底（事件通道之外的保险），只在真跑着的时候开。 */
let poll = 0
let unlisten: (() => void) | null = null
let unlistenLive: (() => void) | null = null
/**
 * `nodeId -> 直播态`。
 *
 * 存成普通对象并整体替换，而不是 `Map`：Vue 的响应式对 Map 的增删要走
 * `reactive`，这里只在事件到来时换一份，模板拿到的引用一定是最新的。
 */
const liveByNode = ref<Record<string, LiveState>>({})
/** 直播正文最多留这么多字符：长报告跑十分钟，不能把所有增量都攒在内存里。 */
const LIVE_TEXT_LIMIT = 24_000
/** 实时正文的滚动容器：新内容进来要跟着到底，否则看到的永远是开头。 */
const livePre = ref<HTMLElement | null>(null)

/** 制品预览：点开节点的产物时按 id 取回正文。 */
const artifactPreview = ref<{ ref: ArtifactRef; text: string; truncated: boolean } | null>(null)
const artifactBusy = ref(false)

/**
 * 产出的看法：节点写的是 markdown，默认按渲染稿读（标题、列表、代码块、表格都在）；
 * 要抠字、贴到别处时切「原文」。两个视图各记各的，切节点不会互相打回默认。
 */
const outputView = ref<'preview' | 'raw'>('preview')
const artifactView = ref<'preview' | 'raw'>('preview')

/** 制品能不能当 markdown 看：只有纯文本可以，json / 图片 / 二进制照旧原文。 */
const artifactIsText = computed(() => artifactPreview.value?.ref.kind === 'text')

async function openArtifact(ref: ArtifactRef): Promise<void> {
  if (!plan.value) return
  artifactBusy.value = true
  try {
    const data = await api.dagArtifactGet(plan.value.id, ref.id, 40_000)
    artifactPreview.value = { ref: data.artifact, text: data.text, truncated: data.truncated }
    artifactView.value = 'preview'
  } catch (error) {
    // 取不到就明说：制品可能被手动清掉了，不要留一个空白弹层。
    artifactPreview.value = {
      ref,
      text: `读取失败：${String(error)}`,
      truncated: false,
    }
    artifactView.value = 'raw'
  } finally {
    artifactBusy.value = false
  }
}

/** 刚复制过的那个按钮；1.4 秒后复原——不然点了像没反应，会重复点。 */
const copiedKey = ref('')
let copiedTimer = 0

function copyText(text: string, key = 'copy'): void {
  void navigator.clipboard?.writeText(text)
  copiedKey.value = key
  if (copiedTimer) window.clearTimeout(copiedTimer)
  copiedTimer = window.setTimeout(() => {
    copiedKey.value = ''
    copiedTimer = 0
  }, 1400)
}

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

/** 选中节点的直播态。只有正在跑的节点才有内容。 */
const selectedLive = computed(() =>
  selectedNodeId.value ? liveByNode.value[selectedNodeId.value] : undefined,
)

/**
 * 过程列表。
 *
 * 跑着的时候快照是旧的（步骤只在落定时才写进记录），所以优先用增量里的；
 * 落定后快照才是准的（它含重试前几次的步骤）。
 */
const selectedSteps = computed(() => {
  const item = selectedRun.value
  if (!item) return []
  const live = selectedLive.value
  if (live?.steps.length) return live.steps
  return item.steps
})

/** 选中节点的 token：跑着显示实时累计，落定后以快照为准。 */
const selectedTokens = computed(() => {
  const item = selectedRun.value
  const live = selectedLive.value
  if (item && live && item.status === 'running') {
    return {
      tin: Math.max(item.tokensIn, live.tokensIn),
      tout: Math.max(item.tokensOut, live.tokensOut),
      cached: item.tokensCached ?? 0,
    }
  }
  return { tin: item?.tokensIn ?? 0, tout: item?.tokensOut ?? 0, cached: item?.tokensCached ?? 0 }
})

/** 当前正在跑的节点（运行条上显示"现在谁在干什么"）。 */
const activeNode = computed(
  () => run.value?.nodes.find(node => node.status === 'running') ?? null,
)

/** 它最近的一步：优先增量，其次上一次快照。 */
const activeStep = computed(() => {
  const node = activeNode.value
  if (!node) return ''
  const live = liveByNode.value[node.id]
  if (live?.steps.length) return live.steps[live.steps.length - 1]
  if (node.steps.length) return node.steps[node.steps.length - 1]
  return live?.text ? '正在输出…' : '正在思考…'
})

/** 缓存命中率（整数 %）。没报缓存或算不出时返回空串，界面就不显示这一格。 */
function cachePercent(cached: number, input: number): string {
  if (!input || !cached) return ''
  return Math.round((cached / input) * 100).toString()
}

/** 运行条上的 token 总数：把正在跑那一段实时的也算进去。 */
const totalTokens = computed(() => {
  let tin = 0
  let tout = 0
  let cached = 0
  for (const node of run.value?.nodes ?? []) {
    const live = liveByNode.value[node.id]
    tin += Math.max(node.tokensIn, live?.tokensIn ?? 0)
    tout += Math.max(node.tokensOut, live?.tokensOut ?? 0)
    // 缓存命中只在快照里（live 增量不报）：节点没落定前总数偏低一点，可接受。
    cached += node.tokensCached ?? 0
  }
  return { tin, tout, cached }
})

/**
 * 正在跑的节点已经多久没有新动静（毫秒）。
 *
 * 引擎卡在模型请求上时，面板只会一直显示"运行中"——用户没法区分"在慢慢跑"
 * 和"真挂了"。超过阈值就明说一句，而不是让人盯着猜。
 */
const idleFor = computed(() => {
  const node = activeNode.value
  if (!node) return 0
  const live = liveByNode.value[node.id]
  const since = live?.at ?? node.startedAt
  if (!since) return 0
  return Math.max(0, now.value - since)
})

/** 超过这个时长没有任何输出，就提示"可能卡住"。 */
const IDLE_HINT_MS = 120_000

/** 活动节点的直播态。轮次要从这里取：快照得等这一轮结束才更新。 */
const activeLive = computed(() =>
  activeNode.value ? liveByNode.value[activeNode.value.id] : undefined,
)

/**
 * 轮次文案（`第 3/12 轮`）。
 *
 * 一个节点跑十分钟时，用户唯一能确认"它在动"的东西就是这个数字——
 * 正文可能半天不吐一个字，轮次至少每轮往前走一次。
 */
function turnText(node: DagNodeRun | null | undefined, state?: LiveState): string {
  const max = state?.maxTurns || node?.maxTurns || 0
  if (!max) return ''
  const current = state?.turn || node?.turns || 0
  return current ? `第 ${current}/${max} 轮` : `上限 ${max} 轮`
}

/** 选中节点已经多久没动静（毫秒）。只在运行中有意义。 */
const selectedIdle = computed(() => {
  const node = selectedRun.value
  if (!node || node.status !== 'running') return 0
  const since = selectedLive.value?.at ?? node.startedAt
  return since ? Math.max(0, now.value - since) : 0
})

/**
 * 还没有正文时的占位：把"轮次 + 静默多久"写出来。
 *
 * 一句"正在等模型的输出…"跟卡住没有区别；给出数字，用户才知道该继续等还是该停。
 */
const livePlaceholder = computed(() => {
  const turn = turnText(selectedRun.value, selectedLive.value)
  const parts = ['还没有正文']
  if (turn) parts.push(turn)
  parts.push(`已静默 ${formatDuration(selectedIdle.value) || '0s'}`)
  return `${parts.join(' · ')}\n\n模型在思考或写长回复时可能一两分钟不吐字；轮次往前走就说明还在跑。`
})

/**
 * 跑完之后的结果：成功几个、失败几个、为什么失败。
 *
 * 以前跑完只剩状态条变色，用户还得逐个点节点才能知道到底成了没有。
 */
const runResult = computed(() => {
  const nodes = run.value?.nodes ?? []
  return {
    ok: nodes.filter(node => node.status === 'succeeded').length,
    skipped: nodes.filter(node => node.status === 'skipped').length,
    cancelled: nodes.filter(node => node.status === 'cancelled').length,
    failed: nodes.filter(node => node.status === 'failed'),
    total: nodes.length,
  }
})

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
  // 事件通道之外再加一条轮询兜底：跑着的时候每 2s 拉一次快照。
  // 事件要是因为版本不匹配、IPC 卡顿而断了，进度最多慢 2 秒，不会整场空白。
  if (live.value && !poll) {
    poll = window.setInterval(() => {
      if (plan.value) void refreshRun(plan.value.id)
    }, 2000)
  }
  if (!live.value && poll) {
    window.clearInterval(poll)
    poll = 0
  }
}

/** 只认这张计划的快照：别的计划推过来的事件直接丢掉，免得串台。 */
function adoptRun(next: DagRun | null | undefined): void {
  if (!next) return
  if (plan.value && next.planId !== plan.value.id) return
  run.value = next
  now.value = Date.now()
  pruneLive(next)
  syncTicker()
}

/**
 * 丢掉已经落定节点的直播态。
 *
 * 节点一旦不在「运行中/待确认」，快照里就有完整正文与完整步骤，直播增量再留着
 * 只会让面板显示两份内容（而且增量是截断的）。
 */
function pruneLive(snapshot: DagRun): void {
  const current = liveByNode.value
  let changed = false
  const next: Record<string, LiveState> = {}
  for (const [id, state] of Object.entries(current)) {
    const node = snapshot.nodes.find(item => item.id === id)
    if (node && (node.status === 'running' || node.status === 'waiting')) {
      next[id] = state
    } else {
      changed = true
    }
  }
  if (changed) liveByNode.value = next
}

/** 收到一条增量：叠到该节点的直播态上（正文追加、步骤去尾、token 覆盖）。 */
function applyLive(event: NodeLiveEvent): void {
  // 只认当前这张计划的运行：换计划或换运行后，旧事件一律丢弃。
  if (!run.value || event.runId !== run.value.id) return
  const previous = liveByNode.value[event.nodeId]
  const state: LiveState = previous
    ? { ...previous, steps: [...previous.steps] }
    : { text: '', steps: [], tokensIn: 0, tokensOut: 0, at: Date.now(), turn: 0, maxTurns: 0 }
  if (event.delta) {
    state.text += event.delta
    if (state.text.length > LIVE_TEXT_LIMIT) state.text = state.text.slice(-LIVE_TEXT_LIMIT)
  }
  if (event.step) {
    state.steps.push(event.step)
    if (state.steps.length > 60) state.steps = state.steps.slice(-60)
  }
  state.tokensIn = event.tokensIn
  state.tokensOut = event.tokensOut
  if (event.turn) state.turn = event.turn
  if (event.maxTurns) state.maxTurns = event.maxTurns
  state.at = Date.now()
  liveByNode.value = { ...liveByNode.value, [event.nodeId]: state }
}

/**
 * 实时正文跟着滚到底。
 *
 * 只在用户本来就在底部时才自动滚——他往上翻看前文的时候，不该被拽回来。
 */
watch(
  () => selectedLive.value?.text ?? '',
  async () => {
    const el = livePre.value
    if (!el) return
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 80
    if (!nearBottom && el.scrollTop > 0) return
    await nextTick()
    el.scrollTop = el.scrollHeight
  },
)

async function startRun(): Promise<void> {
  const current = plan.value
  if (!current || runBusy.value || blocker.value) return
  runBusy.value = true
  try {
    // 上一次运行留下的直播态要清掉，否则新一次开跑会先闪一段旧正文。
    liveByNode.value = {}
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

/** 节点实际会用的权限：节点覆盖 → 计划默认，再与 Agent 画像取严。 */
function effectivePermission(
  agentId: string | null | undefined,
  own: AgentPermission | null,
): AgentPermission {
  const planned = own ?? plan.value?.defaults.permission ?? 'workspace-write'
  const agent = agents.value.find(item => item.id === (agentId ?? plan.value?.defaults.agent))
  if (!agent) return planned
  const rank: Record<AgentPermission, number> = {
    'read-only': 0,
    'workspace-write': 1,
    'danger-full-access': 2,
  }
  return rank[agent.permission] <= rank[planned] ? agent.permission : planned
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
    const live = liveByNode.value[node.id]
    // 轮次放最前面：正文半天不动的时候，至少这个数字在走。
    const turn = turnText(item, live)
    const detail = live?.steps.length
      ? live.steps[live.steps.length - 1]
      : item.steps.length
        ? item.steps[item.steps.length - 1]
        : live?.text
          ? '正在输出…'
          : '正在思考…'
    return turn ? `${turn} · ${detail}` : detail
  }
  if (item?.status === 'failed' && item.error) return item.error
  return node.task || '还没有任务描述'
}

// ---- Planner ----

const plannerOpen = ref(false)
const plannerGoal = ref('')
const plannerProject = ref('')
const plannerMax = ref(6)
/** 归档之外、这次手动挑的目录（新克隆的仓库还没被引擎扫到）。 */
const extraProjects = ref<{ value: string; label: string; hint?: string }[]>([])

/**
 * 打开弹层那一刻的底：之后靠它判断「用户到底动过没有」。
 * 没动过的空白弹层点一下就关掉没关系，动过就不能静默丢。
 */
const plannerBaseline = ref({ goal: '', project: '', max: 6 })
const plannerDirty = computed(
  () =>
    plannerGoal.value !== plannerBaseline.value.goal ||
    plannerProject.value !== plannerBaseline.value.project ||
    plannerMax.value !== plannerBaseline.value.max,
)
/** 被拦下时的说明；只活几秒，不作为常驻文案。 */
const plannerHold = ref('')
let plannerHoldTimer = 0

function flashPlannerHold(text: string): void {
  plannerHold.value = text
  if (plannerHoldTimer) window.clearTimeout(plannerHoldTimer)
  plannerHoldTimer = window.setTimeout(() => {
    plannerHold.value = ''
    plannerHoldTimer = 0
  }, 3200)
}

/** 关弹层只走这一个口：顺手把提示也收掉。 */
function closePlanner(): void {
  plannerOpen.value = false
  plannerHold.value = ''
  if (plannerHoldTimer) {
    window.clearTimeout(plannerHoldTimer)
    plannerHoldTimer = 0
  }
}

/**
 * 点遮罩关弹层——但有两条限制，见 dialogDismiss.ts：
 * 拖选文字不算点空白；填过东西（或正在规划）时拦下，给一句提示。
 */
const plannerBackdrop = useBackdropDismiss({
  dismiss: closePlanner,
  hasUnsavedWork: () => planning.value || plannerDirty.value,
  onHold: () =>
    flashPlannerHold(
      planning.value
        ? '正在规划：草案出来会自动关掉，先别关'
        : '已填的内容还在：点「取消」才关掉这层',
    ),
})

/** 制品预览只读，没有要保护的内容，同样只挡「拖选文字误触」。 */
const artifactBackdrop = useBackdropDismiss({
  dismiss: () => (artifactPreview.value = null),
})

/** Esc 等同于「取消」：键盘是明确意图，不因为填过内容就拦。 */
function onPlannerEscape(event: KeyboardEvent): void {
  if (event.key !== 'Escape' || !plannerOpen.value) return
  if (planning.value) {
    flashPlannerHold('正在规划：草案出来会自动关掉，先别关')
    return
  }
  closePlanner()
}

/**
 * 目录选项：已归档的项目 + 本次新挑的目录。
 *
 * 值就是绝对路径（计划里存的是路径，不是项目 id），label 用项目名，
 * 路径放进 hint —— 同名项目靠第二行区分。和工作台的新建页同一套交互。
 */
const projectOptions = computed(() => {
  const seen = new Set<string>()
  const list: { value: string; label: string; hint?: string }[] = []
  for (const item of props.projects) {
    const path = item.worktree?.trim()
    if (!path || seen.has(path)) continue
    seen.add(path)
    list.push({ value: path, label: item.name || basename(path), hint: path })
  }
  for (const item of extraProjects.value) {
    if (!seen.has(item.value)) list.push(item)
  }
  return list
})

/** 弹层底部「选择文件夹…」：挑完直接成为当前目录，再挑一次也不会重复入列。 */
async function pickProjectFolder(): Promise<void> {
  try {
    const picked = await openDirectoryDialog({
      directory: true,
      multiple: false,
      title: '选择项目目录',
    })
    if (typeof picked !== 'string' || !picked) return
    if (!projectOptions.value.some(option => option.value === picked)) {
      extraProjects.value = [
        ...extraProjects.value,
        { value: picked, label: basename(picked) || picked, hint: picked },
      ]
    }
    plannerProject.value = picked
  } catch (cause) {
    report(cause)
  }
}

function openPlanner(): void {
  plannerGoal.value = plan.value?.goal ?? ''
  plannerProject.value = plan.value?.project || defaultProject.value
  plannerBaseline.value = {
    goal: plannerGoal.value,
    project: plannerProject.value,
    max: plannerMax.value,
  }
  plannerHold.value = ''
  plannerBackdrop.reset()
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
    closePlanner()
    notice.value = '草案已生成（未保存）：改完点「保存计划」'
    await nextTick()
    fitView()
  } catch (cause) {
    // 除了弹一下提示，还把原因留在界面那一行：toast 六秒就没了，回来想再看
    // 一眼失败原因就找不到了，只剩"刚才好像没反应"。
    const message = cause instanceof Error ? cause.message : String(cause)
    notice.value = `生成失败：${message}`
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
    inputArtifacts: [],
    outputArtifactKey: '',
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
  window.addEventListener('keydown', onPlannerEscape)
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
  // 运行中的正文 / 步骤走增量事件：没有它，节点跑十分钟面板就是十分钟空白。
  void listen<NodeLiveEvent>(LIVE_EVENT_NAME, envelope => applyLive(envelope.payload)).then(off => {
    unlistenLive = off
  })
})

onBeforeUnmount(() => {
  window.removeEventListener('keydown', onPlannerEscape)
  if (copiedTimer) {
    window.clearTimeout(copiedTimer)
    copiedTimer = 0
  }
  if (plannerHoldTimer) {
    window.clearTimeout(plannerHoldTimer)
    plannerHoldTimer = 0
  }
  unlisten?.()
  unlisten = null
  unlistenLive?.()
  unlistenLive = null
  if (ticker) {
    window.clearInterval(ticker)
    ticker = 0
  }
  if (poll) {
    window.clearInterval(poll)
    poll = 0
  }
})

/** 切计划时把运行快照一起换掉：不换会显示上一张图的进度。 */
watch(
  () => plan.value?.id ?? null,
  id => {
    run.value = null
    liveByNode.value = {}
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
          <span class="run-state" :class="gates.length ? 'warn' : runMeta(run.status).tone">
            <i class="run-dot" />
            {{ gates.length ? '等你确认' : runMeta(run.status).label }}
          </span>
          <span class="run-track" :title="`${progress.settled}/${progress.total} 已落定`">
            <i :class="runMeta(run.status).tone" :style="{ width: `${progress.percent}%` }" />
          </span>
          <span class="run-count mono">{{ runSummary(run) }}</span>
          <span v-if="totalTokens.tin || totalTokens.tout" class="run-tokens mono">
            ↑{{ formatTokens(totalTokens.tin) }} ↓{{ formatTokens(totalTokens.tout) }}
            <span v-if="cachePercent(totalTokens.cached, totalTokens.tin)" class="run-cache"
              :title="`缓存命中 ${formatTokens(totalTokens.cached)} / ${formatTokens(totalTokens.tin)} 输入`"
              >⚡{{ cachePercent(totalTokens.cached, totalTokens.tin) }}%</span>
          </span>
          <span v-if="elapsed" class="run-clock mono">{{ formatDuration(elapsed) }}</span>
          <span class="spacer" />
          <button class="ghost-btn tiny" title="收起运行条（运行本身不受影响）" @click="run = null">
            收起
          </button>
        </div>

        <!-- 正在跑的那一步：比以前只有"运行中"三个字，能看出到底卡在哪 -->
        <div v-if="activeNode" class="run-activity">
          <span class="run-activity-node">{{ activeNode.title || activeNode.id }}</span>
          <span v-if="turnText(activeNode, activeLive)" class="run-activity-turn mono">
            {{ turnText(activeNode, activeLive) }}
          </span>
          <span class="run-activity-step mono">{{ activeStep }}</span>
          <span
            class="run-activity-idle mono"
            :class="{ slow: idleFor > 45_000, stuck: idleFor > IDLE_HINT_MS }"
            :title="`最后一次收到输出是在 ${formatDuration(idleFor)} 前`"
          >
            静默 {{ formatDuration(idleFor) || '0s' }}
          </span>
          <span class="run-activity-clock mono">{{ formatDuration(nodeElapsed(activeNode, now)) }}</span>
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

        <!-- 跑完之后一句话说清结果：以前只有状态条换个颜色，得逐个点节点才知道成没成 -->
        <div v-if="run && !live && !gates.length" class="run-result" :class="runResult.failed.length ? 'bad' : 'good'">
          <span class="run-result-title">
            {{ runResult.failed.length ? '有节点失败' : runResult.cancelled ? '已取消' : '跑完了' }}
          </span>
          <span class="run-result-text">
            成功 {{ runResult.ok }}/{{ runResult.total }}
            <template v-if="runResult.failed.length"> · 失败 {{ runResult.failed.length }}</template>
            <template v-if="runResult.skipped"> · 跳过 {{ runResult.skipped }}</template>
            <template v-if="runResult.cancelled"> · 取消 {{ runResult.cancelled }}</template>
          </span>
          <div v-for="node in runResult.failed.slice(0, 3)" :key="`fail-${node.id}`" class="run-result-fail">
            <b>{{ node.title || node.id }}</b>
            <span class="mono">{{ node.error || '没有留下错误信息' }}</span>
          </div>
        </div>
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
            <label class="field">
              <span class="field-label">产出制品 key <em>留空 = report</em></span>
              <input
                v-model="selectedNode.outputArtifactKey"
                placeholder="report"
                spellcheck="false"
              />
            </label>
            <p class="hint small">
              跑完的正文会自动存成制品，下游用 <code>{{ ARTIFACT_HINT }}</code> 取摘要与引用。
              每个上游的主产出默认以「节点 id」为别名，不用配。
            </p>
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
                <b>{{ permissionLabel(effectivePermission(selectedNode.agent, selectedNode.permission)) }}</b>
              </div>
              <p
                v-if="selectedNode.permission !== null
                  && effectivePermission(selectedNode.agent, selectedNode.permission) !== selectedNode.permission"
                class="hint small"
              >
                节点要的是「{{ permissionLabel(selectedNode.permission) }}」，但 Agent 的权限上限更低——
                实际按上面那档跑。节点只能收紧，不能提权。
              </p>
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
                <span class="kv"><span>轮次</span><b>{{ turnText(selectedRun, selectedLive) || '—' }}</b></span>
                <span class="kv"><span>工具</span><b>{{ selectedSteps.length }} 次</b></span>
                <span class="kv"><span>耗时</span><b>{{ formatDuration(selectedElapsed) || '—' }}</b></span>
                <span class="kv"><span>Token</span><b>↑{{ formatTokens(selectedTokens.tin) }} ↓{{ formatTokens(selectedTokens.tout) }}</b></span>
                <span v-if="cachePercent(selectedTokens.cached, selectedTokens.tin)" class="kv">
                  <span>缓存</span><b>⚡{{ cachePercent(selectedTokens.cached, selectedTokens.tin) }}%</b>
                </span>
                <span v-if="selectedRun.status === 'running'" class="kv">
                  <span>静默</span>
                  <b :class="{ 'kv-warn': selectedIdle > 45_000 }">{{ formatDuration(selectedIdle) || '刚刚' }}</b>
                </span>
              </div>

              <div v-if="selectedRun.status === 'waiting'" class="run-gate inline">
                <span class="run-gate-text">等你放行。</span>
                <button class="ghost-btn tiny" :disabled="runBusy" @click="decideGate(selectedRun.id, false)">拒绝</button>
                <button class="primary-btn compact" :disabled="runBusy" @click="decideGate(selectedRun.id, true)">放行</button>
              </div>

              <p v-if="selectedRun.error" class="run-error">{{ selectedRun.error }}</p>

              <div v-if="selectedSteps.length" class="run-steps">
                <div class="run-steps-title">过程</div>
                <div v-for="(step, index) in selectedSteps" :key="index" class="run-step">
                  <span class="run-step-index mono">{{ index + 1 }}</span>
                  <span class="run-step-text mono">{{ step }}</span>
                </div>
              </div>

              <div v-if="selectedRun.failures?.length" class="run-steps">
                <div class="run-steps-title">工具失败（已随上游清单传给下游）</div>
                <div v-for="(failure, index) in selectedRun.failures" :key="`fail-${index}`" class="run-step">
                  <span class="run-step-index mono">!</span>
                  <span class="run-step-text mono">{{ failure }}</span>
                </div>
              </div>

              <div v-if="selectedRun.artifact" class="run-artifact">
                <div class="run-steps-title">制品</div>
                <div class="art-row">
                  <span class="art-key">{{ selectedRun.artifact.key }}</span>
                  <span class="art-meta mono">{{ selectedRun.artifact.kind }} · {{ formatBytes(selectedRun.artifact.size) }}</span>
                  <span class="art-sha mono" :title="selectedRun.artifact.sha256">
                    {{ selectedRun.artifact.sha256.slice(0, 12) }}
                  </span>
                </div>
                <div class="art-id mono" :title="selectedRun.artifact.id">{{ selectedRun.artifact.id }}</div>
                <div class="art-actions">
                  <button
                    class="ghost-btn tiny"
                    :disabled="artifactBusy"
                    @click="openArtifact(selectedRun.artifact)"
                  >
                    {{ artifactBusy ? '读取中…' : '查看全文' }}
                  </button>
                  <button
                    class="ghost-btn tiny"
                    @click="copyText(selectedRun.artifact.id, 'artifact-id')"
                  >
                    {{ copiedKey === 'artifact-id' ? '已复制' : '复制 id' }}
                  </button>
                  <span v-if="selectedRun.artifact.parentId" class="art-parent mono">
                    派生自 {{ selectedRun.artifact.parentId }}
                  </span>
                </div>
              </div>

              <!-- 实时正文：跑到一半也能看见它正在写什么，而不是干等 -->
              <div v-if="selectedRun.status === 'running'" class="run-live">
                <div class="run-steps-title">实时</div>
                <pre ref="livePre" class="run-pre live">{{ selectedLive?.text || livePlaceholder }}</pre>
              </div>

              <div v-if="selectedRun.output" class="run-output">
                <div class="run-steps-title row">
                  <span>产出</span>
                  <span class="run-title-actions">
                    <button
                      class="ghost-btn tiny"
                      :title="outputView === 'preview' ? '看 markdown 原文' : '看渲染后的效果'"
                      @click="outputView = outputView === 'preview' ? 'raw' : 'preview'"
                    >
                      {{ outputView === 'preview' ? '原文' : '预览' }}
                    </button>
                    <button
                      class="ghost-btn tiny"
                      title="复制这份报告的 markdown 原文"
                      @click="copyText(selectedRun.output, 'output')"
                    >
                      {{ copiedKey === 'output' ? '已复制' : '复制' }}
                    </button>
                  </span>
                </div>
                <div
                  v-if="outputView === 'preview'"
                  class="run-pre md"
                  v-html="renderMarkdown(selectedRun.output)"
                ></div>
                <pre v-else class="run-pre">{{ selectedRun.output }}</pre>
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

    <div
      v-if="artifactPreview"
      class="planner-backdrop"
      @mousedown="artifactBackdrop.onMousedown"
      @click="artifactBackdrop.onClick"
    >
      <div class="artifact-card">
        <div class="artifact-head">
          <div>
            <div class="planner-kicker">制品 · {{ artifactPreview.ref.key }}</div>
            <div class="artifact-title">{{ artifactPreview.ref.name }}</div>
          </div>
          <div class="artifact-head-actions">
            <button
              v-if="artifactIsText"
              class="ghost-btn tiny"
              :title="artifactView === 'preview' ? '看原文' : '看渲染后的效果'"
              @click="artifactView = artifactView === 'preview' ? 'raw' : 'preview'"
            >
              {{ artifactView === 'preview' ? '原文' : '预览' }}
            </button>
            <button
              class="ghost-btn tiny"
              @click="copyText(artifactPreview.text, 'artifact')"
            >
              {{ copiedKey === 'artifact' ? '已复制' : '复制全文' }}
            </button>
            <button class="ghost-btn" @click="artifactPreview = null">关闭</button>
          </div>
        </div>
        <div class="artifact-meta mono">
          {{ formatBytes(artifactPreview.ref.size) }} · sha256 {{ artifactPreview.ref.sha256 }}
        </div>
        <div
          v-if="artifactIsText && artifactView === 'preview'"
          class="artifact-pre md"
          v-html="renderMarkdown(artifactPreview.text)"
        ></div>
        <pre v-else class="artifact-pre">{{ artifactPreview.text }}</pre>
        <div v-if="artifactPreview.truncated" class="artifact-note">
          只显示前 4 万字符：全文在 <code>~/.prism/artifacts/</code> 下，按 id 可以继续取。
        </div>
      </div>
    </div>

    <div
      v-if="plannerOpen"
      class="planner-backdrop"
      @mousedown="plannerBackdrop.onMousedown"
      @click="plannerBackdrop.onClick"
    >
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
          <div class="field">
            <span class="field-label">项目目录</span>
            <SelectMenu
              v-model="plannerProject"
              :options="projectOptions"
              placeholder="选择或新增目录…"
              :title="plannerProject || '项目目录'"
              action-label="选择文件夹…"
              @action="pickProjectFolder"
            />
            <p v-if="plannerProject" class="path-hint" :title="plannerProject">
              {{ plannerProject }}
            </p>
          </div>
          <div class="pc-row">
            <label class="field">
              <span class="field-label">节点上限 <em>2–24</em></span>
              <input v-model.number="plannerMax" type="number" min="2" max="24" />
            </label>
            <div class="field">
              <span class="field-label">候选 Agent</span>
              <div class="readonly-field">{{ agents.length }} 个 · 内置 + 自定义</div>
            </div>
          </div>
          <p class="hint small">
            规划用的是设置里「原生引擎」的当前模型；没配置会在这里直接报错。
          </p>
        </div>
        <div class="planner-foot">
          <span v-if="plannerHold" class="planner-hold">{{ plannerHold }}</span>
          <span class="grow" />
          <button class="ghost-btn" title="关闭（Esc）" @click="closePlanner">取消</button>
          <button
            class="primary-btn compact"
            :disabled="planning || !plannerGoal.trim()"
            @click="runPlanner"
          >
            {{ planning ? '规划中…' : '生成 DAG 草案' }}
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped src="../styles/components/DagPane.css"></style>
