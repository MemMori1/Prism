<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { api } from '../api'
import { joinPath } from '../changes'
import { copyPlainText } from '../clipboard'
import ChangesPane from './ChangesPane.vue'
import BackgroundTasks from './BackgroundTasks.vue'
import { annotationFor, ensureAnnotations, isStarred, setStar, setTitle, tagsOf, titleOf } from '../annotations'
import {
  applyCompletion,
  commandInvocation,
  detectTrigger,
  fileLabel,
  modelKeyOf,
  modelLabel,
  recordModelKey,
  shellCommand,
  type Trigger,
} from '../composer'
import { onEngineEvent } from '../engineEvents'
import {
  coveredByLive,
  createLiveTurn,
  liveCoverage,
  reduceLiveTurn,
  type LiveCoverage,
  type LiveTurn,
} from '../liveTurn'
import { pushNotification } from '../notify'
import {
  mergePermissions,
  permissionFromEvent,
  permissionHeadline,
  permissionResources,
  removePermission,
  repliedRequestId,
  type PermissionDecision,
} from '../permissions'
import { engineLabel } from '../workshop'
import { CREATABLE_ENGINE_IDS } from '../sessionStart'
import type {
  ContextStats,
  EngineCatalog,
  EngineCommand,
  FileHit,
  PermissionRequest,
  SendOptions,
  SessionSummary,
  TimelinePage,
  TimelineRecord,
} from '../types'
import { formatCost, formatTokens, fullTime } from '../format'
import { turnDurations } from '../timeline'
import TimelineItem from './TimelineItem.vue'
import LiveTurnView from './LiveTurn.vue'
import ArtifactViewer from './ArtifactViewer.vue'
import TagPicker from './TagPicker.vue'
import ModelMenu from './ModelMenu.vue'
import BranchPicker from './BranchPicker.vue'
import PermissionPicker from './PermissionPicker.vue'
import ContextRing from './ContextRing.vue'
import ContextPanel from './ContextPanel.vue'
import { contextWindowOf } from '../modelIcons'
import { charsOfRecords, contextBreakdown, type ContextChars } from '../contextShare'
import { DEFAULT_APPROVAL, loadApproval, saveApproval, type ApprovalMode } from '../approval'

const props = withDefaults(
  defineProps<{
    session: SessionSummary | null
    hasProject: boolean
    /** 工作台列内嵌模式：隐藏会话头（由列头承担标题 / 状态）。 */
    embedded?: boolean
    /** 是否显示输入框上方的任务坞（工作台非聚焦列关掉，避免每列顶一条）。 */
    showTasks?: boolean
    /** 搜索结果定位：滚动到该消息并短暂高亮。 */
    jumpMessageId?: string | null
  }>(),
  // Boolean 型 prop 没传时 Vue 会当 false，任务坞默认要开。
  { showTasks: true },
)

const emit = defineEmits<{
  (e: 'error', message: string): void
  (e: 'settled'): void
  (e: 'jumpConsumed'): void
  (e: 'openWorkbench', sessionId: string): void
  (e: 'status', status: { running: boolean; waiting: number }): void
  /** 切了分支：外层刷新项目列表，让分支徽标跟着变。 */
  (e: 'branchChanged', branch: string): void
}>()

const scroller = ref<HTMLElement | null>(null)
const records = ref<TimelineRecord[]>([])
const cursor = ref<number | null>(null)
const hasMore = ref(false)
const loading = ref(false)
const loadingOlder = ref(false)

// ---- 运行轴（live）状态 ----

const live = ref<LiveTurn>(createLiveTurn())
const liveStartedAt = ref<number | null>(null)
const draft = ref('')
const composerInput = ref<HTMLTextAreaElement | null>(null)
const sending = ref(false)
const stopping = ref(false)
/** 挂起的权限请求（引擎阻塞等待审批）。 */
const pendingPermissions = ref<PermissionRequest[]>([])
/** 正在回复中的权限请求 id（防连点）。 */
const replyingPermission = ref<string | null>(null)
/** 视口是否吸附在底部（跟随流式输出）。 */
const stick = ref(true)
/** 搜索定位的目标消息（短暂高亮）。 */
const highlightId = ref<string | null>(null)
/** 交付物就地预览：当前打开的本地文件路径。 */
const artifactPath = ref<string | null>(null)

/** 权限档位：只对原生引擎有意义（别的引擎在各自应用里管权限）。 */
const approvalMode = ref<ApprovalMode>(DEFAULT_APPROVAL)

watch(
  () => props.session?.id,
  () => {
    approvalMode.value = loadApproval(props.session?.id)
  },
  { immediate: true },
)

watch(approvalMode, mode => saveApproval(props.session?.id, mode))

/** 这个会话是不是 Prism 内置引擎（只有它吃这里的权限档位）。 */
const nativeSession = computed(() => props.session?.engine === 'native')

/**
 * 执行中按回车不直接插话，先停在「待发送」——当前输出还在流，突然插一句
 * 容易让人以为打断了；要不要立刻送进去由卡片上的按钮决定。
 */
const queued = ref('')

/** 「面板」：一个能开多个标签页的侧边抽屉（文件 / 终端 / 以后的网页）。 */
const changesOpen = ref(false)
/** 要侧栏打开什么；每次 +1 让同样的意图再触发一次。 */
const paneTarget = ref<
  | { kind: 'files'; path?: string | null }
  | { kind: 'term'; taskId?: string | null; newTask?: boolean }
  | null
>(null)
const paneTick = ref(0)

function openPane(target: NonNullable<typeof paneTarget.value>): void {
  paneTarget.value = target
  paneTick.value += 1
  changesOpen.value = true
}

/** 未吸附底部时累计的新消息数（回底浮标角标）。 */
const unseen = ref(0)

// ---- 组合器（M20）：模型 / 档位 / @ 文件 / / 命令 / ! shell ----

/** 引擎能力目录（模型 + 斜杠命令；聚焦输入框时惰性加载）。 */
const catalog = ref<EngineCatalog | null>(null)
const catalogLoading = ref(false)
/** 已选文件附件（发送时作为引擎结构化附件）。 */
const attachments = ref<FileHit[]>([])
/** 当前模型 key（provider::id 或 id；空 = 引擎默认）与推理档位。 */
const modelKey = ref('')
const variant = ref('')
const popup = ref<ComposerPopup | null>(null)
/** 一次性提示（如 shell 已执行）。 */
const notice = ref<string | null>(null)
let popupTimer: number | null = null

interface ComposerPopup {
  kind: 'file' | 'command'
  items: (FileHit | EngineCommand)[]
  active: number
  trigger: Trigger
}

const sessionModels = computed(() => catalog.value?.models ?? [])
const selectedModel = computed(
  () => sessionModels.value.find(model => modelKeyOf(model) === modelKey.value) ?? null,
)
const sessionVariants = computed(() => selectedModel.value?.variants ?? [])
/** 底部工具条「模型 + 推理强度」合并下拉的模型选项。
 *  label 给弹层（带 provider / 默认标记），short 给触发器（模型 id，避免把档位挤掉）。 */
const modelOptions = computed(() => [
  { value: '', label: catalogLoading.value ? '加载中…' : '默认', short: '默认' },
  ...sessionModels.value.map(model => ({
    value: modelKeyOf(model),
    label: modelLabel(model),
    short: model.id,
  })),
])
/** ! 开头 = 本机 shell 直跑。 */
const shellMode = computed(() => shellCommand(draft.value) !== null)
/** /name = 斜杠命令调用预览。 */
const commandPreview = computed(() =>
  commandInvocation(draft.value.trim(), catalog.value?.commands ?? []),
)

const PREFS_KEY = 'prism-composer-prefs-v1'

interface ComposerPrefs {
  modelKey: string
  variant: string
}

function readPrefs(engine: string): ComposerPrefs {
  try {
    const raw = localStorage.getItem(PREFS_KEY)
    if (!raw) return { modelKey: '', variant: '' }
    const all = JSON.parse(raw) as Record<string, ComposerPrefs>
    return all[engine] ?? { modelKey: '', variant: '' }
  } catch {
    return { modelKey: '', variant: '' }
  }
}

function writePrefs(engine: string, prefs: ComposerPrefs) {
  try {
    const raw = localStorage.getItem(PREFS_KEY)
    const all = raw ? (JSON.parse(raw) as Record<string, ComposerPrefs>) : {}
    all[engine] = prefs
    localStorage.setItem(PREFS_KEY, JSON.stringify(all))
  } catch {
    // 存不下就算了：只是偏好。
  }
}

/**
 * 偏好只记「用户显式选择」；打开会话时的回显不写，避免把某个会话的状态
 * 当成这个引擎的默认偏好。
 */
function savePrefs() {
  if (!props.session) return
  writePrefs(props.session.engine, { modelKey: modelKey.value, variant: variant.value })
}

function setModel(value: string) {
  if (value === modelKey.value) return
  modelKey.value = value
  // 换模型时该档位若不在新模型的档位表里就回默认；目录未收录的模型不动档位
  // （回显值来自引擎本身，可能不在 catalog 里，不能凭目录缺失就清掉）。
  const next = sessionModels.value.find(model => modelKeyOf(model) === value)
  if (variant.value && next && !next.variants.includes(variant.value)) variant.value = ''
  savePrefs()
}

function setVariant(value: string) {
  if (value === variant.value) return
  variant.value = value
  savePrefs()
}

/**
 * 回显：拿该会话最近一轮真实用过的模型 / 档位初始化下拉。
 * 历史里没有模型信息时保留上面装载的引擎偏好。
 */
function hydrateModelPick(list: TimelineRecord[]) {
  for (let index = list.length - 1; index >= 0; index -= 1) {
    const record = list[index]
    if (record.role !== 'assistant') continue
    const key = recordModelKey(record)
    if (!key) continue
    modelKey.value = key
    variant.value = record.variant ?? ''
    return
  }
}

/** 惰性加载引擎目录（聚焦输入框 / 用到模型或命令时）。 */
async function ensureCatalog() {
  const session = props.session
  if (!session || catalog.value || catalogLoading.value || readOnly.value) return
  catalogLoading.value = true
  try {
    catalog.value = await api.engineCatalog(session.engine, session.directory)
  } catch (cause) {
    report(cause)
  } finally {
    catalogLoading.value = false
  }
}

function closePopup() {
  popup.value = null
  if (popupTimer !== null) {
    window.clearTimeout(popupTimer)
    popupTimer = null
  }
}

/** 组合器候选列表：在光标处检测触发词并按需拉取。 */
function onComposerInput() {
  autoGrow()
  if (composerMenu.value) closeComposerMenu()
  const el = composerInput.value
  const trigger = detectTrigger(draft.value, el?.selectionStart ?? draft.value.length)
  if (!trigger) {
    closePopup()
    return
  }
  if (trigger.kind === 'command') {
    if (!catalog.value && !catalogLoading.value) {
      void ensureCatalog().then(() => onComposerInput())
      return
    }
    const term = trigger.term.toLowerCase()
    const items = (catalog.value?.commands ?? []).filter(command =>
      command.name.toLowerCase().includes(term),
    )
    popup.value = items.length ? { kind: 'command', items, active: 0, trigger } : null
    return
  }
  // 文件：防抖后向会话目录要索引
  if (popupTimer !== null) window.clearTimeout(popupTimer)
  const directory = props.session?.directory
  if (!directory) return
  popupTimer = window.setTimeout(async () => {
    popupTimer = null
    try {
      const hits = await api.projectFiles(directory, trigger.term, 20)
      const current = detectTrigger(draft.value, composerInput.value?.selectionStart ?? 0)
      if (!current || current.kind !== 'file' || current.start !== trigger.start) return
      popup.value = hits.length ? { kind: 'file', items: hits, active: 0, trigger: current } : null
    } catch {
      popup.value = null
    }
  }, 180)
}

async function pickPopup(index: number) {
  const current = popup.value
  const item = current?.items[index]
  if (!current || !item) return
  let replacement: string
  if (current.kind === 'file') {
    const file = item as FileHit
    if (!attachments.value.some(entry => entry.path === file.path)) {
      attachments.value = [...attachments.value, file]
    }
    replacement = `@${file.path}`
  } else {
    replacement = `/${(item as EngineCommand).name}`
  }
  const caret = composerInput.value?.selectionStart ?? draft.value.length
  const next = applyCompletion(draft.value, current.trigger, replacement, caret)
  draft.value = next.text
  closePopup()
  await nextTick()
  const el = composerInput.value
  if (el) {
    el.focus()
    el.setSelectionRange(next.caret, next.caret)
    autoGrow()
  }
}

function removeAttachment(file: FileHit) {
  attachments.value = attachments.value.filter(entry => entry.path !== file.path)
  const token = `@${file.path}`
  if (draft.value.includes(token)) {
    draft.value = draft.value.split(token).join('').replace(/[ \t]{2,}/g, ' ')
  }
}

// ---- 输入框工具条：+ 命令菜单 / 文件引用菜单 / 模型与档位 / 发送 ----

type ComposerMenu = 'command' | 'file'

const composerBox = ref<HTMLElement | null>(null)
const composerMenuEl = ref<HTMLElement | null>(null)
const commandBtn = ref<HTMLButtonElement | null>(null)
const fileBtn = ref<HTMLButtonElement | null>(null)
const menuSearch = ref<HTMLInputElement | null>(null)
/** 当前展开的工具条菜单（null = 都收起）。 */
const composerMenu = ref<ComposerMenu | null>(null)
const menuQuery = ref('')
const menuActive = ref(0)
const menuFiles = ref<FileHit[]>([])
const menuFilesLoading = ref(false)
const composerFocused = ref(false)
let fileSearchTimer: number | null = null
let fileSearchToken = 0

const menuCommands = computed(() => {
  const list = catalog.value?.commands ?? []
  const term = menuQuery.value.trim().toLowerCase()
  if (!term) return list
  return list.filter(
    command =>
      command.name.toLowerCase().includes(term) ||
      (command.description ?? '').toLowerCase().includes(term),
  )
})

/** 当前菜单的候选数（键盘导航/空态共用）。 */
const menuCount = computed(() =>
  composerMenu.value === 'file' ? menuFiles.value.length : menuCommands.value.length,
)

/** 发送按钮可用性：`!` 直跑要求命令非空，其余要求有文字或附件。 */
const canSend = computed(() => {
  const shell = shellCommand(draft.value)
  if (shell !== null) return shell.length > 0
  return !!draft.value.trim() || attachments.value.length > 0
})

/**
 * 上下文占用：取**最近一轮**的 token 用量当对话大小。
 *
 * 不用会话累计量——那是所有轮次的和，多聊几轮就会顶到 100%，看着像要爆了其实没有。
 * 分母优先用引擎给的 `contextWindow`（codex 有），否则按模型名查常见值，都没有就不画弧。
 */
const contextUsage = computed(() => {
  for (let index = records.value.length - 1; index >= 0; index -= 1) {
    const tokens = records.value[index].tokens
    if (!tokens) continue
    const used = tokens.input + tokens.cacheRead + tokens.output
    const fromEngine = tokens.contextWindow ?? null
    const window = fromEngine ?? contextWindowOf(modelKey.value)
    return { used, window, estimated: !fromEngine && !!window }
  }
  const fallback = contextWindowOf(modelKey.value)
  return { used: 0, window: fallback, estimated: true }
})

/** 上下文明细面板是否展开。 */
const ctxOpen = ref(false)
/** 引擎给的字符统计（只有原生引擎给得出系统提示与工具声明）。 */
const ctxStats = ref<ContextStats | null>(null)

watch(
  () => props.session?.id,
  async id => {
    ctxOpen.value = false
    ctxStats.value = null
    if (!id) return
    try {
      const stats = await api.contextStats(id)
      if (props.session?.id === id) ctxStats.value = stats
    } catch {
      // 拿不到统计不影响总量：面板会退回按时间线文字估。
    }
  },
  { immediate: true },
)

/** 四个分项的字符数：有引擎统计就用它，否则按时间线文字估。 */
const ctxChars = computed<ContextChars>(() => {
  const stats = ctxStats.value
  if (stats) {
    return {
      system: stats.system,
      tools: stats.tools,
      tool: stats.tool,
      dialog: stats.user + stats.assistant,
    }
  }
  return charsOfRecords(records.value)
})

const ctxSlices = computed(() =>
  contextBreakdown(contextUsage.value.used, contextUsage.value.window, ctxChars.value),
)

/** 点面板外面就收起（输入框里点字也算外面，但那时用户本来就在写字）。 */
function onCtxOutside(event: PointerEvent) {
  const target = event.target
  if (!(target instanceof HTMLElement)) return
  if (target.closest('.ctx-wrap')) return
  ctxOpen.value = false
}

/** Esc 也收：它是个浮层，键盘用户不该只能靠鼠标关。 */
function onCtxEscape(event: KeyboardEvent) {
  if (event.key !== 'Escape') return
  ctxOpen.value = false
}

watch(ctxOpen, open => {
  if (open) {
    document.addEventListener('pointerdown', onCtxOutside, true)
    document.addEventListener('keydown', onCtxEscape)
  } else {
    document.removeEventListener('pointerdown', onCtxOutside, true)
    document.removeEventListener('keydown', onCtxEscape)
  }
})

/** 工具条左侧的实时提示（发送中 / shell 模式 / 斜杠命令预览…）。 */
const composerHint = computed(() => {
  if (stopping.value) return '正在中断…'
  if (sending.value) return '发送中…'
  if (notice.value) return notice.value
  if (shellMode.value) return '! 开头 = 本机直跑 shell（不发给模型）'
  if (running.value) return '执行中，发送即插话'
  if (commandPreview.value) return `斜杠命令 /${commandPreview.value.name}`
  return ''
})

function onComposerFocus() {
  composerFocused.value = true
  void ensureCatalog()
}

function scheduleFileSearch(query: string) {
  if (fileSearchTimer !== null) window.clearTimeout(fileSearchTimer)
  // 每次调度都推进 token：上一次还在路上的请求回来时会被丢弃。
  const token = ++fileSearchToken
  const directory = props.session?.directory
  if (!directory) {
    menuFiles.value = []
    menuFilesLoading.value = false
    return
  }
  menuFilesLoading.value = true
  fileSearchTimer = window.setTimeout(() => {
    fileSearchTimer = null
    void loadMenuFiles(query, token)
  }, 160)
}

async function loadMenuFiles(query: string, token: number) {
  const directory = props.session?.directory
  if (!directory) return
  try {
    const hits = await api.projectFiles(directory, query, 30)
    if (token !== fileSearchToken) return
    menuFiles.value = hits
    menuActive.value = 0
  } catch {
    if (token === fileSearchToken) menuFiles.value = []
  } finally {
    if (token === fileSearchToken) menuFilesLoading.value = false
  }
}

function scrollMenuActive() {
  const node = composerMenuEl.value?.querySelectorAll<HTMLElement>('.menu-item')[menuActive.value]
  node?.scrollIntoView({ block: 'nearest' })
}

async function toggleComposerMenu(kind: ComposerMenu) {
  if (composerMenu.value === kind) {
    closeComposerMenu()
    return
  }
  composerMenu.value = kind
  menuQuery.value = ''
  menuActive.value = 0
  closePopup()
  if (kind === 'command') {
    await ensureCatalog()
  } else {
    scheduleFileSearch('')
  }
  await nextTick()
  menuSearch.value?.focus()
}

function closeComposerMenu(refocus = false) {
  if (fileSearchTimer !== null) {
    window.clearTimeout(fileSearchTimer)
    fileSearchTimer = null
  }
  if (!composerMenu.value) return
  composerMenu.value = null
  menuQuery.value = ''
  menuActive.value = 0
  if (refocus) void nextTick(() => composerInput.value?.focus())
}

/** 在光标处插入内容（自动补一个前导空格），插完把光标放到末尾。 */
function insertComposerToken(token: string) {
  const el = composerInput.value
  const start = el?.selectionStart ?? draft.value.length
  const end = el?.selectionEnd ?? start
  const before = draft.value.slice(0, start)
  const after = draft.value.slice(end)
  const pad = before && !/\s$/.test(before) ? ' ' : ''
  const inserted = `${pad}${token}`
  draft.value = before + inserted + after
  void nextTick(() => {
    const input = composerInput.value
    if (!input) return
    const caret = start + inserted.length
    input.focus()
    input.setSelectionRange(caret, caret)
    autoGrow()
  })
}

/**
 * 选中命令：斜杠命令只有在整段文本开头才会被识别，
 * 所以统一放到最前面（已有命令则替换命令名，保留后面当参数）。
 */
function pickMenuCommand(command: EngineCommand) {
  const token = `/${command.name} `
  const rest = draft.value.trimStart()
  if (!rest) {
    draft.value = token
  } else if (rest.startsWith('/')) {
    draft.value = `${token}${rest.replace(/^\/\S*\s*/, '')}`
  } else {
    draft.value = `${token}${rest}`
  }
  closeComposerMenu()
  void nextTick(() => {
    const input = composerInput.value
    if (!input) return
    const caret = draft.value.length
    input.focus()
    input.setSelectionRange(caret, caret)
    autoGrow()
  })
}

function pickMenuFile(file: FileHit) {
  if (!attachments.value.some(entry => entry.path === file.path)) {
    attachments.value = [...attachments.value, file]
  }
  insertComposerToken(`@${file.path} `)
  closeComposerMenu()
}

function pickMenuActive() {
  if (composerMenu.value === 'file') {
    const file = menuFiles.value[menuActive.value]
    if (file) pickMenuFile(file)
    return
  }
  const command = menuCommands.value[menuActive.value]
  if (command) pickMenuCommand(command)
}

function onMenuKeydown(event: KeyboardEvent) {
  if (event.isComposing) return
  const total = menuCount.value
  if (event.key === 'ArrowDown' && total) {
    event.preventDefault()
    menuActive.value = (menuActive.value + 1) % total
    scrollMenuActive()
    return
  }
  if (event.key === 'ArrowUp' && total) {
    event.preventDefault()
    menuActive.value = (menuActive.value - 1 + total) % total
    scrollMenuActive()
    return
  }
  if (event.key === 'Enter') {
    event.preventDefault()
    pickMenuActive()
    return
  }
  if (event.key === 'Escape') {
    event.preventDefault()
    closeComposerMenu(true)
    return
  }
  if (event.key === 'Tab') closeComposerMenu()
}

function onDocPointerDown(event: PointerEvent) {
  if (!composerMenu.value) return
  const target = event.target as Node | null
  if (!target) return
  if (
    commandBtn.value?.contains(target) ||
    fileBtn.value?.contains(target) ||
    composerMenuEl.value?.contains(target)
  ) {
    return
  }
  closeComposerMenu()
}

watch(menuQuery, value => {
  menuActive.value = 0
  if (composerMenu.value === 'file') scheduleFileSearch(value)
})

onMounted(() => {
  window.addEventListener('pointerdown', onDocPointerDown, true)
})

onBeforeUnmount(() => {
  window.removeEventListener('pointerdown', onDocPointerDown, true)
})

function onComposerKeydown(event: KeyboardEvent) {
  if (event.isComposing) return
  if (popup.value?.items.length) {
    if (event.key === 'ArrowDown') {
      event.preventDefault()
      popup.value.active = (popup.value.active + 1) % popup.value.items.length
      return
    }
    if (event.key === 'ArrowUp') {
      event.preventDefault()
      popup.value.active =
        (popup.value.active - 1 + popup.value.items.length) % popup.value.items.length
      return
    }
    if (event.key === 'Enter' || event.key === 'Tab') {
      event.preventDefault()
      void pickPopup(popup.value.active)
      return
    }
    if (event.key === 'Escape') {
      event.preventDefault()
      closePopup()
      return
    }
  }
  if (event.key === 'Enter' && !event.shiftKey) {
    event.preventDefault()
    void send()
  }
}

/** 解析发送时的模型参数（目录缺位时按 key 形态回退解析 provider::id）。
 *  只带用户显式选择的档位：未选则交给引擎默认（尊重 config.toml）。 */
function resolveModel(): { model: string; provider: string | null; variant: string | null } | null {
  if (!modelKey.value) return null
  const model = selectedModel.value
  const chosen = variant.value || null
  if (model) {
    return { model: model.id, provider: model.provider, variant: chosen }
  }
  const parts = modelKey.value.split('::')
  if (parts.length === 2) {
    return { model: parts[1], provider: parts[0], variant: chosen }
  }
  return { model: modelKey.value, provider: null, variant: chosen }
}

/** 标题编辑与标签选择器状态。 */
const titleEditing = ref(false)
const titleDraft = ref('')
const titleInput = ref<HTMLInputElement | null>(null)
const tagPickerOpen = ref(false)
/** 分支切换成功：轻提示 + 让外层刷新项目列表里的分支徽标。 */
function onBranchChanged(branch: string) {
  notice.value = `已切换到分支 ${branch}`
  emit('branchChanged', branch)
}

const starred = computed(() => (props.session ? isStarred(props.session.id) : false))
const sessionTags = computed(() => (props.session ? tagsOf(props.session.id) : []))

function startTitleEdit() {
  if (!props.session) return
  titleDraft.value = titleOf(props.session.id, props.session.title)
  titleEditing.value = true
  void nextTick(() => titleInput.value?.select())
}

async function saveTitle() {
  if (!props.session || !titleEditing.value) return
  titleEditing.value = false
  const sessionId = props.session.id
  const next = titleDraft.value.trim()
  const current = annotationFor(sessionId)?.title ?? ''
  if (next === current) return
  try {
    await setTitle(sessionId, next)
  } catch (cause) {
    console.error('[prism] 标题保存失败', cause)
  }
}

async function toggleHeaderStar() {
  if (!props.session) return
  try {
    await setStar(props.session.id, !isStarred(props.session.id))
  } catch (cause) {
    console.error('[prism] 星标失败', cause)
  }
}

onMounted(() => {
  void ensureAnnotations()
})

const running = computed(() => live.value.running)
const waiting = computed(() => pendingPermissions.value.length)
const showLive = computed(
  () => live.value.running || live.value.parts.length > 0 || !!live.value.error,
)
/** 每轮用时＝与上一条记录的时间差（没有时间戳的记录不给）。 */
const durations = computed(() => turnDurations(records.value))
/** live 轮次脚下那行的模型：优先当前选中的（这一轮就是用它跑的），退回会话记录里的。 */
const liveModel = computed(() => selectedModel.value?.id ?? props.session?.model ?? null)
/** 已接入运行轴的引擎（其余引擎只能浏览）。与新建会话的可用名单同源，避免两处漂移。 */
const RUNTIME_ENGINES = new Set<string>(CREATABLE_ENGINE_IDS)
const readOnly = computed(() => !!props.session && !RUNTIME_ENGINES.has(props.session.engine))

// 运行 / 待审批状态上报：工作台列头与全局审批角标用。
watch(
  [running, waiting],
  () => emit('status', { running: running.value, waiting: waiting.value }),
  { immediate: true },
)

const TERMINAL_EVENTS = new Set([
  'session.execution.succeeded',
  'session.execution.failed',
  'session.execution.interrupted',
])

let settleTimer: number | null = null

function report(cause: unknown) {
  emit('error', cause instanceof Error ? cause.message : String(cause))
}

function sleep(ms: number) {
  return new Promise(resolve => window.setTimeout(resolve, ms))
}

/** 会话标题（通知展示用；缺省回退短 id）。 */
function sessionLabel(sessionId: string): string {
  return props.session?.title?.trim() || sessionId.slice(0, 8)
}

function truncateText(text: string, limit: number): string {
  const oneLine = text.replace(/\s+/g, ' ').trim()
  return oneLine.length > limit ? `${oneLine.slice(0, limit)}…` : oneLine
}

/**
 * 回合终止 → 通知中心（M11）。
 * 打断是用户主动动作，不打扰；完成 / 失败各留一条持久条目。
 */
function notifyTerminal(eventType: string, turn: LiveTurn, sessionId: string): void {
  const session = props.session
  if (!session || session.id !== sessionId) return
  const label = sessionLabel(sessionId)
  const base = { sessionId, projectId: session.projectId }
  if (eventType === 'session.execution.succeeded') {
    pushNotification({
      key: `${sessionId}:done`,
      kind: 'done',
      title: `「${label}」回合完成`,
      body: engineLabel(session.engine),
      ...base,
    })
  } else if (eventType === 'session.execution.failed') {
    pushNotification({
      key: `${sessionId}:failed`,
      kind: 'failed',
      title: `「${label}」回合失败`,
      body: truncateText(turn.error ?? '执行失败', 140),
      ...base,
    })
  }
}

const stopEvents = onEngineEvent(event => {
  const sessionId = typeof event.data?.sessionID === 'string' ? event.data.sessionID : null
  const current = props.session?.id ?? null
  if (!sessionId || !current || sessionId !== current) return

  // 中断：底部给一句。停止按钮自己会立刻提示，这里兜「从别处中断」的情况。
  if (event.type === 'session.execution.interrupted') {
    flashNotice('已中断')
    recallQueued()
  }
  if (event.type === 'session.execution.failed') {
    recallQueued()
  }

  // 插话：入队确认 → 提示一句；真正落库（下一步送达）→ 拉一次时间线。
  if (event.type === 'session.input.queued') {
    flashNotice('已插话 · 下一步送达')
    return
  }
  if (event.type === 'session.input.delivered') {
    void refreshTail()
    return
  }
  if (event.type === 'session.input.dropped') {
    // 停止 / 失败时还没送出去：原样退回输入框，别让它凭空消失。
    const texts = Array.isArray(event.data?.texts) ? (event.data.texts as string[]) : []
    const back = texts.join('\n')
    if (back) {
      draft.value = draft.value ? `${back}\n${draft.value}` : back
      flashNotice('排队消息已退回输入框')
    }
    return
  }

  // 权限审批：进入 / 退出挂起列表（不参与 live 归约）。
  if (event.type === 'permission.asked') {
    const request = permissionFromEvent(event.data)
    if (request) {
      pendingPermissions.value = mergePermissions(pendingPermissions.value, [request])
      pushNotification({
        key: `${sessionId}:perm:${request.id}`,
        kind: 'permission',
        title: `「${sessionLabel(sessionId)}」等待审批`,
        body: permissionHeadline(request),
        sessionId,
        projectId: props.session?.projectId ?? null,
        // 引擎重连可能重放同一请求：按请求 id 永久去重。
        dedupe: 'key',
      })
    }
    return
  }
  if (event.type === 'permission.replied' || event.type === 'permission.rejected') {
    const requestId = repliedRequestId(event.data)
    if (requestId) {
      pendingPermissions.value = removePermission(pendingPermissions.value, requestId)
    }
    return
  }

  const before = live.value
  const after = reduceLiveTurn(before, event)
  if (after !== before) {
    live.value = after
    if (event.type === 'session.execution.started') {
      liveStartedAt.value = Date.now()
    }
    if (after.running) maybeStick()
  }
  if (TERMINAL_EVENTS.has(event.type)) {
    notifyTerminal(event.type, after, sessionId)
    scheduleSettle(sessionId)
  }
})

onBeforeUnmount(() => {
  stopEvents()
  if (settleTimer !== null) window.clearTimeout(settleTimer)
  document.removeEventListener('pointerdown', onCtxOutside, true)
  document.removeEventListener('keydown', onCtxEscape)
})

function scheduleSettle(sessionId: string) {
  if (settleTimer !== null) window.clearTimeout(settleTimer)
  settleTimer = window.setTimeout(() => {
    settleTimer = null
    void settleTurn(sessionId)
  }, 500)
}

/**
 * turn 收口：等 DB 落定本轮结果，先收掉 live 块、再把记录并进时间线。
 * DB 写入可能略滞后于 execution.* 事件，必要时重试刷新兜底。
 */
async function settleTurn(sessionId: string) {
  if (props.session?.id !== sessionId) return
  const coverage = liveCoverage(live.value, liveStartedAt.value)
  let page = await readTailOnce(sessionId)
  if (props.session?.id !== sessionId) return
  if (!page || !hasTurnResult(page, coverage)) {
    await sleep(800)
    if (props.session?.id !== sessionId) return
    page = await readTailOnce(sessionId)
  }
  if (props.session?.id !== sessionId) return
  if (!page || !hasTurnResult(page, coverage)) {
    await sleep(1200)
    if (props.session?.id !== sessionId) return
    page = await readTailOnce(sessionId)
  }
  if (props.session?.id !== sessionId) return
  // 顺序要紧：先收 live 再并记录。反过来的话，同一轮会 live 一份、记录一份。
  if (!live.value.running) {
    live.value = createLiveTurn()
    liveStartedAt.value = null
  }
  if (page) mergeTail(page)
  emit('settled')
}

/** DB 最新一页里是否已有本轮结果（收口看 DB，不看「本次刷新新增了什么」）。 */
function hasTurnResult(page: TimelinePage, coverage: LiveCoverage): boolean {
  return page.records.some(record => coveredByLive(record, coverage))
}

// ---- 时间线分页 ----

/**
 * 滚到底。
 *
 * 只滚一次不够：代码块、图片、KaTeX 是分拍渲染的，第一帧量到的 `scrollHeight`
 * 比最终矮，于是停在半路——进入会话时最明显（看到的是中间某条消息）。
 * 这里连续跟几帧，直到高度不再变化；有更新的贴底请求就让它接管。
 */
let pinToken = 0

function scrollToBottom() {
  const token = (pinToken += 1)
  let lastHeight = -1
  let frames = 0
  const tick = () => {
    if (token !== pinToken) return
    const el = scroller.value
    if (!el) return
    // 用户自己往上滚了就别再把他拽回来
    if (!stick.value) return
    el.scrollTop = el.scrollHeight
    const height = el.scrollHeight
    frames += 1
    if (height !== lastHeight && frames < 24) {
      lastHeight = height
      requestAnimationFrame(tick)
    }
  }
  void nextTick(() => requestAnimationFrame(tick))
}

function maybeStick() {
  if (stick.value) scrollToBottom()
}

/** 读最新一页；失败则上报并返回 null。 */
async function readTailOnce(sessionId: string): Promise<TimelinePage | null> {
  try {
    const page = await api.readTimeline(sessionId)
    if (props.session?.id !== sessionId) return null
    return page
  } catch (cause) {
    report(cause)
    return null
  }
}

/** 把一页里还没出现过的记录并进时间线（live 块已在展示的不算），返回新增。 */
function mergeTail(page: TimelinePage): TimelineRecord[] {
  const known = new Set(records.value.map(record => record.id))
  const coverage = liveCoverage(live.value, liveStartedAt.value)
  const additions = page.records.filter(
    record => !known.has(record.id) && !coveredByLive(record, coverage),
  )
  if (additions.length) {
    records.value = [...records.value, ...additions]
  }
  return additions
}

/** 合并 DB 最新一页里尚未出现过的记录（重连/收口时用）。 */
async function refreshTail(): Promise<TimelineRecord[]> {
  const sessionId = props.session?.id
  if (!sessionId) return []
  const page = await readTailOnce(sessionId)
  return page ? mergeTail(page) : []
}

async function loadInitial(sessionId: string) {
  records.value = []
  cursor.value = null
  hasMore.value = false
  pendingPermissions.value = []
  loading.value = true
  try {
    const page = await api.readTimeline(sessionId)
    if (props.session?.id !== sessionId) return
    records.value = page.records
    hydrateModelPick(page.records)
    cursor.value = page.nextCursor
    hasMore.value = page.hasMore
    scrollToBottom()
    // 补拉挂起的权限请求（引擎可能正在等待审批）。
    try {
      const pending = await api.sessionPermissions(sessionId)
      if (props.session?.id === sessionId) pendingPermissions.value = pending
    } catch {
      // 权限补拉失败不阻塞时间线。
    }
  } catch (cause) {
    report(cause)
  } finally {
    loading.value = false
  }
  // loading 期间只渲染「加载中」，内容这一刻才真正挂上——再贴一次底。
  if (props.session?.id === sessionId) {
    stick.value = true
    scrollToBottom()
  }
}

async function loadOlder() {
  const sessionId = props.session?.id
  if (
    !sessionId ||
    !hasMore.value ||
    cursor.value === null ||
    loadingOlder.value ||
    loading.value
  ) {
    return
  }

  const el = scroller.value
  const previousHeight = el?.scrollHeight ?? 0
  const previousTop = el?.scrollTop ?? 0

  loadingOlder.value = true
  try {
    const page = await api.readTimeline(sessionId, cursor.value)
    if (props.session?.id !== sessionId) return
    records.value = [...page.records, ...records.value]
    cursor.value = page.nextCursor
    hasMore.value = page.hasMore
    // 保持视口锚定：新增内容在顶部，回补滚动偏移。
    await nextTick()
    if (el) el.scrollTop = el.scrollHeight - (previousHeight - previousTop)
  } catch (cause) {
    report(cause)
  } finally {
    loadingOlder.value = false
  }
}

/** 搜索定位：确保目标消息已加载（必要时连续翻页），滚动 + 短暂高亮。 */
async function performJump(messageId: string) {
  const sessionId = props.session?.id
  if (!sessionId) {
    emit('jumpConsumed')
    return
  }
  let guard = 0
  while (
    !records.value.some(record => record.id === messageId) &&
    hasMore.value &&
    guard < 60
  ) {
    await loadOlder()
    guard += 1
    if (props.session?.id !== sessionId) break
  }
  const found = records.value.some(record => record.id === messageId)
  emit('jumpConsumed')
  if (!found || props.session?.id !== sessionId) return

  stick.value = false
  highlightId.value = messageId
  await nextTick()
  const el = scroller.value?.querySelector(`[data-message-id="${messageId}"]`)
  el?.scrollIntoView({ behavior: 'smooth', block: 'center' })
  window.setTimeout(() => {
    if (highlightId.value === messageId) highlightId.value = null
  }, 3600)
}

function onScroll() {
  const el = scroller.value
  if (!el) return
  stick.value = el.scrollHeight - el.scrollTop - el.clientHeight < 80
  if (stick.value) unseen.value = 0
  if (loading.value || loadingOlder.value || !hasMore.value) return
  if (el.scrollTop < 120) void loadOlder()
}

function backToBottom() {
  unseen.value = 0
  stick.value = true
  scrollToBottom()
}

// ---- 交付物（就地预览） ----

/** 记录增长且未吸附底部时：累计角标。 */
watch(
  () => records.value.length,
  (next, prev) => {
    if (!stick.value && next > prev) unseen.value += next - prev
  },
)

function resolveArtifactPath(raw: string): string {
  if (raw.startsWith('/')) return raw
  const base = props.session?.directory
  if (!base) return raw
  return `${base.replace(/\/+$/, '')}/${raw.replace(/^\.\//, '')}`
}

/**
 * 代码块右上角的「复制」（v-html 里的内容没法挂 Vue 事件，统一在这里代理）。
 * 点完把按钮改成「已复制」一会儿——不给反馈，用户会以为没点上。
 */
const copyTimers = new WeakMap<HTMLElement, number>()

async function copyCodeBlock(button: HTMLElement, text: string) {
  const ok = await copyPlainText(text)
  const previous = copyTimers.get(button)
  if (previous !== undefined) window.clearTimeout(previous)
  // 失败也要说话：静默的按钮最让人以为「点了没反应」。
  button.textContent = ok ? '已复制' : '复制失败'
  button.classList.toggle('done', ok)
  button.classList.toggle('failed', !ok)
  copyTimers.set(
    button,
    window.setTimeout(() => {
      button.textContent = '复制'
      button.classList.remove('done', 'failed')
      copyTimers.delete(button)
    }, 1400),
  )
}

/** 聊天里点到文件路径：相对路径按会话目录补全，然后开侧栏定位。 */
function openFileLink(raw: string) {
  const path = raw.trim()
  if (!path) return
  const directory = props.session?.directory ?? ''
  openPane({ kind: 'files', path: path.startsWith('/') ? path : joinPath(directory, path) })
}

// ---- 面板：标签页（文件 / 终端） ----

/** 点会话头的「面板」：开着就收起，关着就打开（落在文件标签）。 */
function togglePane() {
  if (changesOpen.value) {
    changesOpen.value = false
    return
  }
  openPane({ kind: 'files' })
}

/** 任务坞「在终端打开」：切到终端标签并选中那条任务。 */
function openTerminalTab(taskId: string) {
  openPane({ kind: 'term', taskId })
}

/** 任务坞「在终端里看全部」。 */
function openTerminalList() {
  openPane({ kind: 'term' })
}

/** 任务坞「＋ 新任务」：终端标签直接落在新建表单。 */
function openNewTerminal() {
  openPane({ kind: 'term', newTask: true })
}

function closeChangesPane() {
  changesOpen.value = false
}

function onTimelineClick(event: MouseEvent) {
  const target = event.target
  if (!(target instanceof HTMLElement)) return

  const copyBtn = target.closest('.md-code-copy')
  if (copyBtn instanceof HTMLElement) {
    const code = copyBtn.closest('.md-code')?.querySelector('pre code')
    if (code) void copyCodeBlock(copyBtn, code.textContent ?? '')
    return
  }

  const fileLink = target.closest('.md-file')
  if (fileLink instanceof HTMLElement) {
    event.preventDefault()
    openFileLink(fileLink.dataset.path ?? '')
    return
  }

  const card = target.closest('.md-artifact')
  if (!(card instanceof HTMLElement)) return
  event.preventDefault()
  const raw = card.dataset.artifactPath
  if (raw) artifactPath.value = resolveArtifactPath(raw)
}

/** 图片类交付物卡片：自动补缩略图（只读读取；超过 4 MiB 不缩略）。 */
const thumbCache = new Map<string, string | null>()
let thumbsInFlight = 0

async function loadThumb(card: HTMLElement, path: string) {
  let url = thumbCache.get(path)
  if (url === undefined) {
    try {
      const data = await api.readArtifact(path)
      url =
        data.kind === 'image' && data.base64 && data.size <= 4 * 1024 * 1024
          ? `data:${data.mime};base64,${data.base64}`
          : null
    } catch {
      url = null
    }
    if (thumbCache.size > 300) thumbCache.clear()
    thumbCache.set(path, url)
  }
  if (!url) return
  const image = document.createElement('img')
  image.className = 'md-artifact-thumb'
  image.src = url
  image.alt = ''
  card.insertBefore(image, card.firstChild)
}

async function enhanceArtifacts() {
  const root = scroller.value
  if (!root) return
  const cards = root.querySelectorAll<HTMLElement>(
    '.md-artifact[data-artifact-kind="image"]:not([data-thumb-done])',
  )
  for (const card of Array.from(cards)) {
    if (thumbsInFlight >= 4) break
    const raw = card.dataset.artifactPath
    if (!raw) continue
    card.dataset.thumbDone = '1'
    thumbsInFlight += 1
    void loadThumb(card, resolveArtifactPath(raw)).finally(() => {
      thumbsInFlight -= 1
      // 缩略图插进来会把内容顶高：还贴着底就再跟一次，别停在图上面。
      maybeStick()
    })
  }
}

watch(
  () => records.value.length,
  () => {
    void nextTick(enhanceArtifacts)
  },
)

// ---- 输入与运行 ----

async function send(force = false) {
  if (readOnly.value) return
  const sessionId = props.session?.id
  if (!sessionId || sending.value) return

  // 上一轮还在跑：先停在输入框上面的「待发送」，别直接打断。
  if (running.value && !force && shellCommand(draft.value) === null) {
    const pending = draft.value.trim()
    if (!pending && !attachments.value.length) return
    queued.value = pending
    draft.value = ''
    attachments.value = []
    closePopup()
    flashNotice('已排队 · 这一轮结束自动发出，或点「立即发送」插话')
    return
  }

  // ! 开头：本机 shell 直跑（不发给模型）
  const shell = shellCommand(draft.value)
  if (shell !== null) {
    if (!shell) return
    sending.value = true
    try {
      await api.sessionShell(sessionId, shell)
      if (props.session?.id !== sessionId) return
      draft.value = ''
      closePopup()
      flashNotice('已在本机执行 shell，输出见时间线')
      window.setTimeout(() => {
        if (props.session?.id === sessionId) void refreshTail()
      }, 900)
    } catch (cause) {
      report(cause)
    } finally {
      sending.value = false
    }
    return
  }

  const content = draft.value.trim()
  if (!content && !attachments.value.length) return
  // 先清空再投递：一次往返期间用户可能已经在写下一句，等回来再清会把它抹掉。
  const sentDraft = draft.value
  const sentFiles = attachments.value
  draft.value = ''
  attachments.value = []
  closePopup()
  sending.value = true
  try {
    const directory = (props.session?.directory ?? '').replace(/\/+$/, '')
    const files = attachments.value.map(file => ({
      uri: directory ? `${directory}/${file.path}` : file.path,
      name: fileLabel(file.path),
    }))
    const invocation = commandInvocation(content, catalog.value?.commands ?? [])
    if (invocation) {
      // 斜杠命令：走引擎命令通道（剩余文本作为命令参数）
      await api.sessionCommand(sessionId, invocation.name, invocation.text)
    } else {
      const options: SendOptions = { files }
      if (nativeSession.value) options.approval = approvalMode.value
      const picked = resolveModel()
      if (picked) {
        options.model = picked.model
        options.provider = picked.provider
        if (picked.variant && picked.variant !== 'default') options.variant = picked.variant
      }
      // 执行中发送 = 插话：引擎会把消息投递进当前执行（下一步边界送达）。
      await api.sessionSend(sessionId, content, options)
    }
    if (props.session?.id !== sessionId) return
    await refreshTail()
    stick.value = true
    scrollToBottom()
    // 用户消息落库可能略滞后：稍后再兜一次底。
    window.setTimeout(() => {
      if (props.session?.id === sessionId) void refreshTail()
    }, 600)
  } catch (cause) {
    // 没发出去：放回输入框（用户没接着打字的前提下），别让内容凭空消失。
    if (!draft.value) draft.value = sentDraft
    if (!attachments.value.length) attachments.value = sentFiles
    report(cause)
  } finally {
    sending.value = false
  }
}

/** 「立即发送」：把排队内容现在就投进去（引擎会在下一步边界接上）。 */
async function sendQueued() {
  const content = queued.value
  if (!content || sending.value) return
  queued.value = ''
  draft.value = content
  await send(true)
  if (draft.value === content) draft.value = ''
  stick.value = true
  scrollToBottom()
}

/**
 * 回合被打断（或失败）：排队那条不要顺势发出去——用户刚点了停止，
 * 再替他发一条等于没停。放回输入框，发不发由他决定。
 */
function recallQueued() {
  if (!queued.value) return
  draft.value = draft.value ? `${queued.value}\n${draft.value}` : queued.value
  queued.value = ''
}

/** 「修改」：把排队内容搬回输入框接着编辑。 */
function editQueued() {
  if (!queued.value) return
  draft.value = queued.value
  queued.value = ''
  void nextTick(() => composerInput.value?.focus())
}

/** 回合结束（running 由 true 变 false）：排队的那条自动发出去。 */
watch(running, (now, before) => {
  if (!before || now || !queued.value) return
  // 后端释放回合槽位有一拍延迟，稍等一下再发，不然会被判成"上一轮还没结束"。
  window.setTimeout(() => {
    if (queued.value && !running.value && !sending.value) void sendQueued()
  }, 400)
})

/**
 * 外面塞一条消息进来并发出去（工作台「新建会话」草稿的首条消息）。
 * 走的是和输入框点发送完全一样的路径：流式输出、审批、收口都照常。
 */
async function sendText(text: string) {
  const value = text.trim()
  if (!value || sending.value) return
  draft.value = value
  await send()
}

defineExpose({ sendText })

/** 一次性提示（4 秒后自动消失）。 */
function flashNotice(text: string) {
  notice.value = text
  window.setTimeout(() => {
    if (notice.value === text) notice.value = null
  }, 4000)
}

/** 回复权限请求：once / always / reject。 */
async function replyPermission(request: PermissionRequest, decision: PermissionDecision) {
  const sessionId = props.session?.id
  if (!sessionId || replyingPermission.value) return
  replyingPermission.value = request.id
  try {
    await api.sessionPermissionReply(sessionId, request.id, decision)
    if (props.session?.id !== sessionId) return
    pendingPermissions.value = removePermission(pendingPermissions.value, request.id)
  } catch (cause) {
    report(cause)
  } finally {
    replyingPermission.value = null
  }
}

async function stop() {
  const sessionId = props.session?.id
  if (!sessionId || stopping.value) return
  stopping.value = true
  try {
    await api.sessionInterrupt(sessionId)
    // 引擎的 interrupted 事件可能晚一拍到，先给一句，别让人以为按钮没反应。
    flashNotice('已中断')
  } catch (cause) {
    report(cause)
  } finally {
    stopping.value = false
  }
}

/** 输入框随内容长高（约 8 行封顶）。 */
function autoGrow() {
  const el = composerInput.value
  if (!el) return
  el.style.height = 'auto'
  el.style.height = `${Math.min(el.scrollHeight, 172)}px`
}

watch(draft, () => {
  void nextTick(autoGrow)
})

watch(
  () => props.session?.id,
  sessionId => {
    if (settleTimer !== null) {
      window.clearTimeout(settleTimer)
      settleTimer = null
    }
    live.value = createLiveTurn()
    liveStartedAt.value = null
    draft.value = ''
    stopping.value = false
    pendingPermissions.value = []
    replyingPermission.value = null
    stick.value = true
    tagPickerOpen.value = false
    titleEditing.value = false
    // 组合器：换会话时清空附件 / 目录缓存，装载该引擎的上次偏好
    catalog.value = null
    attachments.value = []
    popup.value = null
    notice.value = null
    closeComposerMenu()
    menuFiles.value = []
    if (sessionId) {
      const prefs = readPrefs(props.session?.engine ?? 'opencode')
      modelKey.value = prefs.modelKey
      variant.value = prefs.variant
      void prepareSession(sessionId)
    } else {
      modelKey.value = ''
      variant.value = ''
      records.value = []
      cursor.value = null
      hasMore.value = false
    }
  },
  { immediate: true },
)

/** 会话切换：装载初始页；若带搜索定位目标，装载完成后直接跳转。 */
async function prepareSession(sessionId: string) {
  await loadInitial(sessionId)
  const target = props.jumpMessageId
  if (target && props.session?.id === sessionId) {
    await performJump(target)
  }
}

// 同一会话内点击搜索结果：数据已在内存，直接定位。
watch(
  () => props.jumpMessageId,
  messageId => {
    if (!messageId || loading.value) return
    if (records.value.length || !hasMore.value) void performJump(messageId)
  },
)
</script>

<template>
  <main class="session-view" :class="{ embedded }" @click="tagPickerOpen = false">
    <template v-if="!session">
      <div class="placeholder">
        <div class="placeholder-mark">◆</div>
        <div v-if="!hasProject">从左侧选择一个项目</div>
        <div v-else>选择会话查看时间线</div>
      </div>
    </template>

    <template v-else>
      <header class="session-header" :class="{ embedded }">
        <div class="session-heading">
          <input
            v-if="titleEditing"
            ref="titleInput"
            v-model="titleDraft"
            class="title-input"
            spellcheck="false"
            @keydown.enter="saveTitle"
            @keydown.esc="titleEditing = false"
            @blur="saveTitle"
          />
          <h1
            v-else
            class="session-title"
            title="双击编辑标题"
            @dblclick="startTitleEdit"
          >
            {{ titleOf(session.id, session.title) }}
          </h1>
          <div class="head-actions">
            <div class="icon-group">
              <button
                class="icon-btn"
                :class="{ 'on-star': starred }"
                :aria-pressed="starred"
                :title="starred ? '取消星标' : '加星标'"
                :aria-label="starred ? '取消星标' : '加星标'"
                @click="toggleHeaderStar"
              >
                <svg
                  viewBox="0 0 24 24"
                  :fill="starred ? 'currentColor' : 'none'"
                  stroke="currentColor"
                  stroke-width="1.7"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                  aria-hidden="true"
                >
                  <path d="M12 3.2l2.6 5.4 5.9.85-4.3 4.2 1.02 5.9L12 16.75 6.78 19.55 7.8 13.65 3.5 9.45l5.9-.85L12 3.2Z" />
                </svg>
              </button>
              <button
                v-if="!titleEditing"
                class="icon-btn"
                title="编辑标题"
                aria-label="编辑标题"
                @click="startTitleEdit"
              >
                <svg
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.7"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                  aria-hidden="true"
                >
                  <path d="M4 20h4l10.5-10.5a2.1 2.1 0 0 0-3-3L5 17v3Z" />
                  <path d="M13.5 6.5 17 10" />
                </svg>
              </button>
            </div>
            <button
              class="changes-btn"
              :class="{ on: changesOpen }"
              title="面板：文件改动 / 全部文件 / 后台任务，可开多个标签页"
              @click.stop="togglePane"
            >
              <svg
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="1.7"
                stroke-linecap="round"
                stroke-linejoin="round"
                aria-hidden="true"
              >
                <rect x="3.4" y="4.6" width="17.2" height="14.8" rx="2.2" />
                <path d="M14.6 4.6v14.8M17.6 9.2h.01M17.6 12h.01" />
              </svg>
              面板
            </button>
            <button
              v-if="!embedded"
              class="open-wb-btn"
              title="在工作台打开：多列并行盯着多个会话"
              @click="emit('openWorkbench', session.id)"
            >
              <svg
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="1.7"
                stroke-linecap="round"
                stroke-linejoin="round"
                aria-hidden="true"
              >
                <rect x="3" y="4.5" width="18" height="15" rx="2.2" />
                <path d="M9.5 4.5v15M15 4.5v15" />
              </svg>
              工作台
            </button>
          </div>
        </div>
        <div class="session-tags">
          <span v-for="tag in sessionTags" :key="tag.id" class="tag-chip">
            <span class="tag-dot" :style="{ background: tag.color }" />
            {{ tag.name }}
          </span>
          <button class="tag-add" @click.stop="tagPickerOpen = !tagPickerOpen">
            ＋ 标签
          </button>
          <TagPicker
            v-if="tagPickerOpen"
            :session-id="session.id"
            @close="tagPickerOpen = false"
          />
        </div>
        <div class="session-meta">
          <span class="meta-dir" :title="session.directory">{{ session.directory }}</span>
          <span v-if="session.model" class="meta-chip meta-model">{{ session.model }}</span>
          <span class="meta-chip meta-tokens">
            ↓{{ formatTokens(session.tokensInput + session.tokensReasoning) }}
            ↑{{ formatTokens(session.tokensOutput) }}
          </span>
          <span v-if="session.cost" class="meta-chip meta-cost">{{ formatCost(session.cost) }}</span>
          <span class="meta-time">{{ fullTime(session.timeCreated) }}</span>
        </div>
      </header>

      <div class="timeline-area">
        <div
          ref="scroller"
          class="timeline"
          @scroll.passive="onScroll"
          @click="onTimelineClick"
        >
        <div v-if="loading" class="timeline-hint">加载中…</div>
        <div v-else-if="!records.length && !showLive" class="timeline-hint">
          这个会话还没有消息，直接在下面发一条试试
        </div>

        <template v-else>
          <div class="timeline-top">
            <button
              v-if="hasMore"
              class="older-btn"
              :disabled="loadingOlder"
              @click="loadOlder"
            >
              {{ loadingOlder ? '加载中…' : '加载更早的消息' }}
            </button>
            <div v-else class="timeline-start">已是全部历史</div>
          </div>
          <div class="timeline-track">
            <TimelineItem
              v-for="record in records"
              :key="record.id"
              :record="record"
              :session-id="session.id"
              :highlighted="record.id === highlightId"
              :duration-ms="durations.get(record.id) ?? null"
              @open-file="openFileLink"
            />
            <LiveTurnView
              v-if="showLive"
              :turn="live"
              :model="liveModel"
              :running-since="liveStartedAt"
            />
          </div>
        </template>
        </div>
      </div>

      <button
        v-if="!stick && records.length"
        class="to-bottom"
        :title="unseen ? `${unseen} 条新消息，回到底部` : '回到底部'"
        @click="backToBottom"
      >
        ↓<span v-if="unseen" class="to-bottom-badge">{{ unseen > 99 ? '99+' : unseen }}</span>
      </button>

      <footer class="composer">
        <BackgroundTasks
          v-if="showTasks !== false"
          :session-id="session.id"
          @error="report"
          @open-terminal="openTerminalTab"
          @open-terminal-list="openTerminalList"
          @new-terminal="openNewTerminal"
        />
        <div v-if="readOnly" class="readonly-bar">
          <span aria-hidden="true">🔒</span>
          <span>
            该引擎暂为只读浏览（运行轴尚在开发）——发消息请到对应应用；
            回到 Prism 点「刷新」即可看到新内容。
          </span>
          <BranchPicker
            class="readonly-branch"
            :directory="session.directory"
            @changed="onBranchChanged"
          />
        </div>
        <template v-else>
          <div v-if="pendingPermissions.length" class="permission-stack">
          <div
            v-for="item in pendingPermissions"
            :key="item.id"
            class="permission-card"
          >
            <span class="permission-lock" aria-hidden="true">🔐</span>
            <div class="permission-main">
              <div class="permission-headline">{{ permissionHeadline(item) }}</div>
              <div class="permission-paths">
                <span
                  v-for="(resource, index) in permissionResources(item)"
                  :key="index"
                  class="permission-path"
                  >{{ resource }}</span
                >
              </div>
            </div>
            <div class="permission-actions">
              <button
                class="permission-btn allow"
                :disabled="replyingPermission === item.id"
                @click="replyPermission(item, 'once')"
              >
                允许一次
              </button>
              <button
                class="permission-btn always"
                :disabled="replyingPermission === item.id"
                title="保存到项目权限，同类请求以后自动放行"
                @click="replyPermission(item, 'always')"
              >
                总是允许
              </button>
              <button
                class="permission-btn reject"
                :disabled="replyingPermission === item.id"
                @click="replyPermission(item, 'reject')"
              >
                拒绝
              </button>
            </div>
          </div>
        </div>
        <div
          ref="composerBox"
          class="composer-box"
          :class="{ 'is-focused': composerFocused, 'is-menu': !!composerMenu }"
        >
          <!-- 执行中按回车先停在这儿：要不要立刻送进去由这三个按钮决定 -->
          <div v-if="queued" class="queued-card">
            <span class="queued-tag">待发送</span>
            <p class="queued-text">{{ queued }}</p>
            <div class="queued-actions">
              <button
                class="queued-btn primary"
                title="立即发送：插进这一轮，下一步送达"
                aria-label="立即发送"
                @click="sendQueued()"
              >
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                  <path d="M12 19V5M5.5 11.5 12 5l6.5 6.5" />
                </svg>
              </button>
              <button class="queued-btn" title="修改" aria-label="修改" @click="editQueued()">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                  <path d="M4 20h4l9.6-9.6a2.8 2.8 0 0 0-4-4L4 16v4Z" />
                  <path d="m13.4 6.6 4 4" />
                </svg>
              </button>
              <button class="queued-btn" title="删除" aria-label="删除" @click="queued = ''">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
                  <path d="M6 6l12 12M18 6 6 18" />
                </svg>
              </button>
            </div>
          </div>

          <div class="composer-input-wrap">
            <textarea
              ref="composerInput"
              v-model="draft"
              class="composer-input"
              rows="3"
              spellcheck="false"
              :placeholder="
                running
                  ? '执行中：Enter 先排队，点「立即发送」立刻插进这一轮'
                  : '发一条指令…（Enter 发送，! 开头直接跑本机 shell）'
              "
              @input="onComposerInput"
              @keydown="onComposerKeydown"
              @focus="onComposerFocus"
              @blur="composerFocused = false"
            />
            <div v-if="popup && popup.items.length" class="composer-popup">
              <template v-if="popup.kind === 'file'">
                <button
                  v-for="(item, index) in popup.items"
                  :key="(item as FileHit).path"
                  class="popup-item"
                  :class="{ active: index === popup.active }"
                  @mousedown.prevent="pickPopup(index)"
                >
                  <span class="popup-main">{{ (item as FileHit).name }}</span>
                  <span class="popup-sub">{{ (item as FileHit).path }}</span>
                </button>
              </template>
              <template v-else>
                <button
                  v-for="(item, index) in popup.items"
                  :key="(item as EngineCommand).name"
                  class="popup-item"
                  :class="{ active: index === popup.active }"
                  @mousedown.prevent="pickPopup(index)"
                >
                  <span class="popup-main">/{{ (item as EngineCommand).name }}</span>
                  <span class="popup-sub">{{ (item as EngineCommand).description }}</span>
                </button>
              </template>
            </div>
          </div>

          <div v-if="attachments.length" class="composer-attachments">
            <span v-for="file in attachments" :key="file.path" class="attach-chip">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                <path d="M21.4 11.05 12.9 19.5a5 5 0 0 1-7.07-7.07l8.49-8.49a3.5 3.5 0 1 1 4.95 4.95l-8.49 8.49a2 2 0 0 1-2.83-2.83l7.78-7.78" />
              </svg>
              <span class="chip-label">{{ fileLabel(file.path) }}</span>
              <button class="chip-x" title="移除附件" @click="removeAttachment(file)">✕</button>
            </span>
          </div>

          <div class="composer-bar">
            <div class="composer-rail">
              <button
                ref="commandBtn"
                class="rail-btn"
                :class="{ on: composerMenu === 'command' }"
                :disabled="sending"
                :aria-expanded="composerMenu === 'command'"
                aria-haspopup="dialog"
                title="斜杠命令（/）"
                aria-label="插入斜杠命令"
                @click="toggleComposerMenu('command')"
              >
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" aria-hidden="true">
                  <path d="M12 5v14M5 12h14" />
                </svg>
              </button>
              <button
                ref="fileBtn"
                class="rail-btn"
                :class="{ on: composerMenu === 'file' }"
                :disabled="sending"
                :aria-expanded="composerMenu === 'file'"
                aria-haspopup="dialog"
                title="引用文件（@）"
                aria-label="引用项目文件"
                @click="toggleComposerMenu('file')"
              >
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                  <path d="M21.4 11.05 12.9 19.5a5 5 0 0 1-7.07-7.07l8.49-8.49a3.5 3.5 0 1 1 4.95 4.95l-8.49 8.49a2 2 0 0 1-2.83-2.83l7.78-7.78" />
                </svg>
                <span v-if="attachments.length && composerMenu !== 'file'" class="rail-dot" />
              </button>
            </div>

            <BranchPicker
              class="composer-branch"
              :directory="session.directory"
              @changed="onBranchChanged"
            />

            <PermissionPicker
              v-if="nativeSession"
              v-model="approvalMode"
              :disabled="sending"
            />

            <span class="composer-hint" :class="{ notice: !!notice || stopping }" role="status">
              {{ composerHint }}
            </span>

            <div class="composer-right">
              <!-- 还没有任何用量（新会话 / 引擎没给 usage）就不占位：
                   一个空环加「—」比不显示更像坏了。 -->
              <span v-if="contextUsage.used > 0" class="ctx-wrap">
                <ContextRing
                  :used="contextUsage.used"
                  :window="contextUsage.window"
                  :estimated="contextUsage.estimated"
                  @click="ctxOpen = !ctxOpen"
                />
                <ContextPanel
                  v-if="ctxOpen"
                  :used="contextUsage.used"
                  :window="contextUsage.window"
                  :slices="ctxSlices"
                  :estimated="!ctxStats"
                />
              </span>
              <ModelMenu
                class="model-pick"
                :model-value="modelKey"
                :variant="variant"
                :options="modelOptions"
                :variants="sessionVariants"
                :loading="catalogLoading"
                :disabled="sending"
                title="模型 / 推理强度（下一轮生效）"
                @update:model-value="setModel"
                @update:variant="setVariant"
                @open="ensureCatalog"
              />
              <button
                v-if="running"
                class="stop-btn"
                :disabled="stopping"
                title="停止当前回合"
                aria-label="停止当前回合"
                @click="stop"
              >
                <svg viewBox="0 0 24 24" fill="currentColor" stroke="none" aria-hidden="true">
                  <rect x="7" y="7" width="10" height="10" rx="2" />
                </svg>
              </button>
              <button
                class="send-btn"
                :class="{ 'shell-btn': shellMode }"
                :disabled="!canSend || sending"
                :title="
                  shellMode
                    ? '运行 shell（Enter）'
                    : running
                      ? '插话（Enter）'
                      : '发送（Enter）'
                "
                :aria-label="shellMode ? '运行 shell' : running ? '插话' : '发送'"
                @click="send()"
              >
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                  <path d="M12 19V5M5.5 11.5 12 5l6.5 6.5" />
                </svg>
              </button>
            </div>
          </div>

          <div
            v-if="composerMenu"
            ref="composerMenuEl"
            class="composer-menu"
            :class="`composer-menu-${composerMenu}`"
          >
            <div class="menu-head">
              <span class="menu-title">{{ composerMenu === 'command' ? '斜杠命令' : '引用文件' }}</span>
              <span v-if="session" class="menu-engine">{{ engineLabel(session.engine) }}</span>
              <button class="menu-close" title="关闭" aria-label="关闭菜单" @click="closeComposerMenu(true)">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
                  <path d="M6 6l12 12M18 6 6 18" />
                </svg>
              </button>
            </div>

            <label class="menu-search">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
                <circle cx="11" cy="11" r="7" />
                <path d="m20 20-3.5-3.5" />
              </svg>
              <input
                ref="menuSearch"
                v-model="menuQuery"
                type="text"
                spellcheck="false"
                :placeholder="composerMenu === 'command' ? '搜索命令…' : '搜索项目文件…'"
                @keydown="onMenuKeydown"
              />
            </label>

            <div class="menu-list">
              <template v-if="composerMenu === 'command'">
                <div v-if="catalogLoading" class="menu-empty">正在读取引擎命令…</div>
                <div v-else-if="!catalog" class="menu-empty">
                  读不到这个引擎的命令目录
                  <span class="menu-empty-sub">可以直接在输入框输入 / 触发补全</span>
                </div>
                <div v-else-if="!menuCommands.length" class="menu-empty">
                  {{ menuQuery.trim() ? `没有匹配「${menuQuery.trim()}」的命令` : `${engineLabel(session?.engine ?? '')} 没有可列举的斜杠命令` }}
                  <span v-if="!menuQuery.trim()" class="menu-empty-sub">
                    也可以直接在输入框输入 / 或 @ 触发补全
                  </span>
                </div>
                <template v-else>
                  <button
                    v-for="(command, index) in menuCommands"
                    :key="command.name"
                    class="menu-item"
                    :class="{ active: index === menuActive }"
                    @pointerenter="menuActive = index"
                    @click="pickMenuCommand(command)"
                  >
                    <span class="menu-item-main mono">/{{ command.name }}</span>
                    <span class="menu-item-sub">{{ command.description }}</span>
                  </button>
                </template>
              </template>

              <template v-else>
                <div v-if="menuFilesLoading" class="menu-empty">正在索引项目文件…</div>
                <div v-else-if="!session?.directory" class="menu-empty">这个会话没有可引用的工作目录</div>
                <div v-else-if="!menuFiles.length" class="menu-empty">
                  {{ menuQuery.trim() ? `没有匹配「${menuQuery.trim()}」的文件` : '这个目录里没有可引用的文件' }}
                </div>
                <template v-else>
                  <button
                    v-for="(file, index) in menuFiles"
                    :key="file.path"
                    class="menu-item"
                    :class="{ active: index === menuActive }"
                    @pointerenter="menuActive = index"
                    @click="pickMenuFile(file)"
                  >
                    <span class="menu-item-main">{{ file.name }}</span>
                    <span class="menu-item-sub mono">{{ file.path }}</span>
                  </button>
                </template>
              </template>
            </div>

            <div class="menu-foot">
              <span>↑↓ 选择 · Enter {{ composerMenu === 'command' ? '插入' : '引用' }} · Esc 关闭</span>
            </div>
          </div>
        </div>
        </template>
      </footer>
    </template>
    <ArtifactViewer :path="artifactPath" @close="artifactPath = null" />
    <ChangesPane
      v-if="changesOpen && session"
      :directory="session.directory"
      :session-id="session.id"
      :running="running"
      :intent="paneTarget"
      :intent-tick="paneTick"

      @close="closeChangesPane"
    />
  </main>
</template>

<style scoped src="../styles/components/SessionView.css"></style>
