<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import type { ProjectItem, SessionSummary } from '../types'
import { basename, formatTokens, relativeTime } from '../format'
import { ensureAnnotations, isStarred, setStar, tagsOf, titleOf } from '../annotations'
import { ensureGitInfo, gitInfoOf } from '../gitInfo'
import SelectMenu from './SelectMenu.vue'
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
/** 只看星标会话。 */
const starOnly = ref(false)

const SORT_OPTIONS = [
  { value: 'recent', label: '最近活动' },
  { value: 'created', label: '创建时间' },
  { value: 'tokens', label: 'Token' },
]

const RANGE_OPTIONS = [
  { value: 'all', label: '全部时间' },
  { value: 'today', label: '今天' },
  { value: 'week', label: '本周' },
  { value: 'month', label: '本月' },
]

function setSort(value: string): void {
  sortKey.value = value as SessionSort
}

function setRange(value: string): void {
  rangeKey.value = value as TimeRange
}

onMounted(() => {
  void ensureAnnotations()
})

/** 当前项目的分支（用于标题行标注）。 */
const projectBranch = computed(() => gitInfoOf(props.project?.worktree)?.branch ?? null)

watch(
  () => props.project?.worktree,
  path => {
    if (path) void ensureGitInfo([path])
  },
  { immediate: true },
)

async function toggleStar(sessionId: string) {
  try {
    await setStar(sessionId, !isStarred(sessionId))
  } catch (cause) {
    console.error('[prism] 星标失败', cause)
  }
}

/** 新建时再选 Agent，因此只要选好项目就可进入选择页。 */
const canCreate = computed(() => !!props.project)
const newButtonTitle = computed(() => {
  if (!props.project) return '先选择项目'
  return '在当前项目目录新建会话'
})

const visibleSessions = computed(() =>
  sortSessions(
    filterSessionsByRange(props.sessions, rangeToSince(rangeKey.value, Date.now())).filter(
      session => !starOnly.value || isStarred(session.id),
    ),
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
        <span
          v-if="projectBranch"
          class="branch-inline"
          :title="`当前分支：${projectBranch}`"
        >
          ⑂{{ projectBranch }}
        </span>
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
      <SelectMenu
        :model-value="sortKey"
        :options="SORT_OPTIONS"
        size="xs"
        inline
        title="排序方式"
        @update:model-value="setSort"
      />
      <SelectMenu
        :model-value="rangeKey"
        :options="RANGE_OPTIONS"
        size="xs"
        inline
        title="时间范围"
        @update:model-value="setRange"
      />
      <button
        class="mini-toggle"
        :class="{ on: starOnly }"
        title="只看星标会话"
        @click="starOnly = !starOnly"
      >
        ★
      </button>
    </div>

    <div class="pane-body">
      <div v-if="!project" class="pane-empty">从左侧选择一个项目</div>
      <div v-else-if="loading && !sessions.length" class="pane-empty">加载中…</div>
      <div v-else-if="!sessions.length" class="pane-empty">这个项目还没有会话</div>
      <div v-else-if="!visibleSessions.length" class="pane-empty">
        {{ starOnly ? '还没有星标会话' : '该时间范围内没有会话' }}
      </div>

      <button
        v-for="session in visibleSessions"
        :key="session.id"
        class="session-item"
        :class="{ active: session.id === selectedId }"
        :title="titleOf(session.id, session.title)"
        @click="emit('select', session.id)"
      >
        <span class="session-top">
          <span
            class="star-btn"
            :class="{ on: isStarred(session.id) }"
            role="button"
            :title="isStarred(session.id) ? '取消星标' : '加星标'"
            @click.stop="toggleStar(session.id)"
          >
            {{ isStarred(session.id) ? '★' : '☆' }}
          </span>
          <span class="session-title">{{ titleOf(session.id, session.title) }}</span>
        </span>
        <span class="session-sub">
          <span
            v-for="tag in tagsOf(session.id)"
            :key="tag.id"
            class="tag-dot"
            :style="{ background: tag.color }"
            :title="tag.name"
          />
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

<style scoped src="../styles/components/SessionsPane.css"></style>
