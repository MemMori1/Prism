<script setup lang="ts">
import { computed } from 'vue'
import type { LivePart, LiveToolPart, LiveTurn } from '../liveTurn'
import type { ToolDetail } from '../types'
import { renderMarkdown } from '../markdown'
import ToolCard from './ToolCard.vue'
import TurnMeta from './TurnMeta.vue'

const props = defineProps<{
  turn: LiveTurn
  /** 当前轮次的模型（live 还没有归档记录，由父组件给）。 */
  model?: string | null
  /** 开跑时刻：跑着的时候实时计时。 */
  runningSince?: number | null
  /** 还没吐字时的说明（默认「等待模型响应…」；压缩时改成压缩进度）。 */
  waitingNote?: string | null
}>()

/** 正在生成的正文也能先复制走。 */
const copyText = computed(() =>
  props.turn.parts
    .filter((part): part is Extract<LivePart, { kind: 'text' }> => part.kind === 'text')
    .map(part => part.text.trim())
    .filter(Boolean)
    .join('\n\n'),
)

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
        <span class="pulse" />{{ waitingNote || '等待模型响应…' }}
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

      <TurnMeta
        :model="model ?? null"
        :running-since="turn.running ? (runningSince ?? null) : null"
        :copy-text="copyText"
      />
    </div>
  </div>
</template>

<style scoped src="../styles/components/LiveTurn.css"></style>
