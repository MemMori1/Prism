<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { api } from '../api'
import { SCHEDULE_PRESETS, describeCron } from '../routines'
import { ASSET_ENGINE_LABELS } from '../workshop'
import type { RoutineDefinition } from '../types'
import SelectMenu from './SelectMenu.vue'

const props = defineProps<{
  mode: 'create' | 'edit'
  routine: RoutineDefinition | null
}>()

const emit = defineEmits<{
  (e: 'close'): void
  (e: 'saved', id: string): void
  (e: 'error', message: string): void
}>()

const name = ref(props.routine?.name ?? '')
const schedule = ref(props.routine?.schedule ?? '0 9 * * *')
const prompt = ref(props.routine?.prompt ?? '')
const engine = ref(props.routine?.engine ?? 'opencode')
const saving = ref(false)

const preset = ref(
  SCHEDULE_PRESETS.some(option => option.cron === schedule.value.trim())
    ? schedule.value.trim()
    : 'custom',
)

function applyPreset(value: string) {
  preset.value = value
  if (value !== 'custom') {
    schedule.value = value
  }
}

/** 计划预设 / 引擎 下拉选项。 */
const presetOptions = computed(() => [
  ...SCHEDULE_PRESETS.map(option => ({ value: option.cron, label: option.label })),
  { value: 'custom', label: '自定义（cron）' },
])
const engineOptions = computed(() =>
  Object.entries(ASSET_ENGINE_LABELS).map(([value, label]) => ({ value, label })),
)

const scheduleHint = computed(() => {
  const text = schedule.value.trim()
  if (text.split(/\s+/).length !== 5) return '需要 5 段：分 时 日 月 周'
  return describeCron(text)
})

const canSave = computed(
  () =>
    !!name.value.trim() &&
    !!prompt.value.trim() &&
    schedule.value.trim().split(/\s+/).length === 5 &&
    !saving.value,
)

const title = computed(() => (props.mode === 'create' ? '新建定时任务' : '编辑定时任务'))

async function save() {
  if (!canSave.value) return
  saving.value = true
  try {
    if (props.mode === 'create') {
      const created = await api.routineCreate(
        name.value.trim(),
        schedule.value.trim(),
        prompt.value.trim(),
        engine.value,
      )
      emit('saved', created.id)
    } else if (props.routine) {
      const updated = await api.routineUpdate(props.routine.id, {
        name: name.value.trim(),
        schedule: schedule.value.trim(),
        prompt: prompt.value.trim(),
        engine: engine.value,
      })
      emit('saved', updated.id)
    }
  } catch (cause) {
    emit('error', cause instanceof Error ? cause.message : String(cause))
  } finally {
    saving.value = false
  }
}

function onKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape') emit('close')
}

onMounted(() => {
  window.addEventListener('keydown', onKeydown)
})

onBeforeUnmount(() => {
  window.removeEventListener('keydown', onKeydown)
})
</script>

<template>
  <div class="dialog-overlay" @click.self="emit('close')">
    <div class="dialog-panel" role="dialog" aria-label="定时任务">
      <header class="dialog-head">
        <span class="dialog-title">{{ title }}</span>
        <button class="close-btn" title="关闭（Esc）" @click="emit('close')">✕</button>
      </header>

      <div class="dialog-body">
        <label class="field">
          <span class="field-label">名称</span>
          <input v-model="name" class="field-input" placeholder="例如：每日站会摘要" />
        </label>

        <div class="field">
          <span class="field-label">计划</span>
          <div class="schedule-row">
            <SelectMenu
              v-model="preset"
              :options="presetOptions"
              class="preset-select"
              title="计划预设"
              @update:model-value="applyPreset"
            />
            <input
              v-model="schedule"
              class="field-input mono-input"
              placeholder="分 时 日 月 周，如 0 9 * * 1-5"
              @input="preset = 'custom'"
            />
          </div>
          <span class="field-hint">当前：{{ scheduleHint }}</span>
        </div>

        <div class="field">
          <span class="field-label">引擎</span>
          <SelectMenu v-model="engine" :options="engineOptions" title="引擎" />
          <span class="field-hint">任务在 Prism 例行工作区（~/.prism/routine-workspace）里运行。</span>
        </div>

        <label class="field">
          <span class="field-label">提示词</span>
          <textarea
            v-model="prompt"
            class="field-input prompt-area"
            rows="6"
            placeholder="到点发给引擎的内容，例如：汇总昨天的工作进展并给出今天的三条建议。"
          />
        </label>
      </div>

      <footer class="dialog-foot">
        <button class="mini-btn" @click="emit('close')">取消</button>
        <button class="primary-btn" :disabled="!canSave" @click="save">
          {{ saving ? '保存中…' : '保存' }}
        </button>
      </footer>
    </div>
  </div>
</template>

<style scoped src="../styles/components/RoutineDialog.css"></style>
