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

<style scoped src="../styles/components/LiveTurn.css"></style>
