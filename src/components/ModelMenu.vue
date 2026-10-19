<script setup lang="ts">
/**
 * 模型 + 推理档位的合并选择器：一个触发器（`模型 · 档位`），弹层上半是模型列表、
 * 下半是当前模型的推理强度（chip）。比并排两个下拉更省横向空间，也让"档位属于模型"
 * 这层关系显式可见。
 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { modelKeyLabel } from '../composer'
import ModelIcon from './ModelIcon.vue'

interface ModelOption {
  value: string
  /** 弹层里的完整名字（含 provider / 默认标记） */
  label: string
  /** 触发器上的短名字，默认取 label */
  short?: string
}

const props = withDefaults(
  defineProps<{
    modelValue: string
    variant: string
    /** 模型选项，含 `{ value: '', label: '默认' }` 这一项 */
    options: ModelOption[]
    /** 当前模型可选的推理档位（空 = 该模型没有档位可选） */
    variants: string[]
    loading?: boolean
    disabled?: boolean
    title?: string
    maxHeight?: number
  }>(),
  { loading: false, disabled: false, title: '', maxHeight: 300 },
)

const emit = defineEmits<{
  (e: 'update:modelValue', value: string): void
  (e: 'update:variant', value: string): void
  (e: 'open'): void
}>()

const open = ref(false)
const trigger = ref<HTMLButtonElement | null>(null)
const popup = ref<HTMLElement | null>(null)
const list = ref<HTMLElement | null>(null)
const activeIndex = ref(-1)

/** 弹层 fixed 定位：top/bottom 二选一，避免被抽屉 / 弹框的 overflow 裁掉。 */
const pos = ref({ left: 0, top: 0, bottom: null as number | null, width: 280, maxHeight: 300 })

/** 模型列表区高度上限（chips 区固定在底部，不参与滚动）。 */
const FOOT_HEIGHT = 66
const MIN_WIDTH = 260

/** 触发器上的小图标：按模型所属服务挑一个（没选具体模型就不画）。 */
const selectedIndex = computed(() => props.options.findIndex(item => item.value === props.modelValue))
const selectedLabel = computed(() => {
  const found = props.options.find(item => item.value === props.modelValue)
  if (found) return found.short ?? found.label
  if (!props.modelValue) return props.loading ? '加载中…' : '默认'
  return modelKeyLabel(props.modelValue)
})
/** 触发器文案：模型 + 档位（档位是当前会话真实在用的值，不是选项默认值）。 */
const currentLabel = computed(() =>
  props.variant ? `${selectedLabel.value} · ${props.variant}` : selectedLabel.value,
)

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

const listStyle = computed(() => {
  const room = pos.value.maxHeight - FOOT_HEIGHT
  return { maxHeight: `${Math.max(96, room)}px` }
})

/** 目录异步到达后列表变长：重算可用高度，避免弹层被窗口底边截断。 */
watch(
  () => props.options.length,
  () => {
    if (open.value) measure(estimateHeight())
  },
)

function measure(estimate: number): void {
  const el = trigger.value
  if (!el) return
  const rect = el.getBoundingClientRect()
  const margin = 8
  const gap = 6
  const width = Math.min(
    Math.max(rect.width, MIN_WIDTH),
    window.innerWidth - margin * 2,
  )
  const below = window.innerHeight - rect.bottom - margin
  const above = rect.top - margin
  const flip = below < estimate && above > below
  const room = Math.max(140, (flip ? above : below) - gap)
  const maxHeight = Math.min(props.maxHeight, room)
  const left = Math.min(Math.max(margin, rect.left), window.innerWidth - width - margin)
  pos.value = {
    left,
    width,
    maxHeight,
    top: rect.bottom + gap,
    bottom: flip ? window.innerHeight - rect.top + gap : null,
  }
}

function estimateHeight(): number {
  return Math.min(props.maxHeight, props.options.length * 28 + FOOT_HEIGHT + 12)
}

function scrollActiveIntoView(): void {
  const node = list.value?.querySelectorAll<HTMLElement>('.sel-option')[activeIndex.value]
  node?.scrollIntoView({ block: 'nearest' })
}

async function openMenu(): Promise<void> {
  if (props.disabled || open.value) return
  measure(estimateHeight())
  open.value = true
  activeIndex.value = selectedIndex.value >= 0 ? selectedIndex.value : 0
  emit('open')
  await nextTick()
  measure(estimateHeight())
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

/** 选模型：档位由父级按新模型的可选集合校正（这里只管模型与「默认」清空）。 */
function pickModel(index: number): void {
  const option = props.options[index]
  if (!option) return
  if (option.value !== props.modelValue) {
    emit('update:modelValue', option.value)
    // 回到「引擎默认」时档位没有归属，一并清掉。
    if (!option.value && props.variant) emit('update:variant', '')
  }
  closeMenu(true)
}

function pickVariant(value: string): void {
  if (value !== props.variant) emit('update:variant', value)
}

function cycleVariant(step: number): void {
  const choices = ['', ...props.variants]
  if (choices.length < 2) return
  const current = Math.max(0, choices.indexOf(props.variant))
  const next = (current + step + choices.length) % choices.length
  pickVariant(choices[next])
}

function move(step: number): void {
  const total = props.options.length
  if (!total) return
  activeIndex.value = (activeIndex.value + step + total) % total
  scrollActiveIntoView()
}

function onTriggerKeydown(event: KeyboardEvent): void {
  if (open.value) return
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp' || event.key === 'Enter' || event.key === ' ') {
    event.preventDefault()
    // 阻断冒泡：否则同一个按键会被 window 上的导航逻辑再处理一次。
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
    case 'ArrowRight':
      event.preventDefault()
      cycleVariant(1)
      break
    case 'ArrowLeft':
      event.preventDefault()
      cycleVariant(-1)
      break
    case 'Home':
      event.preventDefault()
      activeIndex.value = 0
      scrollActiveIntoView()
      break
    case 'End':
      event.preventDefault()
      activeIndex.value = props.options.length - 1
      scrollActiveIntoView()
      break
    case 'Enter':
    case ' ':
      event.preventDefault()
      pickModel(activeIndex.value)
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
  <div class="sel sel-xs sel-pill sel-field is-inline" :class="{ 'is-open': open, 'is-disabled': disabled }">
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
      <ModelIcon v-if="modelValue" :model="modelValue" :size="15" />
      <span class="sel-label">{{ currentLabel }}</span>
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
      <div v-if="open" ref="popup" class="sel-popup mm-popup" :style="popupStyle">
        <div ref="list" class="mm-list" role="listbox" :style="listStyle">
          <div class="mm-section">模型</div>
          <button
            v-for="(option, index) in options"
            :key="option.value || '__default__'"
            type="button"
            class="sel-option"
            :class="{ active: index === activeIndex, selected: option.value === modelValue }"
            role="option"
            :aria-selected="option.value === modelValue"
            @pointerenter="activeIndex = index"
            @click="pickModel(index)"
          >
            <ModelIcon v-if="option.value" class="mm-icon-sm" :model="option.value" :size="14" />
            <span class="sel-option-label">{{ option.label }}</span>
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

        <div class="mm-foot">
          <div class="mm-section">
            推理强度
            <span class="mm-hint">← →</span>
          </div>
          <div v-if="variants.length" class="mm-chips">
            <button
              type="button"
              class="mm-chip"
              :class="{ on: variant === '' }"
              @click="pickVariant('')"
            >
              默认
            </button>
            <button
              v-for="item in variants"
              :key="item"
              type="button"
              class="mm-chip"
              :class="{ on: variant === item }"
              @click="pickVariant(item)"
            >
              {{ item }}
            </button>
          </div>
          <div v-else class="mm-empty">
            {{ variant ? `${variant}（该模型未提供档位表）` : '该模型没有可调档位' }}
          </div>
        </div>
      </div>
    </Teleport>
  </div>
</template>

<style scoped src="../styles/components/SelectMenu.css"></style>
<style scoped src="../styles/components/ModelMenu.css"></style>
