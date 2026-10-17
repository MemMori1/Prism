<script setup lang="ts">
/**
 * 两栏之间的竖向分隔条：按住左右拖改宽度，双击回到默认，键盘 ←/→ 也能调。
 *
 * 只负责手势和可访问性，宽度怎么算是 App 那边调用 panes.ts 的纯函数。
 */
import { ref } from 'vue'
import type { PaneKind } from '../panes'
import { MAX_PROJECTS, MAX_SESSIONS, MIN_PROJECTS, MIN_SESSIONS } from '../panes'

const props = defineProps<{
  kind: PaneKind
  /** 当前该栏宽度，用于读屏播报。 */
  width: number
}>()

const emit = defineEmits<{
  (e: 'start'): void
  (e: 'drag', deltaX: number): void
  (e: 'commit'): void
  (e: 'reset'): void
}>()

/** 起点横坐标；null 表示没在拖。 */
let originX: number | null = null
const dragging = ref(false)
const root = ref<HTMLElement | null>(null)

const label = () => (props.kind === 'projects' ? '项目栏宽度' : '会话栏宽度')

function onPointerDown(event: PointerEvent): void {
  if (event.button !== 0) return
  event.preventDefault()
  originX = event.clientX
  dragging.value = true
  root.value?.setPointerCapture(event.pointerId)
  emit('start')
}

function onPointerMove(event: PointerEvent): void {
  if (originX == null) return
  emit('drag', event.clientX - originX)
}

function finish(event: PointerEvent): void {
  if (originX == null) return
  originX = null
  dragging.value = false
  if (root.value?.hasPointerCapture(event.pointerId)) {
    root.value.releasePointerCapture(event.pointerId)
  }
  emit('commit')
}

function onKeydown(event: KeyboardEvent): void {
  if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return
  event.preventDefault()
  const step = (event.shiftKey ? 48 : 16) * (event.key === 'ArrowRight' ? 1 : -1)
  emit('start')
  emit('drag', step)
  emit('commit')
}

function onDblclick(): void {
  emit('reset')
}
</script>

<template>
  <div
    ref="root"
    class="split"
    :class="{ dragging }"
    role="separator"
    aria-orientation="vertical"
    :aria-label="label()"
    :aria-valuenow="Math.round(width)"
    :aria-valuemin="kind === 'projects' ? MIN_PROJECTS : MIN_SESSIONS"
    :aria-valuemax="kind === 'projects' ? MAX_PROJECTS : MAX_SESSIONS"
    tabindex="0"
    :title="`拖动调整${kind === 'projects' ? '项目' : '会话'}栏宽度，双击恢复默认`"
    @pointerdown="onPointerDown"
    @pointermove="onPointerMove"
    @pointerup="finish"
    @pointercancel="finish"
    @lostpointercapture="finish"
    @keydown="onKeydown"
    @dblclick="onDblclick"
  >
    <!-- 元素本身 0 宽度（不占布局），靠这块透明区域跨两侧各 3px 接住手势 -->
    <span class="split-hit" aria-hidden="true" />
  </div>
</template>

<style scoped src="../styles/components/SplitHandle.css"></style>
