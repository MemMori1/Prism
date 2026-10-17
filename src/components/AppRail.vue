<script setup lang="ts">
import { computed, onBeforeUnmount, ref } from 'vue'
import {
  NAV_ITEMS,
  loadNavOrder,
  reorderNavItems,
  saveNavOrder,
  type AppView,
  type NavItem,
} from '../nav'
import type { WorkbenchStatus } from '../trayStatus'

const props = defineProps<{
  view: AppView
  wbStatus: WorkbenchStatus
}>()

const emit = defineEmits<{
  (event: 'select', view: AppView): void
  (event: 'settings'): void
}>()

/** 工作台角标：等待审批优先显示数量，其次用绿点表示执行中。 */
const waitingBadge = computed(() => {
  const count = props.wbStatus.waiting
  if (count <= 0) return null
  return count > 9 ? '9+' : String(count)
})

/** 首页固定在第一位；其余图标允许拖拽，顺序持久化到 localStorage。 */
const order = ref<AppView[]>(loadNavOrder())
const groupEl = ref<HTMLElement | null>(null)

const orderedItems = computed(() => {
  const byView = new Map(NAV_ITEMS.map(item => [item.view, item]))
  return order.value
    .map(view => byView.get(view))
    .filter((item): item is NavItem => item !== undefined)
})

const dragView = ref<AppView | null>(null)
const dropGap = ref<number | null>(null)
const dropLineTop = computed(() => (dropGap.value === null ? 0 : 4 + dropGap.value * 38))

let activePointer: number | null = null
let startY = 0
let moved = false
let suppressClickView: AppView | null = null
let suppressTimer: number | undefined

function stopPointerListeners() {
  window.removeEventListener('pointermove', onPointerMove)
  window.removeEventListener('pointerup', onPointerUp)
  window.removeEventListener('pointercancel', onPointerCancel)
  window.removeEventListener('blur', onWindowBlur)
}

function clearDragState() {
  dragView.value = null
  dropGap.value = null
  activePointer = null
  moved = false
}

function resetDrag() {
  stopPointerListeners()
  clearDragState()
}

/** 图标槽位 = 34px 图标 + 4px 间距；返回插入间隙（1 表示首页之后）。 */
function gapAt(clientY: number): number {
  const rect = groupEl.value?.getBoundingClientRect()
  if (!rect) return 1
  const raw = Math.round((clientY - rect.top) / 38)
  return Math.max(1, Math.min(order.value.length, raw))
}

function onPointerDown(event: PointerEvent, item: NavItem) {
  if (item.view === 'home' || event.button !== 0 || !event.isPrimary) return
  // 新的一次按下代表用户已经开始了下一次点击，不再拦截上一次拖拽的 click。
  suppressClickView = null
  resetDrag()
  activePointer = event.pointerId
  dragView.value = item.view
  startY = event.clientY
  window.addEventListener('pointermove', onPointerMove, { passive: false })
  window.addEventListener('pointerup', onPointerUp)
  window.addEventListener('pointercancel', onPointerCancel)
  window.addEventListener('blur', onWindowBlur)
}

function onPointerMove(event: PointerEvent) {
  if (event.pointerId !== activePointer || dragView.value === null) return
  if (!moved && Math.abs(event.clientY - startY) < 5) return
  event.preventDefault()
  moved = true
  dropGap.value = gapAt(event.clientY)
}

function onPointerUp(event: PointerEvent) {
  if (event.pointerId !== activePointer) return
  const view = dragView.value
  const gap = dropGap.value
  const didMove = moved
  stopPointerListeners()
  clearDragState()

  if (view === null || gap === null || !didMove) return

  // 只要发生过拖拽，结束后的那一次 click 就不应触发视图切换。
  suppressClickView = view
  if (suppressTimer !== undefined) window.clearTimeout(suppressTimer)
  suppressTimer = window.setTimeout(() => {
    if (suppressClickView === view) suppressClickView = null
  }, 250)

  const next = reorderNavItems(order.value, view, gap)
  if (next.every((value, index) => value === order.value[index])) return

  order.value = next
  saveNavOrder(next)
}

function onPointerCancel() {
  resetDrag()
}

function onWindowBlur() {
  resetDrag()
}

function onItemClick(item: NavItem) {
  if (suppressClickView === item.view) {
    suppressClickView = null
    return
  }
  emit('select', item.view)
}

/** 键盘补充：聚焦图标后 Alt+↑/↓ 也能调整顺序。 */
function onItemKeydown(event: KeyboardEvent, item: NavItem) {
  if (item.view === 'home' || !event.altKey) return
  if (event.key !== 'ArrowUp' && event.key !== 'ArrowDown') return

  const from = order.value.indexOf(item.view)
  const to = from + (event.key === 'ArrowUp' ? -1 : 1)
  if (from < 1 || to < 1 || to >= order.value.length) return

  event.preventDefault()
  const next = [...order.value]
  const [movedItem] = next.splice(from, 1)
  next.splice(to, 0, movedItem)
  order.value = next
  saveNavOrder(next)
}

function itemTitle(item: NavItem): string {
  return item.view === 'home' ? item.hint : `${item.hint}（拖拽或 Alt+↑/↓ 排序）`
}

const SETTINGS_ICON = ['M4 7h16', 'M4 12h16', 'M4 17h16', 'M9 5.5v3', 'M15 10.5v3', 'M8 15.5v3']

onBeforeUnmount(() => {
  stopPointerListeners()
  if (suppressTimer !== undefined) window.clearTimeout(suppressTimer)
})
</script>

<template>
  <nav class="rail" aria-label="主导航">
    <div ref="groupEl" class="rail-group">
      <button
        v-for="item in orderedItems"
        :key="item.view"
        class="rail-item"
        :class="{
          active: view === item.view,
          dragging: dragView === item.view && dropGap !== null,
          fixed: item.view === 'home',
        }"
        :data-view="item.view"
        :title="itemTitle(item)"
        :aria-label="itemTitle(item)"
        :aria-current="view === item.view ? 'page' : undefined"
        @click="onItemClick(item)"
        @pointerdown="onPointerDown($event, item)"
        @keydown="onItemKeydown($event, item)"
      >
        <svg
          class="rail-icon"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="1.7"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path v-for="d in item.icon" :key="d" :d="d" />
        </svg>
        <span v-if="item.view === 'workbench' && waitingBadge" class="rail-badge">
          {{ waitingBadge }}
        </span>
        <span
          v-else-if="item.view === 'workbench' && wbStatus.running"
          class="rail-dot"
          :title="`${wbStatus.running} 个会话执行中`"
        />
      </button>
    </div>

    <div
      v-if="dropGap !== null"
      class="rail-drop-line"
      :style="{ top: dropLineTop + 'px' }"
      aria-hidden="true"
    />

    <div class="rail-spacer" />

    <button class="rail-item fixed" title="设置" aria-label="设置" @click="emit('settings')">
      <svg
        class="rail-icon"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="1.7"
        stroke-linecap="round"
        stroke-linejoin="round"
        aria-hidden="true"
      >
        <path v-for="d in SETTINGS_ICON" :key="d" :d="d" />
      </svg>
    </button>
  </nav>
</template>

<style scoped>
.rail {
  position: relative;
  display: flex;
  flex: none;
  flex-direction: column;
  align-items: center;
  gap: 4px;
  width: 48px;
  padding: 6px 0 10px;
  border-right: 1px solid var(--border);
  background: var(--panel-2);
}

.rail-group {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.rail-spacer {
  flex: 1;
}

.rail-item {
  position: relative;
  display: grid;
  place-items: center;
  width: 34px;
  height: 34px;
  border-radius: var(--radius-sm);
  color: var(--muted);
  touch-action: none;
  user-select: none;
  transition:
    color 0.15s ease,
    background 0.15s ease;
}

.rail-item:not(.fixed) {
  cursor: grab;
}

.rail-item.dragging {
  cursor: grabbing;
  opacity: 0.35;
}

.rail-item:hover {
  color: var(--text);
  background: color-mix(in srgb, var(--border) 45%, transparent);
}

/* 选中态参考 monet：纸片底 + 左侧强调条。 */
.rail-item.active {
  background: var(--accent-soft);
  color: var(--accent-text);
  box-shadow: 0 1px 2px rgb(18 16 34 / 0.06);
}

.rail-item.active::before {
  content: '';
  position: absolute;
  top: 8px;
  bottom: 8px;
  left: -7px;
  width: 2px;
  border-radius: 1px;
  background: var(--accent);
}

.rail-drop-line {
  position: absolute;
  left: 8px;
  right: 8px;
  height: 2px;
  border-radius: 2px;
  background: var(--accent);
  box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent) 16%, transparent);
  pointer-events: none;
  z-index: 3;
}

.rail-icon {
  width: 18px;
  height: 18px;
}

.rail-badge {
  position: absolute;
  top: 1px;
  right: 1px;
  min-width: 13px;
  padding: 0 3px;
  border-radius: 4px;
  background: var(--amber);
  color: #fff;
  font-size: 9px;
  font-weight: 700;
  line-height: 13px;
  text-align: center;
}

.rail-dot {
  position: absolute;
  right: 3px;
  bottom: 3px;
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--green);
  animation: rail-pulse 1.6s ease infinite;
}

@keyframes rail-pulse {
  50% {
    opacity: 0.35;
  }
}
</style>
