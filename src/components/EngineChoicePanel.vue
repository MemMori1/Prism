<script setup lang="ts">
import type { EngineChoice } from '../sessionStart'

const props = withDefaults(
  defineProps<{
    choices: EngineChoice[]
    title?: string
    description?: string
    selecting?: string | null
    hint?: string | null
    error?: string | null
  }>(),
  {
    title: '选择本次任务使用的 Agent',
    description: '工作目录已经准备好。选择一个 Agent 后，就可以输入第一条消息。',
    selecting: null,
    hint: null,
    error: null,
  },
)

const emit = defineEmits<{
  (e: 'select', engine: string): void
}>()

function pick(choice: EngineChoice) {
  if (!choice.available || choice.checking || props.selecting) return
  emit('select', choice.id)
}
</script>

<template>
  <section class="engine-picker" aria-label="选择本次任务使用的 Agent">
    <div class="engine-picker-inner">
      <div class="picker-mark" aria-hidden="true">
        <svg viewBox="0 0 24 24" width="18" height="18">
          <path
            d="M13.4 3.2c.5 3.9 2.2 5.6 6.1 6.1-3.9.5-5.6 2.2-6.1 6.1-.5-3.9-2.2-5.6-6.1-6.1 3.9-.5 5.6-2.2 6.1-6.1Z"
            fill="none"
            stroke="currentColor"
            stroke-width="1.6"
            stroke-linejoin="round"
          />
          <path
            d="M6.4 14.6c.3 2.2 1.3 3.2 3.5 3.5-2.2.3-3.2 1.3-3.5 3.5-.3-2.2-1.3-3.2-3.5-3.5 2.2-.3 3.2-1.3 3.5-3.5Z"
            fill="none"
            stroke="currentColor"
            stroke-width="1.4"
            stroke-linejoin="round"
          />
        </svg>
      </div>

      <h2>{{ title }}</h2>
      <p class="picker-desc">{{ description }}</p>

      <div class="engine-grid">
        <button
          v-for="choice in choices"
          :key="choice.id"
          class="engine-card"
          :class="[
            `accent-${choice.accent}`,
            {
              unavailable: !choice.available && !choice.checking,
              choosing: selecting === choice.id,
            },
          ]"
          :disabled="!choice.available || choice.checking || !!selecting"
          :aria-label="choice.label"
          @click="pick(choice)"
        >
          <span class="engine-icon" :class="`icon-${choice.icon}`" aria-hidden="true">
            <span v-if="choice.icon === 'claude'" class="ai-glyph">AI</span>
            <svg
              v-else-if="choice.icon === 'prism'"
              viewBox="0 0 24 24"
              width="19"
              height="19"
            >
              <path
                d="M12 3.4 20.6 12 12 20.6 3.4 12 12 3.4Z"
                fill="none"
                stroke="currentColor"
                stroke-width="1.5"
                stroke-linejoin="round"
              />
              <path
                d="M12 8.1 15.9 12 12 15.9 8.1 12 12 8.1Z"
                fill="currentColor"
                opacity="0.55"
              />
            </svg>
            <svg v-else-if="choice.icon === 'codex'" viewBox="0 0 24 24" width="19" height="19">
              <path
                d="M8.1 4.9a4.1 4.1 0 0 1 6.5 1.3 4.1 4.1 0 0 1 3.1 6 4.1 4.1 0 0 1-5 5 4.1 4.1 0 0 1-6.5-1.3 4.1 4.1 0 0 1-3.1-6 4.1 4.1 0 0 1 5-5Z"
                fill="none"
                stroke="currentColor"
                stroke-width="1.5"
                stroke-linejoin="round"
              />
              <path
                d="M9.2 9.2 12 7.7l2.8 1.5v3.1L12 13.8l-2.8-1.5V9.2Z"
                fill="none"
                stroke="currentColor"
                stroke-width="1.1"
                stroke-linejoin="round"
              />
            </svg>
            <span v-else class="terminal-glyph"><span>&gt;</span><span>_</span></span>
          </span>

          <span class="engine-main">
            <span class="engine-name">{{ choice.label }}</span>
            <span class="engine-desc">{{ choice.description }}</span>
            <span v-if="choice.checking" class="engine-state">检测中…</span>
            <span v-else-if="choice.needsSetup" class="engine-state">未配置 · 点开填 API Key</span>
            <span v-else-if="!choice.available" class="engine-state">当前不可用</span>
            <span v-else-if="selecting === choice.id" class="engine-state working">正在创建…</span>
          </span>

          <span class="engine-arrow" aria-hidden="true">→</span>
        </button>
      </div>

      <p v-if="hint" class="picker-hint">{{ hint }}</p>
      <p v-if="error" class="picker-error" role="alert">{{ error }}</p>
    </div>
  </section>
</template>

<style scoped src="../styles/components/EngineChoicePanel.css"></style>
