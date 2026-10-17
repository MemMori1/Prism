<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { api } from '../api'
import { buildHeatmap, heatmapColumns, pulseStats, topModels, type HeatCell } from '../home'
import { basename, relativeTime } from '../format'
import { barHeights, formatCost, formatTokens, totalTokens } from '../usageFormat'
import type { ProjectItem, UsageSummary } from '../types'

const props = defineProps<{
  projects: ProjectItem[]
}>()

const emit = defineEmits<{
  (e: 'error', message: string): void
  (e: 'open-project', projectId: string): void
  (e: 'open-usage'): void
}>()

const summary = ref<UsageSummary | null>(null)
const loading = ref(false)
const error = ref('')
const updatedAt = ref<number | null>(null)

async function load() {
  loading.value = true
  error.value = ''
  try {
    summary.value = await api.usageSummary(28)
    updatedAt.value = Date.now()
  } catch (cause) {
    error.value = String(cause)
    emit('error', `首页统计失败：${cause}`)
  } finally {
    loading.value = false
  }
}

onMounted(() => {
  void load()
})

const pulse = computed(() => pulseStats(summary.value?.daily ?? []))
const daily = computed(() => summary.value?.daily ?? [])
const sparkHeights = computed(() =>
  barHeights(daily.value.map(entry => totalTokens(entry.tokens))),
)
const heatCells = computed(() => buildHeatmap(daily.value, 28))
const heatColumns = computed(() => heatmapColumns(heatCells.value))
const rhythmHeights = computed(() => barHeights(summary.value?.hours ?? []))
const models = computed(() => topModels(summary.value?.engines ?? [], 5))
const hasUsage = computed(() => (summary.value?.totals ? totalTokens(summary.value.totals) > 0 : false))

const deltaText = computed(() => {
  const delta = pulse.value.deltaPercent
  if (delta === null) return '昨日无数据'
  const arrow = delta >= 0 ? '↑' : '↓'
  return `${arrow} ${Math.abs(delta).toFixed(0)}% vs 昨日`
})

const deltaTone = computed(() => {
  const delta = pulse.value.deltaPercent
  if (delta === null) return ''
  return delta >= 0 ? 'tone-up' : 'tone-down'
})

const updatedLabel = computed(() =>
  updatedAt.value ? `更新于 ${new Date(updatedAt.value).toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' })}` : '',
)

/** 项目活跃：跨引擎按工作目录合并，按最近活动倒序。 */
interface ActivityRow {
  key: string
  projectId: string
  name: string
  worktree: string
  engines: string[]
  sessions: number
  timeUpdated: number
}

const ENGINE_LABELS: Record<string, string> = { opencode: 'OC', codex: 'CX', claude: 'CC' }

function engineBadge(engine: string): string {
  return ENGINE_LABELS[engine] ?? engine.slice(0, 2).toUpperCase()
}

const activity = computed<ActivityRow[]>(() => {
  const groups = new Map<string, ActivityRow>()
  for (const project of props.projects) {
    const key = project.worktree
    const existing = groups.get(key)
    if (existing) {
      existing.sessions += project.sessionCount
      existing.timeUpdated = Math.max(existing.timeUpdated, project.timeUpdated)
      if (!existing.engines.includes(project.engine)) existing.engines.push(project.engine)
      // 保留最近活跃的那个项目 id（跳转用）。
      if (project.timeUpdated >= existing.timeUpdated) existing.projectId = project.id
    } else {
      groups.set(key, {
        key,
        projectId: project.id,
        name: project.name || basename(project.worktree),
        worktree: project.worktree,
        engines: [project.engine],
        sessions: project.sessionCount,
        timeUpdated: project.timeUpdated,
      })
    }
  }
  return [...groups.values()]
    .sort((a, b) => b.timeUpdated - a.timeUpdated)
    .slice(0, 8)
})

const WEEKDAY_LABELS = ['一', '二', '三', '四', '五', '六', '日']

function cellTitle(cell: HeatCell): string {
  return `${cell.date} · ${formatTokens(cell.tokens)} tokens`
}

function dayShort(date: string): string {
  return date.slice(5)
}
</script>

<template>
  <section class="home">
    <header class="home-head">
      <h2 class="home-title">首页</h2>
      <span class="home-note">近 28 天 · 跨引擎只读统计</span>
      <span v-if="updatedLabel" class="home-updated">{{ updatedLabel }}</span>
      <button class="home-refresh" :disabled="loading" @click="load">
        {{ loading ? '…' : '⟳ 刷新' }}
      </button>
    </header>

    <div class="home-scroll">
      <div v-if="error" class="home-hint error">{{ error }}</div>
      <div v-else-if="loading && !summary" class="home-hint">统计中…</div>

      <template v-else-if="summary">
        <div class="stat-row">
          <div class="stat-card">
            <span class="stat-label">今日 tokens</span>
            <span class="stat-value">{{ formatTokens(pulse.today) }}</span>
            <span class="stat-sub" :class="deltaTone">{{ deltaText }}</span>
          </div>
          <div class="stat-card">
            <span class="stat-label">近 7 天</span>
            <span class="stat-value">{{ formatTokens(pulse.week) }}</span>
            <span class="stat-sub">合计消耗</span>
          </div>
          <div class="stat-card">
            <span class="stat-label">活跃会话</span>
            <span class="stat-value">{{ summary.sessions }}</span>
            <span class="stat-sub">窗口内 {{ summary.sessions > 0 ? '有产出' : '暂无' }}</span>
          </div>
          <div class="stat-card">
            <span class="stat-label">估算成本</span>
            <span class="stat-value">{{ formatCost(summary.costUsd) }}</span>
            <span class="stat-sub">按内置价目表</span>
          </div>
        </div>

        <div v-if="!hasUsage" class="home-hint">
          这个窗口还没有可统计的用量——先去跑几轮会话吧。
        </div>

        <div class="card-grid">
          <section class="card">
            <header class="card-head">
              <h3>token 脉搏</h3>
              <span v-if="pulse.best" class="card-note">
                峰值 {{ pulse.best.date.slice(5) }} · {{ formatTokens(pulse.best.tokens) }}
              </span>
            </header>
            <div class="spark">
              <span
                v-for="(height, index) in sparkHeights"
                :key="index"
                class="spark-bar"
                :style="{ height: `${height}%` }"
                :title="`${daily[index]?.date} · ${formatTokens(totalTokens(daily[index].tokens))}`"
              />
            </div>
            <div class="axis-row">
              <span>{{ daily.length ? dayShort(daily[0].date) : '' }}</span>
              <span>今天</span>
            </div>
          </section>

          <section class="card">
            <header class="card-head">
              <h3>工作节奏</h3>
              <span class="card-note">按会话开始时刻 · 28 天</span>
            </header>
            <div class="rhythm">
              <span
                v-for="(height, hour) in rhythmHeights"
                :key="hour"
                class="rhythm-bar"
                :class="{ off: !(summary.hours[hour] ?? 0) }"
                :style="{ height: `${height}%` }"
                :title="`${hour} 点 · ${summary.hours[hour] ?? 0} 个会话`"
              />
            </div>
            <div class="axis-row">
              <span>0 点</span>
              <span>6</span>
              <span>12</span>
              <span>18</span>
              <span>23</span>
            </div>
          </section>

          <section class="card span-2">
            <header class="card-head">
              <h3>28 天热力图</h3>
              <span class="legend">
                <span class="card-note">少</span>
                <span v-for="level in [0, 1, 2, 3, 4]" :key="level" class="legend-cell" :class="`lv-${level}`" />
                <span class="card-note">多</span>
              </span>
            </header>
            <div class="heat-wrap">
              <div class="heat-days">
                <span v-for="label in WEEKDAY_LABELS" :key="label">{{ label }}</span>
              </div>
              <div class="heat-grid">
                <div v-for="(column, index) in heatColumns" :key="index" class="heat-col">
                  <span
                    v-for="(cell, row) in column"
                    :key="row"
                    class="heat-cell"
                    :class="cell ? `lv-${cell.level}` : 'lv-empty'"
                    :title="cell ? cellTitle(cell) : ''"
                  />
                </div>
              </div>
            </div>
          </section>

          <section class="card">
            <header class="card-head">
              <h3>模型分布</h3>
              <button class="card-link" @click="emit('open-usage')">用量面板 →</button>
            </header>
            <div v-if="!models.length" class="card-empty">暂无模型用量。</div>
            <div v-else class="model-rows">
              <div v-for="item in models" :key="`${item.engine}-${item.model}`" class="model-row">
                <span class="model-name" :title="item.model">{{ item.model }}</span>
                <span class="engine-chip" :class="`engine-${item.engine}`">{{ engineBadge(item.engine) }}</span>
                <span class="model-bar">
                  <span class="model-fill" :style="{ width: `${Math.max(2, item.share * 100)}%` }" />
                </span>
                <span class="model-share">{{ (item.share * 100).toFixed(0) }}%</span>
                <span class="model-cost">{{ item.costUsd === null ? '未收录' : formatCost(item.costUsd) }}</span>
              </div>
            </div>
          </section>

          <section class="card">
            <header class="card-head">
              <h3>项目活跃</h3>
              <span class="card-note">按最近活动</span>
            </header>
            <div v-if="!activity.length" class="card-empty">还没有项目，先去归档建一个会话。</div>
            <div v-else class="project-rows">
              <button
                v-for="row in activity"
                :key="row.key"
                class="project-row"
                :title="row.worktree"
                @click="emit('open-project', row.projectId)"
              >
                <span class="project-name">{{ row.name }}</span>
                <span class="project-engines">
                  <span
                    v-for="engine in row.engines"
                    :key="engine"
                    class="engine-chip"
                    :class="`engine-${engine}`"
                  >{{ engineBadge(engine) }}</span>
                </span>
                <span class="project-sessions">{{ row.sessions }} 会话</span>
                <span class="project-time">{{ relativeTime(row.timeUpdated) }}</span>
              </button>
            </div>
          </section>
        </div>

        <ul v-if="summary.notes.length" class="home-notes">
          <li v-for="note in summary.notes" :key="note">{{ note }}</li>
        </ul>
      </template>
    </div>
  </section>
</template>

<style scoped src="../styles/components/HomePane.css"></style>
