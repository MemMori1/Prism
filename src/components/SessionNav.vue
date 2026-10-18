<script setup lang="ts">
/**
 * 顶部会话导航（logo 右侧）：**一个控件 + 一个面板**。
 *
 * 收起态只有一句 `<Agent> 的会话`——会话不在顶栏摊成一排标签，全都收在面板里。
 * 面板分两栏：
 *   左＝Agent（点一下换人，右栏跟着换）
 *   右＝这个 Agent 的最近会话（搜 / ↑↓ 选 / Enter 开）
 *
 * 点会话只 emit `select`：App 把左侧会话栏对应的那条激活（不开工作台的新列）。
 * 想在多列画布里开一列，走每行右侧那个 `⇥`（emit `open-in-workbench`）——
 * 这是显式动作，不会因为「点了条会话」就悄悄多出一列。
 *
 * 数据由 App 拉好（跨项目 list_sessions 合成最近会话）传进来，这里只管显示 / 搜 / 键盘。
 */
import { computed, nextTick, ref } from 'vue'
import { titleOf } from '../annotations'
import { ENGINE_GLYPHS } from '../engineIcons'
import { relativeTime } from '../format'
import {
  filterNavSessions,
  sessionsOfEngine,
  type NavAgentChoice,
  type NavSessionItem,
} from '../sessionNav'
import { engineLabel } from '../workshop'

const props = withDefaults(
  defineProps<{
    /** 当前在看的那条会话属于哪个 Agent；null＝还不知道（顶栏就叫「会话」）。 */
    agent: string | null
    /** 最近会话（跨项目、时间倒序，App 已排好）。 */
    recent?: NavSessionItem[]
    /** 面板左列的 Agent（含各自会话数）。 */
    agents?: NavAgentChoice[]
    /** 左侧会话栏当前激活的会话 id（列表里标出来）。 */
    selectedId?: string | null
    /** 正在跑 / 待审批的会话 id（画状态点）。 */
    runningIds?: string[]
    waitingIds?: string[]
    loading?: boolean
  }>(),
  {
    recent: () => [],
    agents: () => [],
    selectedId: null,
    runningIds: () => [],
    waitingIds: () => [],
    loading: false,
  },
)

const emit = defineEmits<{
  /** 点一条会话：只激活左侧会话栏里那一条。 */
  (e: 'select', sessionId: string): void
  /** 显式「在工作台开一列」。 */
  (e: 'open-in-workbench', sessionId: string): void
  /** 用左列选中的 Agent 开一个新会话（该 Agent 的页面 / 草稿框）。 */
  (e: 'new-session', agent: string): void
  /** 面板打开：App 借机刷新列表。 */
  (e: 'open'): void
}>()

const open = ref(false)
const query = ref('')
/** 面板左列选中的 Agent（打开面板时对到当前这个）。 */
const picked = ref<string | null>(null)
/** 键盘选中的行（↑↓ 走这里）。 */
const active = ref(0)
const searchBox = ref<HTMLInputElement | null>(null)

/** 「xx 的会话」：中文名结尾不再多一个空格（`Prism 原生的会话` / `OpenCode 的会话`）。 */
function sessionsTitle(label: string): string {
  return /[\u4e00-\u9fa5]$/.test(label) ? `${label}的会话` : `${label} 的会话`
}

const navTitle = computed(() => (props.agent ? sessionsTitle(engineLabel(props.agent)) : '会话'))
const navGlyph = computed(() => glyphOf(props.agent))

/** 「当前 Agent」：顶栏在看的那个；它不在列表里就当没有。 */
const navTitleAgent = computed(() =>
  props.agents.some(item => item.id === props.agent) ? props.agent : null,
)

/** 左列：Agent + 会话数；当前这个打「当前」，没装 / 不能用的灰一点。 */
const agentRows = computed(() =>
  props.agents.map(item => ({
    ...item,
    label: engineLabel(item.id),
    glyph: glyphOf(item.id),
    current: item.id === navTitleAgent.value,
  })),
)

const pickedLabel = computed(() => (picked.value ? engineLabel(picked.value) : '这个 Agent'))
const pickedTitle = computed(() =>
  picked.value ? sessionsTitle(engineLabel(picked.value)) : '这个 Agent 的会话',
)

/** 右栏：先按 Agent 滤，再按关键词滤。 */
const rows = computed(() => {
  const ofAgent = sessionsOfEngine(props.recent, picked.value)
  return filterNavSessions(ofAgent, query.value).map(item => ({
    item,
    title: titleOf(item.session.id, item.session.title),
    state: stateOf(item.session.id),
    here: item.session.id === props.selectedId,
  }))
})

/** 品牌 mark（见 engineIcons.ts）；Prism 原生与未知引擎返回 null，走兜底图标。 */
function glyphOf(engine: string | null): string[] | null {
  if (!engine) return null
  return ENGINE_GLYPHS[engine] ?? null
}

function stateOf(sessionId: string): 'running' | 'waiting' | null {
  if (props.runningIds.includes(sessionId)) return 'running'
  if (props.waitingIds.includes(sessionId)) return 'waiting'
  return null
}

/** 打开时看谁：当前 Agent → 有会话的第一个 → 第一个。 */
function defaultAgent(): string | null {
  if (navTitleAgent.value) return navTitleAgent.value
  const withSessions = props.agents.find(item => item.count > 0)
  return withSessions?.id ?? props.agents[0]?.id ?? null
}

function toggle() {
  if (open.value) {
    open.value = false
    return
  }
  open.value = true
  query.value = ''
  active.value = 0
  picked.value = defaultAgent()
  emit('open')
  void nextTick(() => searchBox.value?.focus())
}

function close() {
  open.value = false
}

/** 左栏换 Agent：只换右栏看的东西，不动页面。 */
function pickAgent(id: string) {
  if (picked.value === id) return
  picked.value = id
  active.value = 0
  void nextTick(() => searchBox.value?.focus())
}

function pick(item: NavSessionItem) {
  close()
  emit('select', item.session.id)
}

function openInWorkbench(sessionId: string) {
  close()
  emit('open-in-workbench', sessionId)
}

/** 用左列选中的 Agent 开一个新会话（换 Agent 也走这条）。 */
function newSession() {
  if (!picked.value) return
  close()
  emit('new-session', picked.value)
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
    const total = rows.value.length
    if (!total) return
    const step = event.key === 'ArrowDown' ? 1 : -1
    active.value = (active.value + step + total) % total
    return
  }
  if (event.key === 'Enter') {
    event.preventDefault()
    const row = rows.value[active.value]
    if (row) pick(row.item)
  }
}
</script>

<template>
  <div class="nav-wrap">
    <!-- 收起态：只有这一句。会话全在面板里，顶栏不再摊出一排标签。 -->
    <button
      class="nav-btn"
      :class="{ on: open }"
      :aria-expanded="open"
      :title="`${navTitle}——点开挑 Agent / 会话`"
      @click="toggle"
      @keydown.esc="close"
    >
      <span class="nav-glyph" aria-hidden="true">
        <svg v-if="navGlyph" viewBox="0 0 24 24" width="15" height="15">
          <path v-for="(d, index) in navGlyph" :key="index" :d="d" fill="currentColor" />
        </svg>
        <svg v-else-if="agent" viewBox="0 0 24 24" width="15" height="15">
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

      <span class="nav-title">{{ navTitle }}</span>
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
        <div class="nav-cols">
          <!-- 左：Agent。点一下 = 右栏换成它的会话（会话才有得选）。 -->
          <div class="nav-agents">
            <p class="nav-sec-title">Agent</p>
            <button
              v-for="row in agentRows"
              :key="row.id"
              class="nav-agent"
              :class="{ on: row.id === picked, muted: !row.available }"
              :title="row.label"
              @click="pickAgent(row.id)"
            >
              <span class="nav-glyph sm" aria-hidden="true">
                <svg v-if="row.glyph" viewBox="0 0 24 24" width="14" height="14">
                  <path v-for="(d, index) in row.glyph" :key="index" :d="d" fill="currentColor" />
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
              <span class="nav-agent-label">{{ row.label }}</span>
              <span v-if="row.current" class="nav-agent-tag">当前</span>
              <span v-else class="nav-agent-count">{{ row.count }}</span>
            </button>
          </div>

          <!-- 右：这个 Agent 的会话。点一条只让左侧会话栏对应那条激活。 -->
          <div class="nav-sessions">
            <div class="nav-sessions-head">
              <span class="nav-sessions-title">{{ pickedTitle }}</span>
              <span v-if="rows.length" class="nav-sessions-count">{{ rows.length }}</span>
            </div>

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
                :placeholder="picked ? `搜${pickedLabel}的会话 / 项目…` : '先在左边挑一个 Agent'"
                aria-label="搜会话"
                @keydown="onSearchKeydown"
              />
            </div>

            <div class="nav-list">
              <p v-if="!picked" class="nav-empty">左边先挑一个 Agent，右边才是它的会话。</p>
              <p v-else-if="!recent.length && loading" class="nav-empty">正在读最近会话…</p>
              <p v-else-if="!rows.length" class="nav-empty">
                {{ query ? '没有匹配的会话' : `${pickedLabel} 还没有会话` }}
              </p>
              <div
                v-for="(row, index) in rows"
                :key="row.item.session.id"
                class="nav-item"
                :class="{ on: index === active, here: row.here }"
                @pointerenter="active = index"
              >
                <button class="nav-item-open" :title="row.title" @click="pick(row.item)">
                  <span class="nav-item-main">
                    <span class="nav-item-title">{{ row.title }}</span>
                    <span class="nav-item-sub">
                      {{ row.here ? '左侧已选中' : row.item.projectName }} ·
                      {{ relativeTime(row.item.session.timeUpdated) }}
                    </span>
                  </span>
                </button>
                <span v-if="row.state" class="nav-dot" :class="`tone-${row.state}`" />
                <button
                  class="nav-item-workbench"
                  title="在工作台开一列（多列并排）"
                  aria-label="在工作台开一列"
                  @click.stop="openInWorkbench(row.item.session.id)"
                >
                  ⇥
                </button>
              </div>
            </div>
          </div>
        </div>

        <!-- 底：用左列这个 Agent 开一个新的（换 Agent 也从这儿走） -->
        <div class="nav-foot">
          <button class="nav-foot-new" :disabled="!picked" @click="newSession">
            ＋ 用 {{ pickedLabel }} 新建会话
          </button>
        </div>
      </div>
    </template>
  </div>
</template>

<style scoped src="../styles/components/SessionNav.css"></style>
