<script setup lang="ts">
import type { LiveToolPart, LiveTurn } from '../liveTurn'
import type { ToolDetail } from '../types'
import { renderMarkdown } from '../markdown'
import ToolCard from './ToolCard.vue'

defineProps<{ turn: LiveTurn }>()

function liveDetail(part: LiveToolPart): ToolDetail {
  return {
    input: (part.input as Record<string, unknown> | null) ?? null,
    output: part.output,
    error: part.error,
    metadata: (part.metadata as Record<string, unknown> | null) ?? null,
    truncated: false,
  }
}
</script>

<template>
  <div class="record role-assistant live">
    <div class="assistant-block">
      <div v-if="turn.interrupted" class="live-note">已中断</div>
      <div v-if="turn.running && !turn.parts.length" class="live-note waiting">
        <span class="pulse" />等待模型响应…
      </div>

      <div v-for="part in turn.parts" :key="part.key" class="part">
        <details v-if="part.kind === 'reasoning'" class="reasoning" :open="turn.running">
          <summary>思考过程</summary>
          <div class="reasoning-body">{{ part.text }}</div>
        </details>
        <ToolCard
          v-else-if="part.kind === 'tool'"
          :part="part"
          session-id=""
          message-id=""
          :live-detail="liveDetail(part)"
        />
        <div
          v-else-if="part.kind === 'text' && part.text.trim()"
          class="assistant-text md"
          v-html="renderMarkdown(part.text)"
        />
      </div>

      <div v-if="turn.error" class="live-error">{{ turn.error }}</div>
    </div>
  </div>
</template>

<style scoped>
.record {
  display: flex;
  flex-direction: column;
}

.assistant-block {
  display: flex;
  flex-direction: column;
  gap: 8px;
  max-width: 860px;
}

.part {
  display: flex;
  flex-direction: column;
}

.live-note {
  color: var(--faint);
  font-size: 11.5px;
}

.live-note.waiting {
  display: flex;
  align-items: center;
  gap: 7px;
}

.pulse {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--accent);
  animation: pulse 1.1s ease-in-out infinite;
}

@keyframes pulse {
  0%,
  100% {
    opacity: 0.25;
    transform: scale(0.85);
  }
  50% {
    opacity: 1;
    transform: scale(1.1);
  }
}

.live-error {
  border: 1px solid color-mix(in srgb, var(--red) 35%, transparent);
  background: var(--red-soft);
  color: var(--red);
  border-radius: 6px;
  padding: 6px 10px;
  font-size: 12px;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  max-width: 780px;
}

.reasoning {
  border: 1px dashed var(--border-strong);
  border-radius: var(--radius-sm);
  background: transparent;
  max-width: 720px;
}

.reasoning summary {
  cursor: pointer;
  user-select: none;
  list-style: none;
  padding: 4px 10px;
  font-size: 11px;
  color: var(--faint);
}

.reasoning summary::-webkit-details-marker {
  display: none;
}

.reasoning summary::before {
  content: '▸ ';
}

.reasoning[open] summary::before {
  content: '▾ ';
}

.reasoning-body {
  padding: 2px 12px 10px;
  font-size: 12px;
  color: var(--muted);
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}
</style>
