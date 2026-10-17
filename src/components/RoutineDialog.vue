<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { api } from '../api'
import { SCHEDULE_PRESETS, describeCron } from '../routines'
import { ASSET_ENGINE_LABELS } from '../workshop'
import type { RoutineDefinition } from '../types'

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

function applyPreset() {
  if (preset.value !== 'custom') {
    schedule.value = preset.value
  }
}

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

        <label class="field">
          <span class="field-label">计划</span>
          <div class="schedule-row">
            <select v-model="preset" class="field-input preset-select" @change="applyPreset">
              <option v-for="option in SCHEDULE_PRESETS" :key="option.cron" :value="option.cron">
                {{ option.label }}
              </option>
              <option value="custom">自定义（cron）</option>
            </select>
            <input
              v-model="schedule"
              class="field-input mono-input"
              placeholder="分 时 日 月 周，如 0 9 * * 1-5"
              @input="preset = 'custom'"
            />
          </div>
          <span class="field-hint">当前：{{ scheduleHint }}</span>
        </label>

        <label class="field">
          <span class="field-label">引擎</span>
          <select v-model="engine" class="field-input">
            <option v-for="(label, id) in ASSET_ENGINE_LABELS" :key="id" :value="id">
              {{ label }}
            </option>
          </select>
          <span class="field-hint">任务在 Prism 例行工作区（~/.prism/routine-workspace）里运行。</span>
        </label>

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

<style scoped>
.dialog-overlay {
  position: fixed;
  inset: 0;
  z-index: 40;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgb(18 16 34 / 0.35);
}

.dialog-panel {
  width: 520px;
  max-width: calc(100vw - 60px);
  max-height: calc(100vh - 80px);
  display: flex;
  flex-direction: column;
  border: 1px solid var(--border);
  border-radius: var(--radius-lg);
  background: var(--panel);
  box-shadow: var(--shadow);
}

.dialog-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 12px 16px;
  border-bottom: 1px solid var(--border);
}

.dialog-title {
  font-size: 13px;
  font-weight: 650;
  color: var(--text);
}

.close-btn {
  padding: 2px 8px;
  border-radius: var(--radius-sm);
  color: var(--muted);
  transition: all 0.15s ease;
}

.close-btn:hover {
  color: var(--text);
  background: var(--panel-2);
}

.dialog-body {
  display: flex;
  flex-direction: column;
  gap: 14px;
  padding: 16px;
  overflow-y: auto;
}

.field {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.field-label {
  font-size: 12px;
  font-weight: 600;
  color: var(--muted);
}

.field-input {
  padding: 6px 10px;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--panel-2);
  color: var(--text);
  font-size: 12.5px;
  font-family: inherit;
}

.field-input:focus {
  outline: none;
  border-color: var(--accent);
}

.schedule-row {
  display: flex;
  gap: 8px;
}

.preset-select {
  flex: none;
  width: 168px;
}

.mono-input {
  flex: 1;
  min-width: 0;
  font-family: var(--font-mono);
  font-size: 12px;
}

.field-hint {
  font-size: 11.5px;
  color: var(--faint);
}

.prompt-area {
  resize: vertical;
  min-height: 96px;
  line-height: 1.6;
}

.dialog-foot {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  padding: 12px 16px;
  border-top: 1px solid var(--border);
}

.mini-btn {
  padding: 4px 14px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--border);
  background: var(--panel-2);
  color: var(--muted);
  font-size: 12px;
  transition: all 0.15s ease;
}

.mini-btn:hover {
  color: var(--text);
  border-color: var(--border-strong);
}

.primary-btn {
  padding: 4px 16px;
  border-radius: var(--radius-sm);
  background: var(--accent);
  color: #fff;
  font-size: 12px;
  font-weight: 600;
  transition: background 0.15s ease;
}

.primary-btn:hover:not(:disabled) {
  background: var(--accent-strong);
}

.primary-btn:disabled {
  opacity: 0.5;
  cursor: default;
}
</style>
