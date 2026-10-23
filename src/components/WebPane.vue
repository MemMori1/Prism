<script setup lang="ts">
/**
 * 面板里的浏览器。
 *
 * 页面本身不在这个 DOM 里：它是一块挂在主窗口上的原生 webview（Rust 侧
 * `browser.rs`），我们只负责三件事——
 * 1. 把内容区的矩形报上去（`browser_bounds`），原生视图才会跟手；
 * 2. 该显示时显示、该藏时藏（切走标签、收起侧栏、弹出设置等浮层）；
 * 3. 地址栏与导航按钮。
 *
 * z 序是这里最反直觉的地方：原生视图永远画在所有 DOM 之上，所以只要上面出现
 * 任何浮层（设置 / 新建会话 / 规划弹层 / 制品预览…）就必须先把它藏起来，
 * 否则弹窗会被一块网页盖住。下面 `OVERLAY_SELECTOR` 就是这份清单。
 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { listen } from '@tauri-apps/api/event'
import { api } from '../api'
import type { BrowserSnapshot } from '../types'

const props = defineProps<{
  /** 打开这个标签时想去的地址（外部意图），null = 用当前浏览器状态。 */
  initialUrl?: string | null
  /** 这个标签是不是当前显示的：不是就让位（原生视图不能跟着 v-show 走）。 */
  active: boolean
}>()

const emit = defineEmits<{
  (e: 'title', title: string): void
}>()

/**
 * 会盖住原生 webview 的浮层。新加弹窗时把它的根类名补进来——漏一个的表现是
 * 「打开设置，设置被网页盖住了」。
 */
const OVERLAY_SELECTOR = [
  '.settings-overlay',
  '.dialog-overlay',
  '.planner-backdrop',
  '.ns-overlay',
  '.artifact-overlay',
  '.nav-backdrop',
].join(',')

const input = ref('')
const state = ref<BrowserSnapshot>({ url: '', title: '', loading: false, open: false })
const failure = ref('')
const busy = ref(false)
const contentEl = ref<HTMLElement | null>(null)

let unlisten: (() => void) | null = null
let observer: ResizeObserver | null = null
let poll = 0
let shown = false

const ready = computed(() => state.value.open)

function report(cause: unknown): void {
  failure.value = String(cause)
}

/** 内容区矩形（CSS 像素即逻辑像素，与窗口客户区左上角对齐）。 */
function rectOf(): { x: number; y: number; width: number; height: number } | null {
  const el = contentEl.value
  if (!el) return null
  const rect = el.getBoundingClientRect()
  if (rect.width < 2 || rect.height < 2) return null
  return { x: rect.left, y: rect.top, width: rect.width, height: rect.height }
}

function syncBounds(): void {
  const rect = rectOf()
  if (!rect) return
  void api.browserBounds(rect).catch(cause => report(cause))
}

/** 抽屉有开合动画：一次同步不够，铺几个时间点抓稳。 */
function settle(): void {
  syncBounds()
  for (const delay of [60, 140, 260]) window.setTimeout(syncBounds, delay)
}

async function show(): Promise<void> {
  if (shown) return
  const rect = rectOf()
  if (!rect) return
  shown = true
  try {
    // 先定位再露脸：反过来的话会先看到一小块 1×1 再跳过去。
    await api.browserBounds(rect)
    await api.browserVisible(true)
  } catch (cause) {
    shown = false
    report(cause)
  }
}

function hide(): void {
  if (!shown) return
  shown = false
  void api.browserVisible(false).catch(() => {})
}

/** 上面有没有浮层；有就先把原生视图收起来。 */
function overlayOpen(): boolean {
  return !!document.querySelector(OVERLAY_SELECTOR)
}

function refreshVisibility(): void {
  // 矩形为空说明这块地方根本没上屏（view 被 v-show 收起来了、窗口最小化）——
  // 原生视图不会跟着 display:none 走，只能自己收起来。
  if (!props.active || !state.value.open || overlayOpen() || !rectOf()) {
    hide()
    return
  }
  void show()
}

async function open(target?: string): Promise<void> {
  const url = (target ?? input.value).trim()
  if (!url || busy.value) return
  busy.value = true
  failure.value = ''
  try {
    const snapshot = await api.browserOpen(url)
    state.value = snapshot
    input.value = snapshot.url || url
    emit('title', snapshot.title || snapshot.url || url)
    await nextTick()
    // 先报尺寸再显示：否则会先看到一小块空白再跳到正确位置。
    settle()
    refreshVisibility()
  } catch (cause) {
    // 内嵌浏览器建不出来（运行时拒绝 / 平台限制）：给一句实话，退路是系统浏览器。
    failure.value = String(cause)
    state.value = { url: '', title: '', loading: false, open: false }
  } finally {
    busy.value = false
  }
}

function history(direction: 'back' | 'forward' | 'reload'): void {
  void api.browserHistory(direction).catch(cause => report(cause))
}

async function openExternally(): Promise<void> {
  const url = (input.value || state.value.url).trim()
  if (!url) return
  try {
    await api.openUrl(url)
  } catch (cause) {
    report(cause)
  }
}

onMounted(async () => {
  void listen<BrowserSnapshot>('browser.state', envelope => {
    const next = envelope.payload
    state.value = next
    if (next.url) input.value = next.url
    if (next.title) emit('title', next.title)
    refreshVisibility()
  }).then(off => {
    unlisten = off
  })

  try {
    const current = await api.browserState()
    state.value = current
    if (current.url) input.value = current.url
    if (current.title) emit('title', current.title)
  } catch {
    // 读不到状态不影响继续用：地址栏照常输入。
  }

  if (props.active) {
    if (props.initialUrl) void open(props.initialUrl)
    else refreshVisibility()
  }
  if (contentEl.value) {
    observer = new ResizeObserver(() => syncBounds())
    observer.observe(contentEl.value)
  }
  window.addEventListener('resize', syncBounds)
  // 浮层不是我们的子组件（可能在别的面板里），只能盯着 DOM 看它来没来。
  // 250ms 一次、只查一个选择器，代价可以忽略；停用时不跑。
  poll = window.setInterval(() => {
    if (props.active) refreshVisibility()
  }, 250)
})

onBeforeUnmount(() => {
  hide()
  if (poll) window.clearInterval(poll)
  window.removeEventListener('resize', syncBounds)
  observer?.disconnect()
  unlisten?.()
  unlisten = null
})

watch(
  () => props.active,
  active => {
    if (active) {
      // 切换标签时 DOM 刚显示出来，矩形要等一帧才准。
      void nextTick(() => {
        refreshVisibility()
        settle()
      })
    } else {
      hide()
    }
  },
)

watch(
  () => props.initialUrl,
  url => {
    if (url && props.active) void open(url)
  },
)
</script>

<template>
  <div class="web-pane">
    <div class="web-bar">
      <button class="web-btn" title="后退" :disabled="!ready" @click="history('back')">‹</button>
      <button class="web-btn" title="前进" :disabled="!ready" @click="history('forward')">›</button>
      <button class="web-btn" title="刷新" :disabled="!ready" @click="history('reload')">⟳</button>
      <input
        v-model="input"
        class="web-url"
        type="text"
        spellcheck="false"
        placeholder="输入地址，例如 docs.rs/tauri 或 localhost:3000"
        @keydown.enter.prevent="open()"
      />
      <button class="web-btn go" title="打开（回车）" :disabled="busy" @click="open()">
        {{ busy ? '…' : '打开' }}
      </button>
      <button class="web-btn" title="用系统浏览器打开" @click="openExternally">↗</button>
    </div>

    <!-- 页面本体是原生视图，会盖在这块区域上面；这里只放空状态与失败说明。 -->
    <div ref="contentEl" class="web-content">
      <div v-if="failure" class="web-note error">
        <p class="web-note-title">内嵌浏览器打不开这块页面</p>
        <p class="web-note-body">{{ failure }}</p>
        <div class="web-note-actions">
          <button class="web-btn" @click="openExternally">用系统浏览器打开</button>
          <button class="web-btn" @click="failure = ''">重试</button>
        </div>
      </div>
      <div v-else-if="!state.open" class="web-note">
        <p class="web-note-title">面板里开一个真浏览器</p>
        <p class="web-note-body">
          输入地址回车即可。它是一块真实的 webview：JS 渲染、登录态、点击都正常，
          Agent 的 <code>browser.*</code> 工具看的就是同一页。
        </p>
      </div>
      <p v-else class="web-hint">上面显示的是当前页面</p>
    </div>
  </div>
</template>

<style scoped src="../styles/components/WebPane.css"></style>
