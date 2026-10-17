<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue'
import { api } from '../api'
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
import type { PermissionRequest, SessionSummary, TimelineRecord } from '../types'
import { formatCost, formatTokens, fullTime } from '../format'
import TimelineItem from './TimelineItem.vue'
import LiveTurnView from './LiveTurn.vue'

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
  if (loading.value || loadingOlder.value || !hasMore.value) return
  if (el.scrollTop < 120) void loadOlder()
}

// ---- 输入与运行 ----

async function send() {
  if (readOnly.value) return
  const sessionId = props.session?.id
  const content = draft.value.trim()
  if (!sessionId || !content || sending.value) return
  sending.value = true
  try {
    // 执行中发送 = 插话：引擎会把消息投递进当前执行（下一步边界送达）。
    await api.sessionSend(sessionId, content)
    if (props.session?.id !== sessionId) return
    draft.value = ''
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

function onEnter(event: KeyboardEvent) {
  if (event.isComposing) return
  if (!event.shiftKey) {
    event.preventDefault()
    void send()
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
    if (sessionId) {
      void prepareSession(sessionId)
    } else {
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
  <main class="session-view" :class="{ embedded }">
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
          <h1 class="session-title">{{ session.title || '未命名会话' }}</h1>
          <button
            class="open-wb-btn"
            title="在工作台打开：多列并行盯着多个会话"
            @click="emit('openWorkbench', session.id)"
          >
            ⇥ 工作台
          </button>
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

      <div ref="scroller" class="timeline" @scroll.passive="onScroll">
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
          <textarea
            ref="composerInput"
            v-model="draft"
            class="composer-input"
            rows="3"
            spellcheck="false"
            :placeholder="
              running
                ? '执行中：发送 = 插话引导（下一步送达），Shift+Enter 换行'
                : '发一条指令…（Enter 发送，Shift+Enter 换行）'
            "
            :disabled="sending"
            @input="autoGrow"
            @keydown.enter="onEnter"
          />
          <div class="composer-actions">
            <span v-if="sending" class="composer-hint">发送中…</span>
            <span v-else-if="running" class="composer-hint">执行中，发送即插话</span>
            <button
              v-if="running"
              class="send-btn"
              :disabled="!draft.trim() || sending"
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
              :disabled="!draft.trim() || sending"
              @click="send"
            >
              发送 ⏎
            </button>
          </div>
        </div>
        </template>
      </footer>
    </template>
  </main>
</template>

<style scoped>
.session-view {
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
</style>
