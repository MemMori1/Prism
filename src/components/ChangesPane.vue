<script setup lang="ts">
/**
 * 面板外壳：顶部一行标签页，下面每个标签自己的内容。
 *
 * 「＋」新建一个空标签页，页面正中给两个入口（打开文件 / 打开终端）；选完就地
 * 把这个标签换成对应内容，标签名跟着变。标签之间完全独立：开两个文件页看不同
 * 文件、开两个终端页盯不同任务都行。
 *
 * 以后要接网页 / 预览之类的面板，只需要多一种 PaneKind 和一块内容，外壳不用动。
 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { PANE_LABEL, closeTab, makeTab, moveTab, pickTab } from '../paneTabs'
import type { PaneKind, PaneTab } from '../paneTabs'
import { activeTaskCount } from '../tasks'
import FilePane from './FilePane.vue'
import TerminalPane from './TerminalPane.vue'

/** 外部（会话头 / 任务坞 / 聊天里的路径）要让侧栏打开什么。 */
export type PaneIntent =
  | { kind: 'files'; path?: string | null }
  | { kind: 'term'; taskId?: string | null; newTask?: boolean }

const props = defineProps<{
  /** 会话的工作目录；null＝文件页没东西可列。 */
  directory: string | null
  /** 当前会话（终端页新建任务要记来源）。 */
  sessionId?: string | null
  /** 现在有没有回合在跑：跑完文件页自动刷一次。 */
  running?: boolean
  /** 要打开的东西；同一个意图再给一次时靠 intentTick 区分。 */
  intent?: PaneIntent | null
  intentTick?: number
}>()

const emit = defineEmits<{
  (e: 'close'): void
}>()

let seq = 0
let resizeObserver: ResizeObserver | null = null

function nextTab(kind: PaneKind): PaneTab {
  seq += 1
  return makeTab(`pane-${seq}`, kind)
}

/** 打开侧栏先落在文件页（和以前一样）。 */
const tabs = ref<PaneTab[]>([nextTab('files')])
const activeId = ref(tabs.value[0].id)
const active = computed<PaneTab | null>(
  () => tabs.value.find(tab => tab.id === activeId.value) ?? tabs.value[0] ?? null,
)

/** 标签条：横向溢出时滚轮也能滚（隐藏了滚动条，得给个手势）。 */
const tabsEl = ref<HTMLElement | null>(null)

function activate(id: string): void {
  activeId.value = id
  revealTab()
}

/** 让当前标签露出来：新开 / 切过去 / 拖动后都可能把它滚到视野外。 */
function revealTab(): void {
  void nextTick(() => {
    const el = tabsEl.value?.querySelector('.pane-tab.on')
    if (el instanceof HTMLElement) el.scrollIntoView({ block: 'nearest', inline: 'nearest' })
    syncEdges()
  })
}

/** 两边还有没有藏起来的标签：用来决定要不要画渐隐，暗示「还能滚」。 */
const edge = ref({ left: false, right: false })

function syncEdges(): void {
  const el = tabsEl.value
  if (!el) return
  edge.value = {
    left: el.scrollLeft > 2,
    right: el.scrollLeft + el.clientWidth < el.scrollWidth - 2,
  }
}

function onWheel(event: WheelEvent): void {
  const el = tabsEl.value
  if (!el || el.scrollWidth <= el.clientWidth) return
  const delta = Math.abs(event.deltaX) > Math.abs(event.deltaY) ? event.deltaX : event.deltaY
  if (!delta) return
  event.preventDefault()
  el.scrollLeft += delta
  syncEdges()
}

/** 拖动换位置：拖到某个标签的左右半区就插到它前 / 后。 */
const dragId = ref<string | null>(null)

function startDrag(event: DragEvent, id: string): void {
  dragId.value = id
  if (event.dataTransfer) {
    event.dataTransfer.effectAllowed = 'move'
    // Firefox 不带数据就不会开始拖
    event.dataTransfer.setData('text/plain', id)
  }
}

function overTab(event: DragEvent, id: string): void {
  if (!dragId.value || dragId.value === id) return
  event.preventDefault()
  if (event.dataTransfer) event.dataTransfer.dropEffect = 'move'
  autoScrollTabs(event)
  const rect = (event.currentTarget as HTMLElement).getBoundingClientRect()
  const after = event.clientX > rect.left + rect.width / 2
  tabs.value = moveTab(tabs.value, dragId.value, id, after)
}

/** 拖到标签条两端时自己滚：原生拖拽不会帮忙滚容器。 */
function autoScrollTabs(event: DragEvent): void {
  const el = tabsEl.value
  if (!el || el.scrollWidth <= el.clientWidth) return
  const rect = el.getBoundingClientRect()
  const edge = 40
  if (event.clientX < rect.left + edge) el.scrollLeft -= 14
  else if (event.clientX > rect.right - edge) el.scrollLeft += 14
}

function endDrag(): void {
  dragId.value = null
}

/** ＋：加一个空标签，等着选内容。 */
function addTab(): void {
  const tab = nextTab('empty')
  tabs.value = [...tabs.value, tab]
  activeId.value = tab.id
  revealTab()
}

function removeTab(id: string): void {
  const next = closeTab(tabs.value, activeId.value, id)
  tabs.value = next.tabs
  if (next.closed) {
    emit('close')
    return
  }
  activeId.value = next.activeId
}

/** 空标签里选「打开文件 / 打开终端」：就地替换这个标签的内容。 */
function choose(kind: 'files' | 'term'): void {
  const tab = active.value
  if (!tab) return
  tab.kind = kind
  tab.title = null
}

function openFiles(path: string | null): void {
  seq += 1
  const picked = pickTab(tabs.value, activeId.value, 'files', `pane-${seq}`)
  tabs.value = picked.tabs
  const tab = tabs.value.find(item => item.id === picked.tabId)
  if (!tab) return
  tab.kind = 'files'
  tab.title = null
  if (path) tab.path = path
  activeId.value = tab.id
  revealTab()
}

function openTerminal(taskId: string | null, newTask: boolean): void {
  seq += 1
  const picked = pickTab(tabs.value, activeId.value, 'term', `pane-${seq}`)
  tabs.value = picked.tabs
  const tab = tabs.value.find(item => item.id === picked.tabId)
  if (!tab) return
  if (tab.kind !== 'term') {
    tab.kind = 'term'
    tab.title = null
    tab.newTaskTick = 0
  }
  if (taskId) tab.taskId = taskId
  if (newTask) tab.newTaskTick += 1
  activeId.value = tab.id
  revealTab()
}

watch(
  () => tabs.value.map(tab => tab.id).join('|'),
  () => {
    void nextTick(syncEdges)
  },
)

watch(
  () => [props.intentTick ?? 0, props.intent] as const,
  () => {
    const intent = props.intent
    if (!intent) return
    if (intent.kind === 'files') openFiles(intent.path ?? null)
    else openTerminal(intent.taskId ?? null, intent.newTask === true)
  },
  { immediate: true },
)

/**
 * Esc 收起侧栏。文件页自己处理（要先关「打开方式」菜单、编辑器也要能 Esc 退出），
 * 所以这里只管空标签和终端页，避免菜单刚关掉、侧栏也跟着关。
 */
function onKeydown(event: KeyboardEvent): void {
  if (event.key !== 'Escape') return
  if (active.value?.kind === 'files') return
  emit('close')
}

onMounted(() => {
  window.addEventListener('keydown', onKeydown)
  // 窗口变宽变窄、标签增减都会改变「还能不能滚」，重新算一次两端渐隐。
  if (tabsEl.value) {
    resizeObserver = new ResizeObserver(() => syncEdges())
    resizeObserver.observe(tabsEl.value)
  }
  void nextTick(syncEdges)
})

onBeforeUnmount(() => {
  window.removeEventListener('keydown', onKeydown)
  resizeObserver?.disconnect()
})
</script>

<template>
  <div class="changes-backdrop" @click="emit('close')" />
  <aside class="changes-drawer" aria-label="面板">
    <header class="pane-strip">
      <div
        ref="tabsEl"
        class="pane-tabs"
        role="tablist"
        aria-label="面板标签页"
        :class="{ 'fade-left': edge.left, 'fade-right': edge.right }"
        @wheel="onWheel"
        @scroll="syncEdges"
      >
        <div
          v-for="tab in tabs"
          :key="tab.id"
          class="pane-tab"
          :class="{ on: tab.id === activeId, empty: tab.kind === 'empty', dragging: dragId === tab.id }"
          role="tab"
          draggable="true"
          :data-tab-id="tab.id"
          :aria-selected="tab.id === activeId"
          :title="tab.title ?? PANE_LABEL[tab.kind]"
          @click="activate(tab.id)"
          @dragstart="startDrag($event, tab.id)"
          @dragover="overTab($event, tab.id)"
          @drop.prevent="endDrag"
          @dragend="endDrag"
        >
          <svg
            class="pane-tab-icon"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="1.7"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <template v-if="tab.kind === 'files'">
              <path d="M6 3.5h8l5 5v12a1.5 1.5 0 0 1-1.5 1.5h-11A1.5 1.5 0 0 1 5 20.5v-15A2 2 0 0 1 7 3.5Z" />
              <path d="M14 3.5V9h5" />
            </template>
            <template v-else-if="tab.kind === 'term'">
              <rect x="3.2" y="4.5" width="17.6" height="15" rx="2" />
              <path d="m7.4 9.4 2.6 2.6-2.6 2.6M12.6 14.8h4" />
            </template>
            <template v-else>
              <!-- 空标签页用虚线方框：和右边那个「＋」按钮区分开，别让人以为也是按钮 -->
              <rect x="4.2" y="4.2" width="15.6" height="15.6" rx="3.4" stroke-dasharray="3 2.4" />
            </template>
          </svg>
          <span class="pane-tab-label">{{ tab.title || PANE_LABEL[tab.kind] }}</span>
          <span v-if="tab.kind === 'term' && activeTaskCount" class="pane-tab-badge">
            {{ activeTaskCount }}
          </span>
          <button
            class="pane-tab-x"
            :title="tab.kind === 'term' ? '关闭标签页（任务继续在后台跑）' : '关闭标签页'"
            aria-label="关闭标签页"
            @click.stop="removeTab(tab.id)"
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" aria-hidden="true">
              <path d="M7 7l10 10M17 7 7 17" />
            </svg>
          </button>
        </div>
      </div>

      <button class="pane-add" title="新建标签页" aria-label="新建标签页" @click="addTab">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
          <path d="M12 6v12M6 12h12" />
        </svg>
      </button>

      <span class="grow" />

      <button class="pane-close" title="关闭（Esc）" aria-label="关闭侧栏" @click="emit('close')">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" aria-hidden="true">
          <path d="M6 6l12 12M18 6 6 18" />
        </svg>
      </button>
    </header>

    <div class="pane-body">
      <div v-for="tab in tabs" v-show="tab.id === activeId" :key="tab.id" class="pane-view">
        <div v-if="tab.kind === 'empty'" class="pane-empty">
          <div class="pane-empty-head">
            <span class="pane-empty-kicker">Empty tab</span>
            <p class="pane-empty-title">这个标签页还是空的</p>
            <p class="pane-empty-sub">挑一样放进来，标签内容会被替换。</p>
          </div>

          <div class="pane-choices">
            <button class="pane-choice" @click="choose('files')">
              <span class="pane-choice-icon">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                  <path d="M6.5 3.6h7.6l4.4 4.4v12.4H6.5z" />
                  <path d="M13.9 3.6V8h4.6" />
                  <path d="M9.5 13h5M9.5 16.4h3.4" />
                </svg>
              </span>
              <span class="pane-choice-name">打开文件</span>
              <span class="pane-choice-sub">看改动、预览内容，也能直接改。</span>
            </button>

            <button class="pane-choice" @click="choose('term')">
              <span class="pane-choice-icon">
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                  <rect x="3.2" y="4.6" width="17.6" height="14.8" rx="2.2" />
                  <path d="m7.6 9.6 2.5 2.5-2.5 2.5M12.8 14.8h3.8" />
                </svg>
              </span>
              <span class="pane-choice-name">打开终端</span>
              <span class="pane-choice-sub">跑后台任务，看完整输出，随时停止或重跑。</span>
            </button>
          </div>

          <p class="pane-empty-foot">
            还能继续点「＋」再开一个标签页——文件、终端各占一格，互不打扰。
          </p>
        </div>

        <FilePane
          v-else-if="tab.kind === 'files'"
          :directory="directory"
          :running="running"
          :initial-path="tab.path"
          @title="tab.title = $event"
          @close="emit('close')"
        />

        <TerminalPane
          v-else
          :directory="directory"
          :session-id="sessionId ?? null"
          :initial-task-id="tab.taskId"
          :new-task-tick="tab.newTaskTick"
          @title="tab.title = $event"
        />
      </div>
    </div>
  </aside>
</template>

<style scoped src="../styles/components/ChangesPane.css"></style>
