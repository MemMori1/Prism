<script setup lang="ts">
/**
 * 面板里的浏览器。
 *
 * 页面本身不在这个 DOM 里：它是一块挂在主窗口上的原生 webview（Rust 侧
 * `browser.rs`），我们只负责三件事——
 * 1. 把内容区的矩形报上去（`browser_bounds`），原生视图才会跟手；
 * 2. 该显示时显示、该藏时藏（切走标签、收起侧栏、弹出设置等浮层）；
 * 3. 地址栏、导航按钮、以及「起始页」上的最近打开。
 *
 * z 序是这里最反直觉的地方：原生视图永远画在所有 DOM 之上，所以只要上面出现
 * 任何浮层（设置 / 新建会话 / 规划弹层 / 制品预览…）就必须先把它藏起来，
 * 否则弹窗会被一块网页盖住。下面 `OVERLAY_SELECTOR` 就是这份清单。
 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { listen } from '@tauri-apps/api/event'
import { api } from '../api'
import { basename, relativeTime } from '../format'
import {
  ensureRecent,
  filesToShow,
  hostOf,
  pagesToShow,
  relativePath,
  rememberPage,
} from '../recent'
import type { BrowserSnapshot } from '../types'

const props = defineProps<{
  /** 打开这个标签时想去的地址（外部意图），null = 先落在起始页。 */
  initialUrl?: string | null
  /** 这个标签是不是当前显示的：不是就让位（原生视图不能跟着 v-show 走）。 */
  active: boolean
  /** 当前会话目录：起始页里优先列这个项目的最近文件。 */
  directory?: string | null
}>()

const emit = defineEmits<{
  (e: 'title', title: string): void
  (e: 'open-file', path: string): void
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
/** 这个标签里是否已经开过页面：false 时显示起始页（最近打开）。 */
const started = ref(false)
/** 起始页数据变动的计数：清空 / 记录之后用它触发重算。 */
const recentTick = ref(0)

let unlisten: (() => void) | null = null
let observer: ResizeObserver | null = null
let poll = 0
let shown = false

const ready = computed(() => state.value.open)
const pages = computed(() => {
  recentTick.value
  return pagesToShow(6)
})
const files = computed(() => {
  recentTick.value
  return filesToShow(props.directory ?? null, 6)
})

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
  if (shown || !started.value) return
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
  if (!props.active || !state.value.open || !started.value || overlayOpen() || !rectOf()) {
    hide()
    return
  }
  void show()
}

/** 回到起始页（最近打开）。原生视图先让开，DOM 才露得出来。 */
function showStart(): void {
  hide()
  started.value = false
  failure.value = ''
  void nextTick(() => contentEl.value?.querySelector('input')?.focus())
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
    started.value = true
    emit('title', snapshot.title || snapshot.url || url)
    rememberPage(snapshot.url || url, snapshot.title)
    recentTick.value += 1
    await nextTick()
    // 先报尺寸再显示：否则会先看到一小块空白再跳到正确位置。
    settle()
    refreshVisibility()
  } catch (cause) {
    // 内嵌浏览器建不出来（运行时拒绝 / 平台限制）：给一句实话，退路是系统浏览器。
    failure.value = String(cause)
    state.value = { url: '', title: '', loading: false, open: false }
    started.value = false
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

function openRecentFile(path: string): void {
  emit('open-file', path)
}

onMounted(async () => {
  ensureRecent()

  void listen<BrowserSnapshot>('browser.state', envelope => {
    const next = envelope.payload
    state.value = next
    if (next.url) input.value = next.url
    if (next.title) emit('title', next.title)
    // 有页面落定就切到页面视图：Agent 在后台导过去的页面，用户也该立刻看到。
    if (/^https?:/i.test(next.url)) started.value = true
    // 页面真正落定后再记一条：Agent 导过去的页面也一并记上。
    if (next.open && !next.loading && next.url) {
      rememberPage(next.url, next.title)
      recentTick.value += 1
    }
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

  // 带地址进来的意图（从别处点一个链接）就直接开；否则落在起始页。
  if (props.initialUrl && props.active) void open(props.initialUrl)

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
      <button class="web-icon" title="后退" aria-label="后退" :disabled="!ready" @click="history('back')">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M14.5 5.5 8 12l6.5 6.5" />
        </svg>
      </button>
      <button class="web-icon" title="前进" aria-label="前进" :disabled="!ready" @click="history('forward')">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M9.5 5.5 16 12l-6.5 6.5" />
        </svg>
      </button>
      <button
        class="web-icon"
        :class="{ spinning: state.loading }"
        title="刷新"
        aria-label="刷新"
        :disabled="!ready"
        @click="history('reload')"
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M19.5 12a7.5 7.5 0 1 1-2.2-5.3" />
          <path d="M19.5 4.5V9H15" />
        </svg>
      </button>

      <label class="web-address">
        <svg class="web-address-glyph" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <circle cx="12" cy="12" r="8.4" />
          <path d="M3.6 12h16.8M12 3.6c2.2 2.5 3.3 5.4 3.3 8.4S14.2 18 12 20.4C9.8 18 8.7 15 8.7 12S9.8 6.1 12 3.6Z" />
        </svg>
        <input
          v-model="input"
          type="text"
          spellcheck="false"
          placeholder="输入地址，例如 docs.rs/tauri 或 localhost:3000"
          @keydown.enter.prevent="open()"
        />
        <button class="web-go" title="打开（回车）" aria-label="打开" :disabled="busy" @click="open()">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M5.5 12h12M12.5 6.5 18 12l-5.5 5.5" />
          </svg>
        </button>
      </label>

      <button class="web-icon" title="最近打开" aria-label="最近打开" @click="showStart">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <circle cx="12" cy="12" r="8.4" />
          <path d="M12 7.6V12l3.2 2" />
        </svg>
      </button>
      <button class="web-icon" title="用系统浏览器打开" aria-label="用系统浏览器打开" @click="openExternally">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M14 4.5h5.5V10" />
          <path d="M19.5 4.5 12 12" />
          <path d="M18 14.5V19a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h4.5" />
        </svg>
      </button>
    </div>

    <!-- 页面本体是原生视图，会盖在这块区域上面；这里只放起始页与失败说明。 -->
    <div ref="contentEl" class="web-content">
      <div v-if="failure" class="web-note error">
        <p class="web-note-title">内嵌浏览器打不开这块页面</p>
        <p class="web-note-body">{{ failure }}</p>
        <div class="web-note-actions">
          <button class="web-pill" @click="openExternally">用系统浏览器打开</button>
          <button class="web-pill" @click="failure = ''">重试</button>
        </div>
      </div>

      <div v-else class="web-start">
        <p class="web-start-title">在这块面板里开网页</p>
        <p class="web-start-sub">
          真 webview：JS 渲染、登录态都在，Agent 的 <code>browser.*</code> 看到的也是这一页。
        </p>

        <div v-if="pages.length || files.length" class="web-recent">
          <section v-if="pages.length" class="web-recent-block">
            <h4 class="web-recent-head">最近打开的页面</h4>
            <button
              v-for="page in pages"
              :key="page.url"
              class="web-recent-row"
              :title="page.url"
              @click="open(page.url)"
            >
              <span class="web-recent-main">{{ page.title || hostOf(page.url) }}</span>
              <span class="web-recent-meta">{{ hostOf(page.url) }} · {{ relativeTime(page.at) }}</span>
            </button>
          </section>

          <section v-if="files.length" class="web-recent-block">
            <h4 class="web-recent-head">最近打开的文件</h4>
            <button
              v-for="file in files"
              :key="file.path"
              class="web-recent-row"
              :title="file.path"
              @click="openRecentFile(file.path)"
            >
              <span class="web-recent-main">{{ basename(file.path) }}</span>
              <span class="web-recent-meta">
                {{ relativePath(file.path, directory ?? null) }} · {{ relativeTime(file.at) }}
              </span>
            </button>
          </section>
        </div>

        <p v-else class="web-start-empty">
          还没有记录：打开一个页面或文件，下次这里就能一键回来。
        </p>
      </div>
    </div>
  </div>
</template>

<style scoped src="../styles/components/WebPane.css"></style>
