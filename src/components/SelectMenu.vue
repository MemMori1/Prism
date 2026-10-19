<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue'

interface SelectOption {
  value: string
  label: string
  /** 次要说明（如路径）：重名项靠它区分，显示在标签下面一行。 */
  hint?: string
  /** 这项有问题（如目录已不存在）：说明用红色，选项跟着变淡。 */
  warn?: boolean
  disabled?: boolean
}

const props = withDefaults(
  defineProps<{
    modelValue: string
    options: SelectOption[]
    placeholder?: string
    disabled?: boolean
    /** xs=22px / sm=26px / md=32px */
    size?: 'xs' | 'sm' | 'md'
    shape?: 'rect' | 'pill'
    /** field=带边框；ghost=无边框（放进 chip 里用） */
    tone?: 'field' | 'ghost'
    /** 宽度随内容，默认撑满父容器 */
    inline?: boolean
    /** inline 时的最大宽度，如 '220px' */
    maxWidth?: string
    title?: string
    /** 弹层最大高度 */
    maxHeight?: number
    /**
     * 列表底部的一条动作（如「新增目录…」）：跟选项一起列在弹层里，
     * 省掉旁边再放一个按钮。留空＝不显示。
     */
    actionLabel?: string
  }>(),
  {
    placeholder: '请选择',
    disabled: false,
    size: 'md',
    shape: 'rect',
    tone: 'field',
    inline: false,
    maxWidth: '',
    title: '',
    maxHeight: 268,
    actionLabel: '',
  },
)

const emit = defineEmits<{
  (e: 'update:modelValue', value: string): void
  (e: 'open'): void
  (e: 'action'): void
}>()

const open = ref(false)
const trigger = ref<HTMLButtonElement | null>(null)
const popup = ref<HTMLElement | null>(null)
const activeIndex = ref(-1)

/** 弹层用 fixed 定位：top/bottom 二选一，避免在弹框、抽屉里被 overflow 裁掉。 */
const pos = ref({ left: 0, top: 0, bottom: null as number | null, width: 240, maxHeight: 268 })

const selectedIndex = computed(() => props.options.findIndex(item => item.value === props.modelValue))
const selected = computed(() => props.options.find(item => item.value === props.modelValue) ?? null)
/** 选中的这项本身有问题（如目录不存在）：触发器也要标出来。 */
const selectedWarn = computed(() => Boolean(selected.value?.warn))
const currentLabel = computed(() => selected.value?.label ?? props.placeholder)

/** 底部动作也算一行：键盘能走到，`activeIndex === options.length` 就是它。 */
const hasAction = computed(() => props.actionLabel.trim().length > 0)
const rowCount = computed(() => props.options.length + (hasAction.value ? 1 : 0))
const actionIndex = computed(() => (hasAction.value ? props.options.length : -1))

/** 这一行能不能选（动作行永远可选）。 */
function rowDisabled(index: number): boolean {
  if (index === actionIndex.value) return false
  return Boolean(props.options[index]?.disabled)
}

const popupStyle = computed(() => {
  const style: Record<string, string> = {
    left: `${pos.value.left}px`,
    width: `${pos.value.width}px`,
    maxHeight: `${pos.value.maxHeight}px`,
  }
  if (pos.value.bottom != null) style.bottom = `${pos.value.bottom}px`
  else style.top = `${pos.value.top}px`
  return style
})

function measure(estimate: number): void {
  const el = trigger.value
  if (!el) return
  const rect = el.getBoundingClientRect()
  const margin = 8
  const gap = 6
  const width = Math.min(
    Math.max(rect.width, props.inline ? 180 : rect.width),
    window.innerWidth - margin * 2,
  )
  const below = window.innerHeight - rect.bottom - margin
  const above = rect.top - margin
  const flip = below < estimate && above > below
  const maxHeight = Math.max(112, Math.min(props.maxHeight, (flip ? above : below) - gap))
  const left = Math.min(Math.max(margin, rect.left), window.innerWidth - width - margin)
  pos.value = {
    left,
    width,
    maxHeight,
    top: rect.bottom + gap,
    bottom: flip ? window.innerHeight - rect.top + gap : null,
  }
}

function firstEnabled(from = 0, step = 1): number {
  const total = rowCount.value
  for (let i = 0; i < total; i += 1) {
    const index = ((from + i * step) % total + total) % total
    if (!rowDisabled(index)) return index
  }
  return -1
}

function scrollActiveIntoView(): void {
  // 动作行钉在下沿，不参与滚动——这里也不该调 scrollIntoView（会连带滚外层容器）。
  if (activeIndex.value >= props.options.length) return
  const node = popup.value?.querySelectorAll<HTMLElement>('.sel-option')[activeIndex.value]
  node?.scrollIntoView({ block: 'nearest' })
}

async function openMenu(): Promise<void> {
  if (props.disabled || open.value) return
  // 带说明的两行项高一些，估算差值只为决定往上弹还是往下弹。
  const rows = props.options.reduce((sum, item) => sum + (item.hint ? 42 : 30), 0)
  const estimate = rows + (hasAction.value ? 40 : 0) + 10
  measure(Math.min(props.maxHeight, estimate))
  open.value = true
  activeIndex.value = selectedIndex.value >= 0 ? selectedIndex.value : firstEnabled()
  emit('open')
  await nextTick()
  measure(Math.min(props.maxHeight, estimate))
  scrollActiveIntoView()
}

function closeMenu(refocus = false): void {
  if (!open.value) return
  open.value = false
  if (refocus) trigger.value?.focus()
}

function toggle(): void {
  if (open.value) closeMenu()
  else void openMenu()
}

function pick(index: number): void {
  if (index === actionIndex.value) {
    closeMenu(true)
    emit('action')
    return
  }
  const option = props.options[index]
  if (!option || option.disabled) return
  if (option.value !== props.modelValue) emit('update:modelValue', option.value)
  closeMenu(true)
}

function move(step: number): void {
  const total = rowCount.value
  if (!total) return
  let next = activeIndex.value
  for (let i = 0; i < total; i += 1) {
    next = (next + step + total) % total
    if (!rowDisabled(next)) break
  }
  activeIndex.value = next
  scrollActiveIntoView()
}

function onTriggerKeydown(event: KeyboardEvent): void {
  if (open.value) return
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp' || event.key === 'Enter' || event.key === ' ') {
    event.preventDefault()
    // 阻断冒泡：否则同一个按键会被 window 上的「菜单内导航」再处理一次，跳两项。
    event.stopPropagation()
    void openMenu()
  }
}

function onPointerDown(event: PointerEvent): void {
  if (!open.value) return
  const target = event.target as Node | null
  if (!target) return
  if (trigger.value?.contains(target) || popup.value?.contains(target)) return
  closeMenu()
}

function onKeydown(event: KeyboardEvent): void {
  if (!open.value) return
  switch (event.key) {
    case 'Escape':
      event.preventDefault()
      closeMenu(true)
      break
    case 'Tab':
      closeMenu()
      break
    case 'ArrowDown':
      event.preventDefault()
      move(1)
      break
    case 'ArrowUp':
      event.preventDefault()
      move(-1)
      break
    case 'Home':
      event.preventDefault()
      activeIndex.value = firstEnabled()
      scrollActiveIntoView()
      break
    case 'End':
      event.preventDefault()
      // 从最后一行往回找：底部动作行也算最后一行。
      activeIndex.value = firstEnabled(rowCount.value - 1, -1)
      scrollActiveIntoView()
      break
    case 'Enter':
    case ' ':
      event.preventDefault()
      pick(activeIndex.value)
      break
    default:
      break
  }
}

function onScroll(event: Event): void {
  if (!open.value) return
  const target = event.target
  if (target instanceof Node && popup.value?.contains(target)) return
  closeMenu()
}

function onViewportChange(): void {
  closeMenu()
}

onMounted(() => {
  window.addEventListener('pointerdown', onPointerDown, true)
  window.addEventListener('keydown', onKeydown)
  window.addEventListener('scroll', onScroll, true)
  window.addEventListener('resize', onViewportChange)
  window.addEventListener('blur', onViewportChange)
})

onBeforeUnmount(() => {
  window.removeEventListener('pointerdown', onPointerDown, true)
  window.removeEventListener('keydown', onKeydown)
  window.removeEventListener('scroll', onScroll, true)
  window.removeEventListener('resize', onViewportChange)
  window.removeEventListener('blur', onViewportChange)
})
</script>

<template>
  <div
    class="sel"
    :class="[
      `sel-${size}`,
      `sel-${shape}`,
      `sel-${tone}`,
      { 'is-inline': inline, 'is-open': open, 'is-disabled': disabled, 'is-warn': selectedWarn },
    ]"
    :style="inline && maxWidth ? { maxWidth } : undefined"
  >
    <button
      ref="trigger"
      type="button"
      class="sel-trigger"
      :disabled="disabled"
      :title="title || currentLabel"
      aria-haspopup="listbox"
      :aria-expanded="open"
      @click="toggle"
      @keydown="onTriggerKeydown"
    >
      <span class="sel-label" :class="{ 'is-placeholder': !selected }">{{ currentLabel }}</span>
      <svg
        class="sel-chevron"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
        aria-hidden="true"
      >
        <path d="m6 9 6 6 6-6" />
      </svg>
    </button>

    <Teleport to="body">
      <div v-if="open" ref="popup" class="sel-popup" :style="popupStyle" role="listbox">
        <!-- 选项区自己滚：底部的动作行固定住，长列表（几十个项目）也随时点得到。 -->
        <div class="sel-list">
          <button
            v-for="(option, index) in options"
            :key="option.value"
            type="button"
            class="sel-option"
            :class="{
              active: index === activeIndex,
              selected: option.value === modelValue,
              'is-warn': option.warn,
            }"
            :disabled="option.disabled"
            role="option"
            :aria-selected="option.value === modelValue"
            @pointerenter="activeIndex = index"
            @click="pick(index)"
          >
            <span class="sel-option-text">
              <span class="sel-option-label">{{ option.label }}</span>
              <span v-if="option.hint" class="sel-option-hint" :class="{ warn: option.warn }">
                {{ option.hint }}
              </span>
            </span>
            <svg
              v-if="option.value === modelValue"
              class="sel-check"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="2.4"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <path d="m5 12.5 4.5 4.5L19 7" />
            </svg>
          </button>
        </div>

        <div v-if="hasAction" class="sel-foot">
          <div class="sel-sep" role="separator" />
          <button
            type="button"
            class="sel-option sel-action"
            :class="{ active: activeIndex === actionIndex }"
            @pointerenter="activeIndex = actionIndex"
            @click="pick(actionIndex)"
          >
            <svg
              class="sel-plus"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="2.2"
              stroke-linecap="round"
              aria-hidden="true"
            >
              <path d="M12 5v14M5 12h14" />
            </svg>
            <span class="sel-option-label">{{ actionLabel }}</span>
          </button>
        </div>
      </div>
    </Teleport>
  </div>
</template>

<style scoped src="../styles/components/SelectMenu.css"></style>
