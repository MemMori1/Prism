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

<style scoped src="../styles/components/SessionView.css"></style>
