<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { api } from '../api'
import { parseSnippet, type SnippetSegment } from '../search'
import { rangeToSince, type TimeRange } from '../sessionList'
import type { ProjectItem, SearchHit, SearchStats } from '../types'
import { basename, relativeTime } from '../format'
import SelectMenu from './SelectMenu.vue'

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
const inputEl = ref<HTMLInputElement | null>(null)

/** 过滤条两个下拉的选项。 */
const RANGE_OPTIONS = [
  { value: 'all', label: '全部时间' },
  { value: 'today', label: '今天' },
  { value: 'week', label: '本周' },
  { value: 'month', label: '本月' },
]

const projectOptions = computed(() => [
  { value: '', label: '全部项目' },
  ...props.projects.map(item => ({ value: item.id, label: item.name || basename(item.worktree) })),
])

function setRange(value: string): void {
  range.value = value as TimeRange
}

const indexSummary = computed(() => {
  const current = stats.value
  if (!current) return '尚未建立索引'
  return `${current.indexedSessions.toLocaleString('zh-CN')} 个会话 · ${current.indexedMessages.toLocaleString('zh-CN')} 条消息`
})

const indexUpdated = computed(() => {
  const current = stats.value
  if (!current || !current.updatedAt) return '尚未更新'
  return `更新于 ${relativeTime(current.updatedAt)}`
})

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
  if (debounceTimer !== null) {
    window.clearTimeout(debounceTimer)
    debounceTimer = null
  }
  if (!term.value.trim()) {
    void runSearch()
    return
  }
  debounceTimer = window.setTimeout(() => {
    void runSearch()
  }, 280)
}

function submitSearch(): void {
  if (debounceTimer !== null) {
    window.clearTimeout(debounceTimer)
    debounceTimer = null
  }
  void runSearch()
}

async function runSearch(): Promise<void> {
  const token = ++queryToken
  const text = term.value.trim()
  if (!text) {
    results.value = []
    searched.value = false
    searching.value = false
    tookMs.value = 0
    return
  }
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

function clearSearch(): void {
  queryToken += 1
  if (debounceTimer !== null) {
    window.clearTimeout(debounceTimer)
    debounceTimer = null
  }
  term.value = ''
  results.value = []
  searched.value = false
  searching.value = false
  tookMs.value = 0
  inputEl.value?.focus()
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

function openHint(hit: SearchHit): string {
  return hit.messageId ? '打开会话并定位到该消息' : '打开会话'
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
    <div class="search-shell">
      <header class="search-head">
        <div class="search-head-copy">
          <h1 class="search-title">搜索</h1>
          <p class="search-subtitle">在全部会话的标题和消息中检索</p>
        </div>
        <div class="index-state" title="索引位置：~/.prism/search.db">
          <span class="index-dot" :class="{ 'is-ready': stats && stats.indexedSessions > 0 }"></span>
          <span class="index-text">
            {{ indexSummary }}
            <template v-if="stats">
              <span class="index-sep">·</span>
              <span class="index-time">{{ indexUpdated }}</span>
            </template>
          </span>
        </div>
      </header>

      <div class="search-field" :class="{ 'is-busy': searching }">
        <svg class="search-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" aria-hidden="true">
          <circle cx="11" cy="11" r="6.5" />
          <path d="m16 16 4.5 4.5" />
        </svg>
        <input
          ref="inputEl"
          v-model="term"
          class="search-input"
          type="search"
          placeholder="搜索消息内容、会话标题…"
          spellcheck="false"
          @keydown.enter="submitSearch"
          @keydown.esc="clearSearch"
        />
        <span v-if="searching" class="search-spinner" aria-hidden="true"></span>
        <button
          v-if="term"
          type="button"
          class="search-clear"
          title="清空搜索"
          aria-label="清空搜索"
          @click="clearSearch"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" aria-hidden="true">
            <path d="M6 6l12 12M18 6 6 18" />
          </svg>
        </button>
      </div>

      <div class="search-controls">
        <div class="filter-group">
          <div class="filter-chip">
            <span class="filter-label">项目</span>
            <SelectMenu
              v-model="projectId"
              :options="projectOptions"
              size="sm"
              tone="ghost"
              inline
              title="按项目过滤"
            />
          </div>

          <div class="filter-chip">
            <span class="filter-label">时间</span>
            <SelectMenu
              :model-value="range"
              :options="RANGE_OPTIONS"
              size="sm"
              tone="ghost"
              inline
              title="按时间范围过滤"
              @update:model-value="setRange"
            />
          </div>

          <label class="filter-toggle" :class="{ 'is-active': onlyTitle }">
            <input v-model="onlyTitle" type="checkbox" class="toggle-input" />
            <span class="toggle-box" aria-hidden="true">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round">
                <path d="m5 12 4.5 4.5L19 7" />
              </svg>
            </span>
            <span>仅标题</span>
          </label>
        </div>

        <button
          class="index-btn"
          :class="{ 'is-busy': updating }"
          :disabled="updating"
          title="增量更新本地索引（只读引擎数据，索引存在 ~/.prism）"
          @click="updateIndex()"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M20 11a8 8 0 1 0-2.3 5.7" />
            <path d="M20 5v6h-6" />
          </svg>
          {{ updating ? '索引更新中…' : '刷新索引' }}
        </button>
      </div>

      <div class="results">
        <div v-if="!searched && !searching" class="search-empty">
          <span class="empty-icon">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" aria-hidden="true">
              <circle cx="11" cy="11" r="6.5" />
              <path d="m16 16 4.5 4.5" />
            </svg>
          </span>
          <p class="empty-title">搜索全部会话</p>
          <p class="empty-text">输入关键词检索消息内容。索引由 Prism 在本地维护，引擎数据只读。</p>
        </div>

        <div v-else-if="searching && !results.length" class="search-empty">
          <span class="empty-spinner" aria-hidden="true"></span>
          <p class="empty-title">正在搜索…</p>
          <p class="empty-text">正在最后确认索引状态。</p>
        </div>

        <div v-else-if="searched && !results.length" class="search-empty">
          <span class="empty-icon">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" aria-hidden="true">
              <circle cx="11" cy="11" r="6.5" />
              <path d="m16 16 4.5 4.5" />
              <path d="m8.5 8.5 5 5M13.5 8.5l-5 5" />
            </svg>
          </span>
          <p class="empty-title">没有找到「{{ term.trim() }}」</p>
          <p class="empty-text">试试更短的关键词，或放宽项目与时间范围。</p>
        </div>

        <template v-else>
          <div class="results-meta">
            <span><strong>{{ results.length }}</strong> 条结果</span>
            <span class="results-sep">·</span>
            <span>{{ tookMs }} ms</span>
            <span v-if="searching" class="results-live">
              <span class="mini-spinner" aria-hidden="true"></span>更新中
            </span>
            <span class="results-hint">点击结果打开会话</span>
          </div>

          <div class="result-list">
            <button
              v-for="(hit, index) in results"
              :key="`${hit.sessionId}-${hit.messageId ?? 'title'}-${index}`"
              class="result-item"
              :title="openHint(hit)"
              :aria-label="`打开会话：${hit.sessionTitle || '未命名会话'}`"
              @click="emit('openHit', hit)"
            >
              <span class="result-head">
                <span class="result-role" :class="`role-${hit.role ?? 'title'}`">
                  {{ roleLabel(hit.role) }}
                </span>
                <span class="result-title">{{ hit.sessionTitle || '未命名会话' }}</span>
                <span class="result-time">{{ relativeTime(hit.timeCreated) }}</span>
              </span>
              <span class="result-snippet">
                <template v-for="(segment, segIndex) in snippetSegments(hit)" :key="segIndex">
                  <mark v-if="segment.hit">{{ segment.text }}</mark>
                  <span v-else>{{ segment.text }}</span>
                </template>
              </span>
              <span class="result-foot">
                <span class="result-project">
                  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                    <path d="M3.5 7.5a2 2 0 0 1 2-2h4l2 2h7a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z" />
                  </svg>
                  {{ projectLabel(hit.projectId) }}
                </span>
                <span class="result-open">
                  打开会话
                  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                    <path d="M7 17 17 7" />
                    <path d="M9 7h8v8" />
                  </svg>
                </span>
              </span>
            </button>
          </div>
        </template>
      </div>
    </div>
  </section>
</template>

<style scoped src="../styles/components/SearchPane.css"></style>
