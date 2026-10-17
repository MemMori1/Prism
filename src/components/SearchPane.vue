<script setup lang="ts">
import { onMounted, ref, watch } from 'vue'
import { api } from '../api'
import { parseSnippet, type SnippetSegment } from '../search'
import { rangeToSince, type TimeRange } from '../sessionList'
import type { ProjectItem, SearchHit, SearchStats } from '../types'
import { basename, relativeTime } from '../format'

const props = defineProps<{
  projects: ProjectItem[]
}>()

const emit = defineEmits<{
  (e: 'error', message: string): void
  (e: 'openHit', hit: SearchHit): void
}>()

const term = ref('')
const projectId = ref('')
const range = ref<TimeRange>('all')
const onlyTitle = ref(false)
const results = ref<SearchHit[]>([])
const searching = ref(false)
const searched = ref(false)
const tookMs = ref(0)
const stats = ref<SearchStats | null>(null)
const updating = ref(false)

/** 防乱序：只认最后一次查询的响应。 */
let queryToken = 0
let debounceTimer: number | null = null

function report(cause: unknown): void {
  emit('error', cause instanceof Error ? cause.message : String(cause))
}

async function refreshStats(): Promise<void> {
  try {
    stats.value = await api.searchStats()
  } catch (cause) {
    report(cause)
  }
}

/** 增量更新索引；silent 用于打开页面时的后台刷新。 */
async function updateIndex(silent = false): Promise<void> {
  if (updating.value) return
  updating.value = true
  try {
    stats.value = await api.searchUpdateIndex()
  } catch (cause) {
    if (!silent) report(cause)
  } finally {
    updating.value = false
  }
}

function scheduleSearch(): void {
  if (debounceTimer !== null) window.clearTimeout(debounceTimer)
  debounceTimer = window.setTimeout(() => {
    void runSearch()
  }, 280)
}

async function runSearch(): Promise<void> {
  const text = term.value.trim()
  if (!text) {
    results.value = []
    searched.value = false
    return
  }
  const token = ++queryToken
  searching.value = true
  const startedAt = performance.now()
  try {
    const hits = await api.searchQuery(text, {
      projectId: projectId.value || null,
      since: rangeToSince(range.value, Date.now()),
      onlyTitle: onlyTitle.value,
    })
    if (token !== queryToken) return
    results.value = hits
    searched.value = true
    tookMs.value = Math.round(performance.now() - startedAt)
  } catch (cause) {
    if (token === queryToken) report(cause)
  } finally {
    if (token === queryToken) searching.value = false
  }
}

function snippetSegments(hit: SearchHit): SnippetSegment[] {
  return parseSnippet(hit.snippet)
}

function projectLabel(id: string): string {
  const project = props.projects.find(item => item.id === id)
  if (!project) return '未知项目'
  return project.name || basename(project.worktree)
}

function roleLabel(role: string | null): string {
  if (role === 'user') return '我'
  if (role === 'assistant') return '助手'
  return '标题'
}

watch([term, projectId, range, onlyTitle], scheduleSearch)

onMounted(async () => {
  await refreshStats()
  // 索引不存在或超过 10 分钟未刷新：后台增量更新一次。
  const stale = !stats.value || Date.now() - stats.value.updatedAt > 10 * 60 * 1000
  if (stale) void updateIndex(true)
})
</script>

<template>
  <section class="search-pane">
    <div class="search-bar">
      <span class="search-glyph" aria-hidden="true">⌕</span>
      <input
        v-model="term"
        class="search-input"
        type="search"
        placeholder="搜索全部会话内容（支持中文子串）…"
        spellcheck="false"
      />
      <button
        class="index-btn"
        :disabled="updating"
        title="增量更新本地索引（只读引擎数据，索引存在 ~/.prism）"
        @click="updateIndex()"
      >
        {{ updating ? '索引中…' : '刷新索引' }}
      </button>
    </div>

    <div class="search-filters">
      <select v-model="projectId" class="mini-select" title="按项目过滤">
        <option value="">全部项目</option>
        <option v-for="project in projects" :key="project.id" :value="project.id">
          {{ project.name || basename(project.worktree) }}
        </option>
      </select>
      <select v-model="range" class="mini-select" title="按时间范围过滤">
        <option value="all">全部时间</option>
        <option value="today">今天</option>
        <option value="week">本周</option>
        <option value="month">本月</option>
      </select>
      <label class="only-title">
        <input v-model="onlyTitle" type="checkbox" />
        仅标题
      </label>
      <span v-if="stats" class="index-stats" :title="`索引位置：~/.prism/search.db`">
        {{ stats.indexedMessages }} 条消息 / {{ stats.indexedSessions }} 个会话 ·
        {{ stats.updatedAt ? `更新于 ${relativeTime(stats.updatedAt)}` : '尚未建立索引' }}
      </span>
    </div>

    <div class="results">
      <div v-if="!searched && !searching" class="search-hint">
        输入关键词检索全部会话。索引由 Prism 本地维护，引擎数据只读。
      </div>
      <div v-else-if="searching && !results.length" class="search-hint">搜索中…</div>
      <div v-else-if="searched && !results.length" class="search-hint">
        没有找到匹配（{{ tookMs }}ms）
      </div>

      <template v-else>
        <div class="results-meta">
          {{ results.length }} 条命中 · {{ tookMs }}ms
          <span v-if="searching" class="results-live">更新中…</span>
        </div>
        <button
          v-for="(hit, index) in results"
          :key="`${hit.sessionId}-${hit.messageId ?? 'title'}-${index}`"
          class="result-item"
          :title="`打开会话定位到该消息`"
          @click="emit('openHit', hit)"
        >
          <div class="result-head">
            <span class="result-role" :class="`role-${hit.role ?? 'title'}`">
              {{ roleLabel(hit.role) }}
            </span>
            <span class="result-title">{{ hit.sessionTitle || '未命名会话' }}</span>
            <span class="result-project">{{ projectLabel(hit.projectId) }}</span>
            <span class="result-time">{{ relativeTime(hit.timeCreated) }}</span>
          </div>
          <div class="result-snippet">
            <span
              v-for="(segment, segIndex) in snippetSegments(hit)"
              :key="segIndex"
              :class="{ 'seg-hit': segment.hit }"
              >{{ segment.text }}</span
            >
          </div>
        </button>
      </template>
    </div>
  </section>
</template>

<style scoped>
.search-pane {
  display: flex;
  flex-direction: column;
  gap: 10px;
  flex: 1;
  min-width: 0;
  padding: 14px 18px 0;
  overflow: hidden;
}

.search-bar {
  display: flex;
  align-items: center;
  gap: 10px;
}

.search-glyph {
  color: var(--faint);
  font-size: 18px;
}

.search-input {
  flex: 1;
  height: 38px;
  padding: 0 14px;
  border: 1px solid var(--border-strong);
  border-radius: var(--radius);
  background: var(--panel);
  color: var(--text);
  font-size: 14px;
  outline: none;
}

.search-input:focus {
  border-color: var(--accent);
  box-shadow: 0 0 0 3px var(--accent-soft);
}

.index-btn {
  height: 38px;
  padding: 0 14px;
  border: 1px solid var(--border-strong);
  border-radius: var(--radius);
  background: var(--panel);
  color: var(--text);
  font-size: 13px;
  cursor: pointer;
}

.index-btn:hover:not(:disabled) {
  border-color: var(--accent);
  color: var(--accent-text);
}

.index-btn:disabled {
  opacity: 0.6;
  cursor: default;
}

.search-filters {
  display: flex;
  align-items: center;
  gap: 10px;
  font-size: 12px;
  color: var(--muted);
}

.mini-select {
  height: 26px;
  padding: 0 6px;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--panel);
  color: var(--text);
  font-size: 12px;
}

.only-title {
  display: flex;
  align-items: center;
  gap: 5px;
  cursor: pointer;
  user-select: none;
}

.index-stats {
  margin-left: auto;
  color: var(--faint);
}

.results {
  display: flex;
  flex-direction: column;
  gap: 6px;
  overflow-y: auto;
  padding-bottom: 14px;
}

.search-hint {
  margin-top: 48px;
  text-align: center;
  color: var(--faint);
  font-size: 13px;
}

.results-meta {
  color: var(--faint);
  font-size: 12px;
  padding: 2px 2px 4px;
}

.results-live {
  margin-left: 6px;
  color: var(--accent-text);
}

.result-item {
  display: flex;
  flex-direction: column;
  gap: 5px;
  padding: 10px 12px;
  border: 1px solid var(--border);
  border-radius: var(--radius);
  background: var(--panel);
  text-align: left;
  cursor: pointer;
  transition: border-color 0.12s ease, box-shadow 0.12s ease;
}

.result-item:hover {
  border-color: var(--accent);
  box-shadow: var(--shadow);
}

.result-head {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  min-width: 0;
}

.result-role {
  flex: none;
  padding: 1px 7px;
  border-radius: 999px;
  font-size: 11px;
  background: var(--code-bg);
  color: var(--muted);
}

.result-role.role-user {
  background: var(--accent-soft);
  color: var(--accent-text);
}

.result-title {
  font-weight: 600;
  color: var(--text);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.result-project,
.result-time {
  flex: none;
  color: var(--faint);
}

.result-time {
  margin-left: auto;
}

.result-snippet {
  font-size: 12.5px;
  line-height: 1.55;
  color: var(--muted);
  word-break: break-word;
}

.seg-hit {
  color: var(--amber);
  background: var(--amber-soft);
  border-radius: 3px;
  padding: 0 1px;
  font-weight: 600;
}
</style>
