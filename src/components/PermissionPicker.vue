<script setup lang="ts">
/**
 * 权限档位选择器（Codex 那种「审批模式」）。
 *
 * 放在输入框工具行、git 分支右边：一眼能看到「现在这个会话允许它做什么」。
 * 档位只是开关，真正拦人的是后端闸门——发消息时把档位一起带过去。
 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue'
import { APPROVAL_CHOICES, approvalChoice, type ApprovalMode } from '../approval'

const props = withDefaults(
  defineProps<{
    modelValue: ApprovalMode
    disabled?: boolean
  }>(),
  { disabled: false },
)

const emit = defineEmits<{
  (e: 'update:modelValue', value: ApprovalMode): void
}>()

const open = ref(false)
const trigger = ref<HTMLButtonElement | null>(null)
const popup = ref<HTMLElement | null>(null)
const pos = ref({ left: 0, top: 0, bottom: null as number | null, width: 300 })

const current = computed(() => approvalChoice(props.modelValue))

const popupStyle = computed(() => {
  const style: Record<string, string> = {
    left: `${pos.value.left}px`,
    width: `${pos.value.width}px`,
  }
  if (pos.value.bottom != null) style.bottom = `${pos.value.bottom}px`
  else style.top = `${pos.value.top}px`
  return style
})

function measure(): void {
  const el = trigger.value
  if (!el) return
  const rect = el.getBoundingClientRect()
  const margin = 8
  const gap = 6
  const width = Math.min(300, window.innerWidth - margin * 2)
  const below = window.innerHeight - rect.bottom - margin
  const above = rect.top - margin
  const flip = below < 220 && above > below
  pos.value = {
    left: Math.min(Math.max(margin, rect.left), window.innerWidth - width - margin),
    width,
    top: rect.bottom + gap,
    bottom: flip ? window.innerHeight - rect.top + gap : null,
  }
}

async function openMenu(): Promise<void> {
  if (open.value || props.disabled) return
  measure()
  open.value = true
  await nextTick()
  measure()
}

function closeMenu(refocus = false): void {
  if (!open.value) return
  open.value = false
  if (refocus) trigger.value?.focus()
}

function pick(mode: ApprovalMode): void {
  if (mode !== props.modelValue) emit('update:modelValue', mode)
  closeMenu(true)
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
  if (event.key === 'Escape') {
    event.preventDefault()
    closeMenu(true)
  }
}

function onViewportChange(): void {
  closeMenu()
}

onMounted(() => {
  window.addEventListener('pointerdown', onPointerDown, true)
  window.addEventListener('keydown', onKeydown)
  window.addEventListener('resize', onViewportChange)
})

onBeforeUnmount(() => {
  window.removeEventListener('pointerdown', onPointerDown, true)
  window.removeEventListener('keydown', onKeydown)
  window.removeEventListener('resize', onViewportChange)
})
</script>

<template>
  <span class="perm-pick">
    <button
      ref="trigger"
      type="button"
      class="perm-chip"
      :class="[`mode-${modelValue}`, { on: open }]"
      :disabled="disabled"
      :title="`权限：${current.label} —— ${current.hint}`"
      aria-haspopup="dialog"
      :aria-expanded="open"
      @click.stop="open ? closeMenu() : void openMenu()"
    >
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="1.8"
        stroke-linecap="round"
        stroke-linejoin="round"
        aria-hidden="true"
      >
        <path d="M12 3.5 5.5 6v5.5c0 3.9 2.6 7.4 6.5 9 3.9-1.6 6.5-5.1 6.5-9V6L12 3.5Z" />
        <path v-if="modelValue === 'ask'" d="M12 8.5v3.2" />
        <path v-if="modelValue === 'ask'" d="M12 14.6v.1" />
        <path v-else-if="modelValue === 'read-only'" d="M9.5 12h5" />
        <path v-else d="m9.5 12 1.8 1.8 3.4-3.6" />
      </svg>
      <span class="perm-label">{{ current.short }}</span>
    </button>

    <Teleport to="body">
      <div v-if="open" ref="popup" class="perm-pop" :style="popupStyle" role="dialog" aria-label="权限档位">
        <div class="perm-head">
          <span class="perm-title">权限</span>
          <span class="perm-sub">这个会话允许 Agent 做什么</span>
        </div>
        <button
          v-for="item in APPROVAL_CHOICES"
          :key="item.id"
          type="button"
          class="perm-item"
          :class="{ current: item.id === modelValue }"
          @click="pick(item.id)"
        >
          <span class="perm-item-main">
            <span class="perm-item-label">{{ item.label }}</span>
            <span class="perm-item-hint">{{ item.hint }}</span>
          </span>
          <svg
            v-if="item.id === modelValue"
            class="perm-check"
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
        <p class="perm-foot">只作用于 Prism 内置引擎；另外三个引擎的权限在各自应用里管。</p>
      </div>
    </Teleport>
  </span>
</template>

<style scoped src="../styles/components/PermissionPicker.css"></style>
