<script setup lang="ts">
/**
 * 顶部会话导航（logo 右侧）：平时显示当前会话，点开＝最近会话 + 新建入口。
 *
 * 列表数据由 App 拉好（跨项目 list_sessions 合并）传进来，这里只管显示 / 搜 / 键盘。
 * 点会话只 emit 出去——去工作台那一列还是去归档，由 App 决定（组件不碰路由）。
 */
import { computed, nextTick, ref } from 'vue'
import { titleOf } from '../annotations'
import { ENGINE_GLYPHS } from '../engineIcons'
import { relativeTime } from '../format'
import { filterNavSessions, type NavSessionItem } from '../sessionNav'
import type { EngineChoice } from '../sessionStart'
import type { SessionSummary } from '../types'

const props = withDefaults(
  defineProps<{
    /** 跨项目的最近会话（App 拉好、已按时间倒序）。 */
    items: NavSessionItem[]
    /** 能用来新建会话的 Agent（和选择器同一份数据）：面板底部直接点。 */
    choices: EngineChoice[]
    /** 当前会话：归档里选中的那条，或工作台盯着的那一列。 */
    current?: SessionSummary | null
    /** 正在跑 / 待审批的会话 id（画状态点）。 */
    runningIds?: string[]
    waitingIds?: string[]
    /** 上次用的 Agent id：给它加个「上次」角标。 */
    recentEngine?: string | null
    loading?: boolean
  }>(),
  {
    current: null,
    runningIds: () => [],
    waitingIds: () => [],
    recentEngine: null,
    loading: false,
  },
)

const emit = defineEmits<{
  (e: 'select', sessionId: string): void
  /** 点了某个 Agent：用这个 Agent 新建会话，开草稿输入框。 */
  (e: 'new', engineId: string): void
  /** 面板打开：App 借机刷新列表。 */
  (e: 'open'): void
}>()

const open = ref(false)
const query = ref('')
/** 键盘选中的行（↑↓ 走这里）。 */
const active = ref(0)
const searchBox = ref<HTMLInputElement | null>(null)

const visible = computed(() => filterNavSessions(props.items, query.value))
/** 列表行：把标题 / 图标 / 状态一次性算好，模板里只读（同一个函数别在模板里算两遍）。 */
const rows = computed(() =>
  visible.value.map(item => ({
    item,
    title: titleOf(item.session.id, item.session.title),
    glyph: glyphOf(item.session.engine),
    state: stateOf(item.session.id),
  })),
)
const current = computed(() => props.current ?? null)
/** 列表里标「就在这条」、以及状态点判据用的 id。 */
const currentId = computed(() => props.current?.id ?? null)

const currentTitle = computed(() =>
  current.value ? titleOf(current.value.id, current.value.title) : '最近会话',
)
const currentGlyph = computed(() => glyphOf(current.value?.engine ?? null))
const currentState = computed(() => stateOf(currentId.value))

/** 底部「用哪个 Agent 新建」的 chip：图标 / 上次标记 / 悬停说明一次算好。 */
const chipRows = computed(() =>
  props.choices.map(choice => ({
    choice,
    glyph: glyphOf(choice.icon),
    used: choice.id === props.recentEngine,
    title: chipTitle(choice),
  })),
)

/** 不可用的 chip 也要说清为什么（跟选择器的口径一致）。 */
function chipTitle(choice: EngineChoice): string {
  const used = choice.id === props.recentEngine ? '（上次使用）' : ''
  if (choice.available) return `用 ${choice.label} 新建会话${used}`
  if (choice.checking) return `${choice.label}：正在检查本机环境…`
  if (choice.needsSetup) return `${choice.label}：还没配 API Key`
  return `${choice.label}：本机没装或这会儿用不了`
}

/** 点了某个 Agent：关面板，交给 App 去工作台开这个 Agent 的草稿框。 */
function startWith(choice: EngineChoice) {
  if (!choice.available) return
  close()
  emit('new', choice.id)
}

/** 品牌 mark（见 engineIcons.ts）；Prism 原生与未知引擎返回 null，走兜底图标。 */
function glyphOf(engine: string | null): string[] | null {
  if (!engine) return null
  return ENGINE_GLYPHS[engine] ?? null
}

/** running / waiting 只对当前会话与列表行用同一套判据。 */
function stateOf(sessionId: string | null | undefined): 'running' | 'waiting' | null {
  if (!sessionId) return null
  if (props.runningIds.includes(sessionId)) return 'running'
  if (props.waitingIds.includes(sessionId)) return 'waiting'
  return null
}

function toggle() {
  if (open.value) {
    open.value = false
    return
  }
  open.value = true
  query.value = ''
  active.value = 0
  emit('open')
  void nextTick(() => searchBox.value?.focus())
}

function close() {
  open.value = false
}

function pick(item: NavSessionItem) {
  close()
  emit('select', item.session.id)
}

/** 搜框里的键盘：↑↓ 选、Enter 开、Esc 关。 */
function onSearchKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape') {
    event.preventDefault()
    close()
    return
  }
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
    event.preventDefault()
    const total = visible.value.length
    if (!total) return
    const step = event.key === 'ArrowDown' ? 1 : -1
    active.value = (active.value + step + total) % total
    return
  }
  if (event.key === 'Enter') {
    event.preventDefault()
    const item = visible.value[active.value]
    if (item) pick(item)
  }
}
</script>

<template>
  <div class="nav-wrap">
    <button
      class="nav-btn"
      :class="{ on: open }"
      :aria-expanded="open"
      :title="current ? `当前会话：${currentTitle}` : '最近会话与新建入口'"
      @click="toggle"
      @keydown.esc="close"
    >
      <span class="nav-glyph" aria-hidden="true">
        <!-- 有品牌 mark 就用它（按引擎色描） -->
        <svg v-if="currentGlyph" viewBox="0 0 24 24" width="15" height="15">
          <path v-for="(d, index) in currentGlyph" :key="index" :d="d" fill="currentColor" />
        </svg>
        <!-- Prism 原生：自己的双星 mark -->
        <svg v-else-if="current" viewBox="0 0 24 24" width="15" height="15">
          <path
            d="M13.6 3.4c.5 3.7 2.1 5.3 5.8 5.8-3.7.5-5.3 2.1-5.8 5.8-.5-3.7-2.1-5.3-5.8-5.8 3.7-.5 5.3-2.1 5.8-5.8Z"
            fill="none"
            stroke="currentColor"
            stroke-width="1.6"
            stroke-linejoin="round"
          />
          <path
            d="M6.6 14.4c.3 2.1 1.2 3 3.3 3.3-2.1.3-3 1.2-3.3 3.3-.3-2.1-1.2-3-3.3-3.3 2.1-.3 3-1.2 3.3-3.3Z"
            fill="none"
            stroke="currentColor"
            stroke-width="1.4"
            stroke-linejoin="round"
          />
        </svg>
        <!-- 没有当前会话：三条横线，暗示「这里是列表」 -->
        <svg
          v-else
          viewBox="0 0 24 24"
          width="15"
          height="15"
          fill="none"
          stroke="currentColor"
          stroke-width="1.8"
          stroke-linecap="round"
          aria-hidden="true"
        >
          <path d="M4 7h16M4 12h16M4 17h10" />
        </svg>
      </span>

      <span class="nav-title">{{ currentTitle }}</span>
      <span v-if="currentState" class="nav-dot" :class="`tone-${currentState}`" />
      <svg
        class="nav-caret"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
        aria-hidden="true"
      >
        <path d="m6 9 6 6 6-6" />
      </svg>
    </button>

    <template v-if="open">
      <div class="nav-backdrop" @click="close" />
      <div class="nav-panel" role="dialog" aria-label="会话导航">
        <div class="nav-search">
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="1.9"
            stroke-linecap="round"
            aria-hidden="true"
          >
            <circle cx="11" cy="11" r="6" />
            <path d="m20 20-3.6-3.6" />
          </svg>
          <input
            ref="searchBox"
            v-model="query"
            type="text"
            placeholder="搜会话 / 项目…"
            aria-label="搜会话"
            @keydown="onSearchKeydown"
          />
        </div>

        <div class="nav-list">
          <p v-if="!items.length && loading" class="nav-empty">正在读最近会话…</p>
          <p v-else-if="!visible.length" class="nav-empty">
            {{ query ? '没有匹配的会话' : '还没有别的会话' }}
          </p>
          <button
            v-for="(row, index) in rows"
            :key="row.item.session.id"
            class="nav-item"
            :class="{ on: index === active, here: row.item.session.id === currentId }"
            @click="pick(row.item)"
            @pointerenter="active = index"
          >
            <span class="nav-glyph sm" aria-hidden="true">
              <svg v-if="row.glyph" viewBox="0 0 24 24" width="14" height="14">
                <path v-for="(d, gIndex) in row.glyph" :key="gIndex" :d="d" fill="currentColor" />
              </svg>
              <svg v-else viewBox="0 0 24 24" width="14" height="14">
                <path
                  d="M13.6 3.4c.5 3.7 2.1 5.3 5.8 5.8-3.7.5-5.3 2.1-5.8 5.8-.5-3.7-2.1-5.3-5.8-5.8 3.7-.5 5.3-2.1 5.8-5.8Z"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.7"
                  stroke-linejoin="round"
                />
              </svg>
            </span>
            <span class="nav-item-main">
              <span class="nav-item-title">{{ row.title }}</span>
              <span class="nav-item-sub">
                {{ row.item.projectName }} · {{ relativeTime(row.item.session.timeUpdated) }}
              </span>
            </span>
            <span v-if="row.state" class="nav-dot" :class="`tone-${row.state}`" />
          </button>
        </div>

        <!-- 新建入口：Agent 全列出来，点哪个就用哪个开（不限次数，随时换） -->
        <div class="nav-foot">
          <p class="nav-foot-title">新建会话</p>
          <div class="nav-chips">
            <button
              v-for="row in chipRows"
              :key="row.choice.id"
              class="nav-chip"
              :class="{ used: row.used }"
              :disabled="!row.choice.available"
              :title="row.title"
              @click="startWith(row.choice)"
            >
              <span class="nav-glyph xs" aria-hidden="true">
                <svg v-if="row.glyph" viewBox="0 0 24 24" width="13" height="13">
                  <path
                    v-for="(d, gIndex) in row.glyph"
                    :key="gIndex"
                    :d="d"
                    fill="currentColor"
                  />
                </svg>
                <svg v-else viewBox="0 0 24 24" width="13" height="13">
                  <path
                    d="M13.6 3.4c.5 3.7 2.1 5.3 5.8 5.8-3.7.5-5.3 2.1-5.8 5.8-.5-3.7-2.1-5.3-5.8-5.8 3.7-.5 5.3-2.1 5.8-5.8Z"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.7"
                    stroke-linejoin="round"
                  />
                </svg>
              </span>
              {{ row.choice.label }}
              <span v-if="row.used" class="nav-chip-used">上次</span>
            </button>
          </div>
        </div>
      </div>
    </template>
  </div>
</template>

<style scoped src="../styles/components/SessionNav.css"></style>
