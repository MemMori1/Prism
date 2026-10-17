<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { api } from '../api'
import { annotationFor, ensureAnnotations, isStarred, setStar, setTitle, tagsOf, titleOf } from '../annotations'
import {
  applyCompletion,
  commandInvocation,
  detectTrigger,
  fileLabel,
  modelKeyOf,
  modelLabel,
  shellCommand,
  type Trigger,
} from '../composer'
import { onEngineEvent } from '../engineEvents'
import { createLiveTurn, reduceLiveTurn, type LiveTurn } from '../liveTurn'
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
import type {
  EngineCatalog,
  EngineCommand,
  FileHit,
  PermissionRequest,
  SendOptions,
  SessionSummary,
  TimelineRecord,
} from '../types'
import { formatCost, formatTokens, fullTime } from '../format'
import TimelineItem from './TimelineItem.vue'
import LiveTurnView from './LiveTurn.vue'
import ArtifactViewer from './ArtifactViewer.vue'
import TagPicker from './TagPicker.vue'
import LedgerPane from './LedgerPane.vue'

const props = defineProps<{
  session: SessionSummary | null
  hasProject: boolean
  /** 工作台列内嵌模式：隐藏会话头（由列头承担标题 / 状态）。 */
  embedded?: boolean
  /** 搜索结果定位：滚动到该消息并短暂高亮。 */
  jumpMessageId?: string | null
}>()

const emit = defineEmits<{
  (e: 'error', message: string): void
  (e: 'settled'): void
  (e: 'jumpConsumed'): void
  (e: 'openWorkbench', sessionId: string): void
  (e: 'status', status: { running: boolean; waiting: number }): void
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

watch([modelKey, variant], () => {
  if (!props.session) return
  writePrefs(props.session.engine, { modelKey: modelKey.value, variant: variant.value })
})

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
/** 文件账本抽屉。 */
const ledgerOpen = ref(false)

function onLedgerJump(recordId: string) {
  void performJump(recordId)
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
/** 已接入运行轴的引擎（其余引擎只能浏览，后续引擎接入时在这里放行）。 */
const RUNTIME_ENGINES = new Set(['opencode', 'codex'])
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
})

function scheduleSettle(sessionId: string) {
  if (settleTimer !== null) window.clearTimeout(settleTimer)
  settleTimer = window.setTimeout(() => {
    settleTimer = null
    void settleTurn(sessionId)
  }, 500)
}

/**
 * turn 收口：以 DB 为准合并新增记录，然后清掉 live 块。
 * DB 写入可能略滞后于 execution.* 事件，必要时重试刷新兜底。
 */
async function settleTurn(sessionId: string) {
  if (props.session?.id !== sessionId) return
  let additions = await refreshTail()
  if (props.session?.id !== sessionId) return
  if (!coveredBy(additions)) {
    await sleep(800)
    if (props.session?.id !== sessionId) return
    additions = await refreshTail()
  }
  if (props.session?.id !== sessionId) return
  if (!coveredBy(additions)) {
    await sleep(1200)
    if (props.session?.id !== sessionId) return
    await refreshTail()
  }
  if (props.session?.id !== sessionId) return
  if (!live.value.running) {
    live.value = createLiveTurn()
    liveStartedAt.value = null
  }
  emit('settled')
}

/** 本次刷新新增的记录里是否已覆盖本轮结果。 */
function coveredBy(additions: TimelineRecord[]): boolean {
  const startedAt = liveStartedAt.value
  if (startedAt === null) {
    return additions.some(record => record.role === 'assistant')
  }
  return additions.some(
    record =>
      record.role === 'assistant' &&
      record.completed &&
      record.timeCreated >= startedAt - 3000,
  )
}

// ---- 时间线分页 ----

function scrollToBottom() {
  nextTick(() => {
    const el = scroller.value
    if (el) el.scrollTop = el.scrollHeight
  })
}

function maybeStick() {
  if (stick.value) scrollToBottom()
}

/** 合并 DB 最新一页里尚未出现过的记录（重连/收口时用），返回本次新增。 */
async function refreshTail(): Promise<TimelineRecord[]> {
  const sessionId = props.session?.id
  if (!sessionId) return []
  try {
    const page = await api.readTimeline(sessionId)
    if (props.session?.id !== sessionId) return []
    const known = new Set(records.value.map(record => record.id))
    // live 块正在展示的助手消息（插话后可能已完结落库，避免重复渲染）。
    const liveMessageIds = new Set(
      live.value.parts
        .map(part => part.messageId)
        .filter(messageId => messageId && messageId !== 'unknown'),
    )
    const additions = page.records.filter(record => {
      if (known.has(record.id)) return false
      // 执行中：引擎可能已在写未完结（或已由 live 展示）的助手消息。
      if (
        live.value.running &&
        record.role === 'assistant' &&
        (!record.completed || liveMessageIds.has(record.id))
      ) {
        return false
      }
      return true
    })
    if (additions.length) {
      records.value = [...records.value, ...additions]
    }
    return additions
  } catch (cause) {
    report(cause)
    return []
  }
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

function onTimelineClick(event: MouseEvent) {
  const target = event.target
  if (!(target instanceof HTMLElement)) return
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

async function send() {
  if (readOnly.value) return
  const sessionId = props.session?.id
  if (!sessionId || sending.value) return

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
    draft.value = ''
    attachments.value = []
    closePopup()
    await refreshTail()
    stick.value = true
    scrollToBottom()
    // 用户消息落库可能略滞后：稍后再兜一次底。
    window.setTimeout(() => {
      if (props.session?.id === sessionId) void refreshTail()
    }, 600)
  } catch (cause) {
    report(cause)
  } finally {
    sending.value = false
  }
}

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
    ledgerOpen.value = false
    // 组合器：换会话时清空附件 / 目录缓存，装载该引擎的上次偏好
    catalog.value = null
    attachments.value = []
    popup.value = null
    notice.value = null
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
      <header v-if="!embedded" class="session-header">
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
          <button
            class="icon-btn"
            :class="{ on: starred }"
            :title="starred ? '取消星标' : '加星标'"
            @click="toggleHeaderStar"
          >
            {{ starred ? '★' : '☆' }}
          </button>
          <button
            v-if="!titleEditing"
            class="icon-btn"
            title="编辑标题"
            @click="startTitleEdit"
          >
            ✎
          </button>
          <button
            class="icon-btn"
            :class="{ on: ledgerOpen }"
            title="文件账本：这个会话动过哪些文件"
            @click.stop="ledgerOpen = !ledgerOpen"
          >
            🗂
          </button>
          <button
            class="open-wb-btn"
            title="在工作台打开：多列并行盯着多个会话"
            @click="emit('openWorkbench', session.id)"
          >
            ⇥ 工作台
          </button>
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
          <span v-if="session.model" class="meta-chip">{{ session.model }}</span>
          <span class="meta-chip">
            ↓{{ formatTokens(session.tokensInput + session.tokensReasoning) }}
            ↑{{ formatTokens(session.tokensOutput) }}
          </span>
          <span v-if="session.cost" class="meta-chip">{{ formatCost(session.cost) }}</span>
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
            />
            <LiveTurnView v-if="showLive" :turn="live" />
          </div>
        </template>
        </div>

        <LedgerPane
          v-if="ledgerOpen && session"
          :session-id="session.id"
          :directory="session.directory"
          @jump="onLedgerJump"
          @close="ledgerOpen = false"
        />
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
        <div v-if="readOnly" class="readonly-bar">
          <span aria-hidden="true">🔒</span>
          <span>
            该引擎暂为只读浏览（运行轴尚在开发）——发消息请到对应应用；
            回到 Prism 点「刷新」即可看到新内容。
          </span>
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
        <div class="composer-box">
          <div class="composer-tools">
            <label class="tool-select" title="模型（下一轮生效）">
              <span class="tool-label">模型</span>
              <select
                v-model="modelKey"
                :disabled="sending"
                @focus="ensureCatalog"
                @mousedown="ensureCatalog"
              >
                <option value="">{{ catalogLoading ? '加载中…' : '默认' }}</option>
                <option
                  v-for="model in sessionModels"
                  :key="modelKeyOf(model)"
                  :value="modelKeyOf(model)"
                >
                  {{ modelLabel(model) }}
                </option>
              </select>
            </label>
            <label v-if="sessionVariants.length" class="tool-select" title="推理档位（下一轮生效）">
              <span class="tool-label">档位</span>
              <select v-model="variant" :disabled="sending">
                <option value="">默认</option>
                <option v-for="item in sessionVariants" :key="item" :value="item">{{ item }}</option>
              </select>
            </label>
            <span class="tool-tip" title="@ 选文件 · / 选命令 · ! 直跑 shell">@ 文件 · / 命令 · ! shell</span>
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
                  ? '执行中：发送 = 插话引导（下一步送达），Shift+Enter 换行'
                  : '发一条指令…（@ 文件、/ 命令、! shell；Enter 发送）'
              "
              :disabled="sending"
              @input="onComposerInput"
              @keydown="onComposerKeydown"
              @focus="ensureCatalog"
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
              @{{ fileLabel(file.path) }}
              <button class="chip-x" title="移除附件" @click="removeAttachment(file)">✕</button>
            </span>
          </div>
          <div class="composer-actions">
            <span v-if="sending" class="composer-hint">发送中…</span>
            <span v-else-if="notice" class="composer-hint">{{ notice }}</span>
            <span v-else-if="shellMode" class="composer-hint">
              ! 开头 = 本机直跑 shell（不发给模型）
            </span>
            <span v-else-if="running" class="composer-hint">执行中，发送即插话</span>
            <span v-else-if="commandPreview" class="composer-hint">
              斜杠命令 /{{ commandPreview.name }}
            </span>
            <button
              v-if="shellMode"
              class="send-btn shell-btn"
              :disabled="!draft.trim().slice(1).trim() || sending"
              @click="send"
            >
              运行 shell ⏎
            </button>
            <template v-else>
              <button
                v-if="running"
                class="send-btn"
                :disabled="(!draft.trim() && !attachments.length) || sending"
                @click="send"
              >
                插话 ⏎
              </button>
              <button
                v-if="running"
                class="stop-btn"
                :disabled="stopping"
                @click="stop"
              >
                {{ stopping ? '停止中…' : '■ 停止' }}
              </button>
              <button
                v-else
                class="send-btn"
                :disabled="(!draft.trim() && !attachments.length) || sending"
                @click="send"
              >
                发送 ⏎
              </button>
            </template>
          </div>
        </div>
        </template>
      </footer>
    </template>
    <ArtifactViewer :path="artifactPath" @close="artifactPath = null" />
  </main>
</template>

<style scoped>
.session-view {
  position: relative;
  display: flex;
  flex-direction: column;
  flex: 1;
  min-width: 0;
  min-height: 0;
  background: var(--bg);
}

.placeholder {
  margin: auto;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 10px;
  color: var(--faint);
  font-size: 13px;
}

.placeholder-mark {
  font-size: 26px;
  color: var(--border-strong);
}

.session-header {
  flex: none;
  padding: 14px 22px 12px;
  border-bottom: 1px solid var(--border);
  background: var(--panel);
}

.session-heading {
  display: flex;
  align-items: center;
  gap: 10px;
}

.session-title {
  margin: 0;
  flex: 1;
  min-width: 0;
  font-size: 15px;
  font-weight: 650;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.title-input {
  flex: 1;
  min-width: 0;
  height: 30px;
  padding: 0 8px;
  border: 1px solid color-mix(in srgb, var(--accent) 55%, transparent);
  border-radius: var(--radius-sm);
  background: var(--panel-2);
  color: var(--text);
  font-size: 14.5px;
  font-weight: 600;
}

.title-input:focus {
  outline: none;
}

.icon-btn {
  flex: none;
  width: 26px;
  height: 26px;
  display: flex;
  align-items: center;
  justify-content: center;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--panel);
  color: var(--faint);
  font-size: 13px;
  line-height: 1;
  cursor: pointer;
}

.icon-btn:hover {
  color: var(--text);
  border-color: var(--border-strong);
}

.icon-btn.on {
  color: var(--amber);
  border-color: color-mix(in srgb, var(--amber) 45%, var(--border));
  background: var(--amber-soft);
}

.session-tags {
  position: relative;
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 6px;
  margin-top: 8px;
}

.tag-chip {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  padding: 2px 9px;
  border: 1px solid var(--border);
  border-radius: 999px;
  background: var(--panel-2);
  color: var(--muted);
  font-size: 11px;
}

.tag-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
}

.tag-add {
  padding: 2px 9px;
  border: 1px dashed var(--border-strong);
  border-radius: 999px;
  background: transparent;
  color: var(--faint);
  font-size: 11px;
  cursor: pointer;
}

.tag-add:hover {
  color: var(--accent-text);
  border-color: color-mix(in srgb, var(--accent) 45%, var(--border));
}

.open-wb-btn {
  flex: none;
  padding: 3px 10px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--border);
  background: var(--panel-2);
  color: var(--muted);
  font-size: 11.5px;
  transition: all 0.15s ease;
}

.open-wb-btn:hover {
  color: var(--accent);
  border-color: var(--accent);
}

.session-meta {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 5px;
  font-size: 11px;
  color: var(--muted);
  overflow: hidden;
  white-space: nowrap;
}

.meta-dir {
  font-family: var(--font-mono);
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 40%;
}

.meta-chip {
  padding: 0 8px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: var(--panel-2);
  line-height: 17px;
  font-family: var(--font-mono);
  flex: none;
}

.meta-time {
  margin-left: auto;
  flex: none;
}

.timeline-area {
  position: relative;
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
}

.timeline {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: 14px 22px 40px;
}

.timeline-hint {
  text-align: center;
  color: var(--faint);
  font-size: 12px;
  padding-top: 40px;
}

.timeline-top {
  display: flex;
  justify-content: center;
  padding: 2px 0 12px;
}

.older-btn {
  padding: 4px 14px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: var(--panel);
  color: var(--muted);
  font-size: 11.5px;
  transition: all 0.15s ease;
}

.older-btn:hover:not(:disabled) {
  color: var(--text);
  border-color: var(--border-strong);
}

.older-btn:disabled {
  opacity: 0.6;
  cursor: default;
}

.timeline-start {
  color: var(--faint);
  font-size: 10.5px;
}

.timeline-track {
  display: flex;
  flex-direction: column;
  gap: 18px;
  max-width: 1000px;
  margin: 0 auto;
}

.composer {
  flex: none;
  border-top: 1px solid var(--border);
  background: var(--panel);
  padding: 10px 22px 14px;
}

.readonly-bar {
  display: flex;
  align-items: center;
  gap: 8px;
  max-width: 1000px;
  margin: 0 auto;
  padding: 9px 12px;
  border: 1px dashed var(--border-strong);
  border-radius: var(--radius-sm);
  background: var(--panel-2);
  color: var(--muted);
  font-size: 12px;
  line-height: 1.5;
}

.permission-stack {
  max-width: 1000px;
  margin: 0 auto 10px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.permission-card {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  padding: 10px 12px;
  border: 1px solid color-mix(in srgb, var(--amber) 45%, var(--border));
  border-radius: var(--radius-sm);
  background: var(--amber-soft);
}

.permission-lock {
  font-size: 14px;
  line-height: 1.5;
  flex: none;
}

.permission-main {
  flex: 1;
  min-width: 0;
}

.permission-headline {
  color: var(--text);
  font-size: 12.5px;
  font-weight: 600;
}

.permission-paths {
  display: flex;
  flex-wrap: wrap;
  gap: 5px;
  margin-top: 4px;
}

.permission-path {
  padding: 0 7px;
  border-radius: 999px;
  border: 1px solid color-mix(in srgb, var(--amber) 35%, var(--border));
  background: color-mix(in srgb, var(--panel) 55%, transparent);
  color: var(--muted);
  font-family: var(--font-mono);
  font-size: 10.5px;
  line-height: 17px;
  max-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.permission-actions {
  display: flex;
  align-items: center;
  gap: 6px;
  flex: none;
  padding-top: 1px;
}

.permission-btn {
  padding: 4px 11px;
  border-radius: var(--radius-sm);
  font-size: 11.5px;
  font-weight: 600;
  border: 1px solid transparent;
  transition: all 0.15s ease;
}

.permission-btn:disabled {
  opacity: 0.5;
  cursor: default;
}

.permission-btn.allow {
  background: var(--accent);
  color: #fff;
}

.permission-btn.allow:hover:not(:disabled) {
  opacity: 0.9;
}

.permission-btn.always {
  border-color: var(--border-strong);
  background: var(--panel);
  color: var(--text);
}

.permission-btn.always:hover:not(:disabled) {
  border-color: var(--accent);
  color: var(--accent);
}

.permission-btn.reject {
  border-color: color-mix(in srgb, var(--red) 40%, transparent);
  background: transparent;
  color: var(--red);
}

.permission-btn.reject:hover:not(:disabled) {
  border-color: var(--red);
  background: var(--red-soft);
}

.composer-box {
  max-width: 1000px;
  margin: 0 auto;
  display: flex;
  flex-direction: column;
  gap: 7px;
}

.composer-tools {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
  min-height: 20px;
}

.tool-select {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  color: var(--faint);
  font-size: 11px;
}

.tool-select select {
  max-width: 220px;
  border: 1px solid var(--border);
  border-radius: 999px;
  background: var(--panel-2);
  color: var(--muted);
  font-size: 11px;
  padding: 2px 6px;
  cursor: pointer;
}

.tool-select select:focus {
  outline: none;
  border-color: var(--accent);
}

.tool-tip {
  color: var(--faint);
  font-size: 10.5px;
  margin-left: auto;
}

.composer-input-wrap {
  position: relative;
  display: flex;
  flex-direction: column;
}

.composer-popup {
  position: absolute;
  left: 0;
  right: 0;
  bottom: calc(100% + 6px);
  z-index: 40;
  display: flex;
  flex-direction: column;
  max-height: 240px;
  overflow-y: auto;
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-sm);
  background: var(--panel);
  box-shadow: var(--shadow);
}

.popup-item {
  display: flex;
  align-items: baseline;
  gap: 8px;
  padding: 6px 10px;
  border: none;
  background: transparent;
  color: var(--text);
  text-align: left;
  font-size: 12px;
  cursor: pointer;
}

.popup-item.active,
.popup-item:hover {
  background: var(--panel-2);
}

.popup-main {
  font-weight: 600;
  white-space: nowrap;
}

.popup-sub {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--faint);
  font-family: var(--font-mono);
  font-size: 10.5px;
}

.composer-attachments {
  display: flex;
  gap: 6px;
  flex-wrap: wrap;
}

.attach-chip {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 2px 8px;
  border: 1px solid var(--border);
  border-radius: 999px;
  background: var(--panel-2);
  color: var(--accent);
  font-size: 11px;
}

.chip-x {
  border: none;
  background: transparent;
  color: var(--faint);
  font-size: 10px;
  padding: 0;
  cursor: pointer;
}

.chip-x:hover {
  color: var(--red);
}

.send-btn.shell-btn {
  background: var(--amber);
}

.composer-input {
  width: 100%;
  resize: none;
  max-height: 172px;
  overflow-y: auto;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--panel-2);
  color: var(--text);
  font-family: inherit;
  font-size: 12.5px;
  line-height: 1.55;
  padding: 8px 11px;
  transition: border-color 0.15s ease;
}

.composer-input:focus {
  outline: none;
  border-color: color-mix(in srgb, var(--accent) 55%, var(--border));
}

.composer-input:disabled {
  opacity: 0.6;
}

.composer-actions {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 10px;
}

.composer-hint {
  color: var(--faint);
  font-size: 11px;
}

.send-btn {
  padding: 5px 16px;
  border-radius: var(--radius-sm);
  background: var(--accent);
  color: #fff;
  font-size: 12px;
  font-weight: 600;
  transition: opacity 0.15s ease;
}

.send-btn:hover:not(:disabled) {
  opacity: 0.9;
}

.send-btn:disabled {
  opacity: 0.45;
  cursor: default;
}

.stop-btn {
  padding: 5px 16px;
  border-radius: var(--radius-sm);
  border: 1px solid color-mix(in srgb, var(--red) 45%, transparent);
  background: var(--red-soft);
  color: var(--red);
  font-size: 12px;
  font-weight: 600;
}

.stop-btn:hover:not(:disabled) {
  border-color: var(--red);
}

.stop-btn:disabled {
  opacity: 0.55;
  cursor: default;
}

/* 工作台列内嵌：更紧凑的内边距 */
.session-view.embedded .timeline {
  padding: 10px 12px 28px;
}

.session-view.embedded .composer {
  padding: 8px 12px 10px;
}

/* 回底浮标（未吸附底部时显示） */
.to-bottom {
  position: absolute;
  right: 18px;
  bottom: 110px;
  z-index: 5;
  width: 34px;
  height: 34px;
  border-radius: 50%;
  border: 1px solid var(--border);
  background: var(--panel);
  color: var(--muted);
  cursor: pointer;
  font-size: 15px;
  line-height: 1;
  display: flex;
  align-items: center;
  justify-content: center;
  box-shadow: var(--shadow);
}

.to-bottom:hover {
  color: var(--text);
  border-color: var(--border-strong);
}

.to-bottom-badge {
  position: absolute;
  top: -6px;
  right: -8px;
  min-width: 18px;
  height: 18px;
  padding: 0 4px;
  border-radius: 9px;
  background: var(--accent);
  color: #fff;
  font-size: 10px;
  line-height: 18px;
  text-align: center;
}
</style>
