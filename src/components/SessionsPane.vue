<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import type { ProjectItem, SessionSummary } from '../types'
import { basename, formatTokens, relativeTime } from '../format'
import {
  ensureAnnotations,
  isHidden,
  isStarred,
  setHidden,
  setStar,
  tagsOf,
  titleOf,
} from '../annotations'
import SelectMenu from './SelectMenu.vue'
import {
  blankSessionIds,
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
    filterSessionsByRange(
      // 先摘掉「已关闭」的，再按时间范围与星标筛：这样底部那条数字与列表一致。
      props.sessions.filter(session => !isHidden(session.id)),
      rangeToSince(rangeKey.value, Date.now()),
    ).filter(session => !starOnly.value || isStarred(session.id)),
    sortKey.value,
  ),
)

/** 能一键关掉的空会话：跟当前时间范围同一口径（星标的不算、已关的不再关）。 */
const blankIds = computed(() =>
  blankSessionIds(
    filterSessionsByRange(props.sessions, rangeToSince(rangeKey.value, Date.now())),
    isHidden,
    isStarred,
  ),
)

/** 当前项目里被关掉的会话（给「恢复」用）。 */
const hiddenCount = computed(() => props.sessions.filter(item => isHidden(item.id)).length)

const closing = ref(false)

async function closeBlanks(): Promise<void> {
  const ids = blankIds.value
  if (!ids.length || closing.value) return
  closing.value = true
  try {
    await setHidden(ids, true)
  } catch (cause) {
    console.error('[prism] 关闭空会话失败', cause)
  } finally {
    closing.value = false
  }
}

/** 恢复这个项目里所有被关掉的会话；只动显示，数据一直在。 */
async function restoreHidden(): Promise<void> {
  const ids = props.sessions.filter(item => isHidden(item.id)).map(item => item.id)
  if (!ids.length || closing.value) return
  closing.value = true
  try {
    await setHidden(ids, false)
  } catch (cause) {
    console.error('[prism] 恢复会话失败', cause)
  } finally {
    closing.value = false
  }
}

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
      <div v-else-if="!visibleSessions.length && hiddenCount" class="pane-empty">
        这个项目的会话都关掉了（{{ hiddenCount }} 个）——下方面板可以恢复。
      </div>
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

    <!-- 空会话：一键收起 / 一键恢复。关掉只影响这张列表，引擎数据一个字节没动。 -->
    <div v-if="blankIds.length || hiddenCount" class="blank-bar">
      <span class="blank-text" :title="`这个项目共有 ${hiddenCount} 个会话被关掉`">
        <template v-if="blankIds.length">{{ blankIds.length }} 个空会话</template>
        <template v-if="blankIds.length && hiddenCount"> · </template>
        <template v-if="hiddenCount">已关闭 {{ hiddenCount }} 个</template>
      </span>
      <button
        v-if="hiddenCount"
        class="blank-btn"
        :disabled="closing"
        title="把这个项目里关掉的会话重新列出来"
        @click="restoreHidden"
      >
        恢复
      </button>
      <button
        v-if="blankIds.length"
        class="blank-btn primary"
        :disabled="closing"
        title="把没有 token 记录、也没加星标的会话从这个列表里收起来；不删数据，随时能恢复"
        @click="closeBlanks"
      >
        {{ closing ? '…' : '关闭空会话' }}
      </button>
    </div>
  </aside>
</template>

<style scoped src="../styles/components/SessionsPane.css"></style>
