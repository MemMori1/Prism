<script setup lang="ts">
import { computed } from 'vue'
import type { TimelinePart } from '../types'

const props = defineProps<{
  part: Extract<TimelinePart, { kind: 'tool' }>
}>()

const statusInfo = computed(() => {
  switch (props.part.status) {
    case 'completed':
      return { label: '完成', tone: 'ok' as const }
    case 'error':
    case 'failed':
      return { label: '失败', tone: 'error' as const }
    case 'pending':
    case 'running':
    case 'in_progress':
      return { label: '进行中', tone: 'warn' as const }
    default:
      return props.part.status
        ? { label: props.part.status, tone: 'muted' as const }
        : null
  }
})

/** 常见工具给一个小图标字符，未知工具用通用符号。 */
const toolGlyph = computed(() => {
  const name = props.part.name.toLowerCase()
  if (name.includes('bash') || name.includes('shell') || name.includes('exec')) return '❯'
  if (name.includes('read')) return '◉'
  if (name.includes('edit') || name.includes('write') || name.includes('patch')) return '✎'
  if (name.includes('grep') || name.includes('search') || name.includes('glob')) return '⌕'
  if (name.includes('fetch') || name.includes('http') || name.includes('web')) return '⇄'
  if (name.includes('task') || name.includes('agent')) return '⚙'
  return '▸'
})
</script>

<template>
  <div class="tool-card">
    <div class="tool-head">
      <span class="tool-glyph">{{ toolGlyph }}</span>
      <span class="tool-name">{{ part.name }}</span>
      <span v-if="statusInfo" class="tool-status" :class="`tone-${statusInfo.tone}`">
        {{ statusInfo.label }}
      </span>
    </div>
    <div v-if="part.summary" class="tool-summary" :title="part.summary">
      {{ part.summary }}
    </div>
  </div>
</template>

<style scoped>
.tool-card {
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--panel-2);
  padding: 7px 10px 8px;
  max-width: 720px;
}

.tool-head {
  display: flex;
  align-items: center;
  gap: 7px;
}

.tool-glyph {
  color: var(--accent);
  font-family: var(--font-mono);
  font-size: 12px;
  width: 14px;
  text-align: center;
  flex: none;
}

.tool-name {
  font-family: var(--font-mono);
  font-size: 11.5px;
  font-weight: 600;
  color: var(--text);
}

.tool-status {
  margin-left: auto;
  font-size: 10.5px;
  padding: 0 7px;
  border-radius: 999px;
  line-height: 16px;
  flex: none;
}

.tool-status.tone-ok {
  color: var(--green);
  background: var(--green-soft);
}

.tool-status.tone-error {
  color: var(--red);
  background: var(--red-soft);
}

.tool-status.tone-warn {
  color: var(--amber);
  background: var(--amber-soft);
}

.tool-status.tone-muted {
  color: var(--muted);
  background: var(--panel);
  border: 1px solid var(--border);
}

.tool-summary {
  margin-top: 4px;
  font-family: var(--font-mono);
  font-size: 11px;
  color: var(--muted);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
</style>
