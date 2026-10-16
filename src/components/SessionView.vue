<script setup lang="ts">
import { nextTick, ref, watch } from 'vue'
import { api } from '../api'
import type { SessionSummary, TimelineRecord } from '../types'
import { formatCost, formatTokens, fullTime } from '../format'
import TimelineItem from './TimelineItem.vue'

const props = defineProps<{
  session: SessionSummary | null
  hasProject: boolean
}>()

const emit = defineEmits<{ (e: 'error', message: string): void }>()

const scroller = ref<HTMLElement | null>(null)
const records = ref<TimelineRecord[]>([])
const cursor = ref<number | null>(null)
const hasMore = ref(false)
const loading = ref(false)
const loadingOlder = ref(false)

function report(cause: unknown) {
  emit('error', cause instanceof Error ? cause.message : String(cause))
}

function scrollToBottom() {
  nextTick(() => {
    const el = scroller.value
    if (el) el.scrollTop = el.scrollHeight
  })
}

async function loadInitial(sessionId: string) {
  records.value = []
  cursor.value = null
  hasMore.value = false
  loading.value = true
  try {
    const page = await api.readTimeline(sessionId)
    if (props.session?.id !== sessionId) return
    records.value = page.records
    cursor.value = page.nextCursor
    hasMore.value = page.hasMore
    scrollToBottom()
  } catch (cause) {
    report(cause)
  } finally {
    loading.value = false
  }
}

async function loadOlder() {
  const sessionId = props.session?.id
  if (
    !sessionId ||
    !hasMore.value ||
    cursor.value === null ||
    loadingOlder.value ||
    loading.value
  ) {
    return
  }

  const el = scroller.value
  const previousHeight = el?.scrollHeight ?? 0
  const previousTop = el?.scrollTop ?? 0

  loadingOlder.value = true
  try {
    const page = await api.readTimeline(sessionId, cursor.value)
    if (props.session?.id !== sessionId) return
    records.value = [...page.records, ...records.value]
    cursor.value = page.nextCursor
    hasMore.value = page.hasMore
    // 保持视口锚定：新增内容在顶部，回补滚动偏移。
    await nextTick()
    if (el) el.scrollTop = el.scrollHeight - (previousHeight - previousTop)
  } catch (cause) {
    report(cause)
  } finally {
    loadingOlder.value = false
  }
}

function onScroll() {
  const el = scroller.value
  if (!el || loading.value || loadingOlder.value || !hasMore.value) return
  if (el.scrollTop < 120) void loadOlder()
}

watch(
  () => props.session?.id,
  sessionId => {
    if (sessionId) {
      void loadInitial(sessionId)
    } else {
      records.value = []
      cursor.value = null
      hasMore.value = false
    }
  },
  { immediate: true },
)
</script>

<template>
  <main class="session-view">
    <template v-if="!session">
      <div class="placeholder">
        <div class="placeholder-mark">◆</div>
        <div v-if="!hasProject">从左侧选择一个项目</div>
        <div v-else>选择会话查看时间线</div>
      </div>
    </template>

    <template v-else>
      <header class="session-header">
        <h1 class="session-title">{{ session.title || '未命名会话' }}</h1>
        <div class="session-meta">
          <span class="meta-dir" :title="session.directory">{{ session.directory }}</span>
          <span v-if="session.model" class="meta-chip">{{ session.model }}</span>
          <span class="meta-chip">
            ↓{{ formatTokens(session.tokensInput + session.tokensReasoning) }}
            ↑{{ formatTokens(session.tokensOutput) }}
          </span>
          <span v-if="session.cost" class="meta-chip">{{ formatCost(session.cost) }}</span>
          <span class="meta-time">{{ fullTime(session.timeCreated) }}</span>
        </div>
      </header>

      <div ref="scroller" class="timeline" @scroll.passive="onScroll">
        <div v-if="loading" class="timeline-hint">加载中…</div>
        <div v-else-if="!records.length" class="timeline-hint">这个会话还没有消息</div>

        <template v-else>
          <div class="timeline-top">
            <button
              v-if="hasMore"
              class="older-btn"
              :disabled="loadingOlder"
              @click="loadOlder"
            >
              {{ loadingOlder ? '加载中…' : '加载更早的消息' }}
            </button>
            <div v-else class="timeline-start">已是全部历史</div>
          </div>
          <div class="timeline-track">
            <TimelineItem
              v-for="record in records"
              :key="record.id"
              :record="record"
              :session-id="session.id"
            />
          </div>
        </template>
      </div>
    </template>
  </main>
</template>

<style scoped>
.session-view {
  display: flex;
  flex-direction: column;
  flex: 1;
  min-width: 0;
  min-height: 0;
  background: var(--bg);
}

.placeholder {
  margin: auto;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 10px;
  color: var(--faint);
  font-size: 13px;
}

.placeholder-mark {
  font-size: 26px;
  color: var(--border-strong);
}

.session-header {
  flex: none;
  padding: 14px 22px 12px;
  border-bottom: 1px solid var(--border);
  background: var(--panel);
}

.session-title {
  margin: 0;
  font-size: 15px;
  font-weight: 650;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.session-meta {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 5px;
  font-size: 11px;
  color: var(--muted);
  overflow: hidden;
  white-space: nowrap;
}

.meta-dir {
  font-family: var(--font-mono);
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 40%;
}

.meta-chip {
  padding: 0 8px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: var(--panel-2);
  line-height: 17px;
  font-family: var(--font-mono);
  flex: none;
}

.meta-time {
  margin-left: auto;
  flex: none;
}

.timeline {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: 14px 22px 60px;
}

.timeline-hint {
  text-align: center;
  color: var(--faint);
  font-size: 12px;
  padding-top: 40px;
}

.timeline-top {
  display: flex;
  justify-content: center;
  padding: 2px 0 12px;
}

.older-btn {
  padding: 4px 14px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: var(--panel);
  color: var(--muted);
  font-size: 11.5px;
  transition: all 0.15s ease;
}

.older-btn:hover:not(:disabled) {
  color: var(--text);
  border-color: var(--border-strong);
}

.older-btn:disabled {
  opacity: 0.6;
  cursor: default;
}

.timeline-start {
  color: var(--faint);
  font-size: 10.5px;
}

.timeline-track {
  display: flex;
  flex-direction: column;
  gap: 18px;
  max-width: 1000px;
  margin: 0 auto;
}
</style>
