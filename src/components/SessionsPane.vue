<script setup lang="ts">
import type { ProjectItem, SessionSummary } from '../types'
import { basename, formatTokens, relativeTime } from '../format'

defineProps<{
  sessions: SessionSummary[]
  selectedId: string | null
  loading: boolean
  project: ProjectItem | null
}>()

const emit = defineEmits<{
  (e: 'select', sessionId: string): void
}>()

function shortModel(model: string | null): string {
  if (!model) return ''
  const index = model.indexOf('/')
  const name = index === -1 ? model : model.slice(index + 1)
  return name.length > 22 ? `${name.slice(0, 22)}…` : name
}
</script>

<template>
  <aside class="pane sessions">
    <div class="pane-header">
      <span class="pane-title">
        {{ project ? `${project.name || basename(project.worktree)} 的会话` : '会话' }}
      </span>
      <span v-if="sessions.length" class="pane-count">{{ sessions.length }}</span>
    </div>

    <div class="pane-body">
      <div v-if="!project" class="pane-empty">从左侧选择一个项目</div>
      <div v-else-if="loading && !sessions.length" class="pane-empty">加载中…</div>
      <div v-else-if="!sessions.length" class="pane-empty">这个项目还没有会话</div>

      <button
        v-for="session in sessions"
        :key="session.id"
        class="session-item"
        :class="{ active: session.id === selectedId }"
        :title="session.title ?? session.id"
        @click="emit('select', session.id)"
      >
        <span class="session-title">{{ session.title || '未命名会话' }}</span>
        <span class="session-sub">
          <span>{{ relativeTime(session.timeUpdated) }}</span>
          <span v-if="session.model" class="session-model">{{ shortModel(session.model) }}</span>
          <span class="session-tokens">
            ↓{{ formatTokens(session.tokensInput + session.tokensReasoning) }}
            ↑{{ formatTokens(session.tokensOutput) }}
          </span>
        </span>
      </button>
    </div>
  </aside>
</template>

<style scoped>
.pane {
  display: flex;
  flex-direction: column;
  min-height: 0;
  border-right: 1px solid var(--border);
  background: var(--panel);
}

.sessions {
  width: 264px;
  flex: none;
}

.pane-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 10px 14px 6px;
  flex: none;
}

.pane-title {
  font-size: 11px;
  font-weight: 650;
  letter-spacing: 0.4px;
  color: var(--faint);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.pane-count {
  font-size: 11px;
  color: var(--faint);
}

.pane-body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: 4px 8px 10px;
}

.pane-empty {
  padding: 18px 10px;
  color: var(--faint);
  font-size: 12px;
}

.session-item {
  display: flex;
  flex-direction: column;
  gap: 2px;
  width: 100%;
  text-align: left;
  padding: 7px 9px;
  border-radius: var(--radius-sm);
  transition: background 0.12s ease;
}

.session-item:hover {
  background: var(--panel-2);
}

.session-item.active {
  background: var(--accent-soft);
}

.session-title {
  font-size: 12.5px;
  font-weight: 540;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.session-item.active .session-title {
  color: var(--accent-text);
}

.session-sub {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 10.5px;
  color: var(--faint);
  overflow: hidden;
  white-space: nowrap;
}

.session-model {
  font-family: var(--font-mono);
  overflow: hidden;
  text-overflow: ellipsis;
}

.session-tokens {
  margin-left: auto;
  font-family: var(--font-mono);
  flex: none;
}
</style>
