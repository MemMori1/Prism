<script setup lang="ts">
import { computed } from 'vue'
import type { TimelineRecord } from '../types'
import { renderMarkdown } from '../markdown'
import { formatCost, formatTokens } from '../format'
import ToolCard from './ToolCard.vue'

const props = defineProps<{
  record: TimelineRecord
  sessionId: string
  /** 搜索定位命中的消息：短暂高亮。 */
  highlighted?: boolean
}>()

/** 该轮 token（引擎数据可得时）。 */
const tokens = computed(() => props.record.tokens ?? null)

const hasTokens = computed(() => {
  const current = tokens.value
  if (!current) return false
  return Boolean(
    current.input ||
      current.cacheRead ||
      current.cacheWrite ||
      current.output ||
      current.reasoning ||
      current.cost,
  )
})

/** 上下文用量（窗口已知且用量为正时显示）。 */
const context = computed(() => {
  const current = tokens.value
  if (!current?.contextWindow || current.contextWindow <= 0) return null
  const used = current.input + current.cacheRead + current.output
  if (used <= 0) return null
  return {
    used,
    percent: Math.min(100, Math.round((used / current.contextWindow) * 100)),
  }
})
</script>

<template>
  <div
    class="record"
    :class="[`role-${record.role}`, { highlighted }]"
    :data-message-id="record.id"
  >
    <!-- 用户消息：气泡 -->
    <template v-if="record.role === 'user'">
      <div class="user-bubble">
        <template v-for="(part, index) in record.parts" :key="index">
          <div
            v-if="part.kind === 'text'"
            class="user-text md"
            v-html="renderMarkdown(part.text)"
          />
        </template>
      </div>
    </template>

    <!-- 系统提示：居中弱化 -->
    <template v-else-if="record.role === 'system'">
      <div class="system-note">
        <span v-for="(part, index) in record.parts" :key="index">
          {{ part.kind === 'text' ? part.text : '' }}
        </span>
      </div>
    </template>

    <!-- 助手消息：步骤序列 -->
    <template v-else>
      <div class="assistant-block">
        <div v-for="(part, index) in record.parts" :key="index" class="part">
          <details v-if="part.kind === 'reasoning'" class="reasoning">
            <summary>思考过程</summary>
            <div class="reasoning-body">{{ part.text }}</div>
          </details>
          <ToolCard
            v-else-if="part.kind === 'tool'"
            :part="part"
            :session-id="sessionId"
            :message-id="record.id"
          />
          <div
            v-else-if="part.kind === 'text' && part.text.trim()"
            class="assistant-text md"
            v-html="renderMarkdown(part.text)"
          />
        </div>

        <div v-if="tokens && hasTokens" class="turn-tokens">
          <span class="tt-item" title="未缓存输入">↑ {{ formatTokens(tokens.input) }}</span>
          <span v-if="tokens.cacheRead" class="tt-item" title="缓存读">
            ⚡ {{ formatTokens(tokens.cacheRead) }}
          </span>
          <span class="tt-item" title="输出">↓ {{ formatTokens(tokens.output) }}</span>
          <span v-if="tokens.reasoning" class="tt-item" title="思考">
            ✦ {{ formatTokens(tokens.reasoning) }}
          </span>
          <span v-if="tokens.cost" class="tt-item tt-cost" title="该轮成本">
            {{ formatCost(tokens.cost) }}
          </span>
          <span
            v-if="context"
            class="tt-ctx"
            :title="`上下文 ≈ ${formatTokens(context.used)} / 窗口 ${formatTokens(tokens.contextWindow ?? 0)} tokens`"
          >
            <span class="tt-ctx-bar">
              <span class="tt-ctx-fill" :style="{ width: `${context.percent}%` }" />
            </span>
            {{ context.percent }}%
          </span>
        </div>
      </div>
    </template>
  </div>
</template>

<style scoped>
.record {
  display: flex;
  flex-direction: column;
}

/* 搜索结果定位：目标消息短暂高亮。 */
.record.highlighted {
  outline: 2px solid color-mix(in srgb, var(--accent) 45%, transparent);
  outline-offset: 5px;
  border-radius: var(--radius);
  background: color-mix(in srgb, var(--accent) 7%, transparent);
}

.role-user {
  align-items: flex-end;
}

.user-bubble {
  max-width: 78%;
  background: var(--accent-soft);
  color: var(--text);
  border: 1px solid color-mix(in srgb, var(--accent) 18%, transparent);
  border-radius: var(--radius-lg);
  border-bottom-right-radius: 4px;
  padding: 8px 13px;
}

.system-note {
  align-self: center;
  color: var(--faint);
  font-size: 11.5px;
  padding: 2px 0;
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

/* 每轮 token 四联 + 上下文用量条 */
.turn-tokens {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 10px;
  margin-top: 2px;
  font-size: 10.5px;
  color: var(--faint);
  user-select: none;
}

.tt-item {
  white-space: nowrap;
}

.tt-cost {
  color: var(--muted);
}

.tt-ctx {
  display: inline-flex;
  align-items: center;
  gap: 5px;
}

.tt-ctx-bar {
  display: inline-block;
  width: 54px;
  height: 4px;
  border-radius: 2px;
  background: var(--border);
  overflow: hidden;
}

.tt-ctx-fill {
  display: block;
  height: 100%;
  background: var(--accent);
  border-radius: 2px;
}
</style>
