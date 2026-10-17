<script setup lang="ts">
import { computed, ref } from 'vue'
import type { ProjectItem, SessionSummary } from '../types'
import { basename, formatTokens, relativeTime } from '../format'
import {
  filterSessionsByRange,
  rangeToSince,
  sortSessions,
  type SessionSort,
  type TimeRange,
} from '../sessionList'

const props = defineProps<{
  sessions: SessionSummary[]
  selectedId: string | null
  loading: boolean
  project: ProjectItem | null
  creating: boolean
}>()

const emit = defineEmits<{
  (e: 'select', sessionId: string): void
  (e: 'create'): void
}>()

const sortKey = ref<SessionSort>('recent')
const rangeKey = ref<TimeRange>('all')

/** 两个引擎都已接入运行轴：有项目即可新建会话。 */
const canCreate = computed(() => !!props.project)
const newButtonTitle = computed(() => {
  if (!props.project) return '先选择项目'
  return '在当前项目目录新建会话'
})

const visibleSessions = computed(() =>
  sortSessions(
    filterSessionsByRange(props.sessions, rangeToSince(rangeKey.value, Date.now())),
    sortKey.value,
  ),
)

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
      <span class="pane-actions">
        <span v-if="sessions.length" class="pane-count">{{ sessions.length }}</span>
        <button
          class="new-btn"
          :disabled="!project || creating || !canCreate"
          :title="newButtonTitle"
          @click="emit('create')"
        >
          {{ creating ? '…' : '＋' }}
        </button>
      </span>
    </div>

    <div v-if="sessions.length" class="list-toolbar">
      <select v-model="sortKey" class="mini-select" title="排序方式">
        <option value="recent">最近活动</option>
        <option value="created">创建时间</option>
        <option value="tokens">Token</option>
      </select>
      <select v-model="rangeKey" class="mini-select" title="时间范围">
        <option value="all">全部时间</option>
        <option value="today">今天</option>
        <option value="week">本周</option>
        <option value="month">本月</option>
      </select>
    </div>

    <div class="pane-body">
      <div v-if="!project" class="pane-empty">从左侧选择一个项目</div>
      <div v-else-if="loading && !sessions.length" class="pane-empty">加载中…</div>
      <div v-else-if="!sessions.length" class="pane-empty">这个项目还没有会话</div>
      <div v-else-if="!visibleSessions.length" class="pane-empty">该时间范围内没有会话</div>

      <button
        v-for="session in visibleSessions"
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

.pane-actions {
  display: flex;
  align-items: center;
  gap: 6px;
  flex: none;
}

.list-toolbar {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 0 14px 6px;
  flex: none;
}

.mini-select {
  height: 22px;
  padding: 0 4px;
  border: 1px solid var(--border);
  border-radius: 5px;
  background: var(--panel);
  color: var(--muted);
  font-size: 11px;
  cursor: pointer;
}

.mini-select:hover {
  border-color: var(--border-strong);
  color: var(--text);
}

.new-btn {
  width: 20px;
  height: 20px;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 5px;
  border: 1px solid var(--border);
  background: var(--panel-2);
  color: var(--muted);
  font-size: 12px;
  line-height: 1;
  transition: all 0.15s ease;
}

.new-btn:hover:not(:disabled) {
  color: var(--accent-text);
  border-color: color-mix(in srgb, var(--accent) 45%, var(--border));
}

.new-btn:disabled {
  opacity: 0.45;
  cursor: default;
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
