<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { api } from '../api'
import {
  barWidth,
  creditsText,
  fetchedAtText,
  planLabel,
  windowLabel,
  windowMetaText,
} from '../quotaFormat'
import { barHeights, formatCost, formatTokens, totalTokens } from '../usageFormat'
import type { QuotaSummary, TokenTotals, UsageSummary } from '../types'

const emit = defineEmits<{
  (e: 'error', message: string): void
}>()

const RANGES = [
  { days: 1, label: '今日' },
  { days: 7, label: '7 日' },
  { days: 30, label: '30 日' },
  { days: 0, label: '全部' },
] as const

const days = ref<number>(7)
const summary = ref<UsageSummary | null>(null)
const loading = ref(false)

async function load() {
  loading.value = true
  try {
    summary.value = await api.usageSummary(days.value)
  } catch (cause) {
    emit('error', String(cause))
  } finally {
    loading.value = false
  }
}

function pickRange(value: number) {
  if (days.value === value) return
  days.value = value
  void load()
}

// ---------------------------------------------------------------- 配额（实时）

const quota = ref<QuotaSummary | null>(null)
const quotaLoading = ref(false)
const quotaError = ref<string | null>(null)
/** 相对时间（重置倒计时）的心跳，60s 一次。 */
const now = ref(Date.now())
let ticker: ReturnType<typeof setInterval> | undefined

async function loadQuota(force = false) {
  if (quotaLoading.value) return
  quotaLoading.value = true
  try {
    quota.value = await api.quotaSummary(force)
    quotaError.value = null
  } catch (cause) {
    quotaError.value = String(cause)
  } finally {
    quotaLoading.value = false
  }
}

onMounted(() => {
  void load()
  void loadQuota(false)
  ticker = setInterval(() => {
    now.value = Date.now()
  }, 60_000)
})

onUnmounted(() => {
  if (ticker) clearInterval(ticker)
})

const dailyTotals = computed(() => (summary.value?.daily ?? []).map(item => totalTokens(item.tokens)))
const heights = computed(() => barHeights(dailyTotals.value))
const hasUnpriced = computed(() =>
  (summary.value?.engines ?? []).some(engine => engine.unpricedModels.length > 0),
)
const unpricedNames = computed(() => {
  const names = new Set<string>()
  for (const engine of summary.value?.engines ?? []) {
    for (const name of engine.unpricedModels) names.add(name)
  }
  return [...names].join('、')
})

function breakdown(tokens: TokenTotals): string {
  return [
    `输入 ${formatTokens(tokens.input)}`,
    `输出 ${formatTokens(tokens.output)}`,
    `思考 ${formatTokens(tokens.reasoning)}`,
    `缓存读 ${formatTokens(tokens.cacheRead)}`,
    `缓存写 ${formatTokens(tokens.cacheWrite)}`,
  ].join(' · ')
}

function barTitle(index: number): string {
  const item = summary.value?.daily[index]
  if (!item) return ''
  return `${item.date} · 合计 ${formatTokens(dailyTotals.value[index] ?? 0)}\n${breakdown(item.tokens)}`
}
</script>

<template>
  <section class="usage-pane">
    <header class="usage-head">
      <div class="range-tabs">
        <button
          v-for="range in RANGES"
          :key="range.days"
          class="range-btn"
          :class="{ active: days === range.days }"
          @click="pickRange(range.days)"
        >
          {{ range.label }}
        </button>
      </div>
      <span class="usage-hint">只读统计：数据直接来自各引擎本地档案</span>
      <button class="refresh" :disabled="loading" @click="load">
        {{ loading ? '统计中…' : '刷新' }}
      </button>
    </header>

    <div v-if="summary" class="usage-body">
      <div class="panel quota-panel">
        <div class="panel-title quota-head">
          <span>配额（实时）</span>
          <span v-if="quota" class="quota-age">更新于 {{ fetchedAtText(quota.fetchedAtMs) }}</span>
          <button class="refresh quota-refresh" :disabled="quotaLoading" @click="loadQuota(true)">
            {{ quotaLoading ? '查询中…' : '刷新配额' }}
          </button>
        </div>
        <div v-if="quotaError" class="quota-note">配额查询失败：{{ quotaError }}</div>
        <div v-else-if="quota" class="quota-grid">
          <div class="quota-card">
            <div class="quota-card-head">
              <span class="quota-name">Codex</span>
              <span v-if="planLabel(quota.codex.planType)" class="quota-chip">
                {{ planLabel(quota.codex.planType) }}
              </span>
            </div>
            <template v-if="quota.codex.state === 'ok'">
              <div v-if="quota.codex.primary" class="quota-window">
                <div class="quota-window-line">
                  <span>{{ windowLabel(quota.codex.primary.windowMinutes) }}</span>
                  <span class="quota-window-meta">
                    {{ windowMetaText(quota.codex.primary, now) }}
                  </span>
                </div>
                <div class="quota-bar">
                  <div
                    class="quota-bar-fill"
                    :style="{ width: `${barWidth(quota.codex.primary.usedPercent)}%` }"
                  />
                </div>
              </div>
              <div v-if="quota.codex.secondary" class="quota-window">
                <div class="quota-window-line">
                  <span>{{ windowLabel(quota.codex.secondary.windowMinutes) }}</span>
                  <span class="quota-window-meta">
                    {{ windowMetaText(quota.codex.secondary, now) }}
                  </span>
                </div>
                <div class="quota-bar">
                  <div
                    class="quota-bar-fill"
                    :style="{ width: `${barWidth(quota.codex.secondary.usedPercent)}%` }"
                  />
                </div>
              </div>
              <div v-if="!quota.codex.primary && !quota.codex.secondary" class="quota-note">
                {{ quota.codex.message ?? '暂无窗口数据' }}
              </div>
              <div v-if="quota.codex.credits" class="quota-note">
                Credits：{{ creditsText(quota.codex.credits) }}
              </div>
            </template>
            <div v-else class="quota-note">{{ quota.codex.message ?? '配额暂不可用' }}</div>
          </div>
          <div class="quota-card">
            <div class="quota-card-head"><span class="quota-name">Claude Code</span></div>
            <div class="quota-note">{{ quota.claude.message ?? '待接入' }}</div>
          </div>
        </div>
        <div v-else class="quota-note">{{ quotaLoading ? '查询中…' : '配额信息暂不可用' }}</div>
      </div>

      <div class="cards">
        <div class="card">
          <div class="card-label">总 tokens</div>
          <div class="card-value">{{ formatTokens(totalTokens(summary.totals)) }}</div>
          <div class="card-note">{{ breakdown(summary.totals) }}</div>
        </div>
        <div class="card">
          <div class="card-label">估算成本</div>
          <div class="card-value">{{ formatCost(summary.costUsd) }}</div>
          <div class="card-note">
            {{ hasUnpriced ? `未收录模型不计入：${unpricedNames}` : '内置价格表 · 美元估算' }}
          </div>
        </div>
        <div class="card">
          <div class="card-label">会话数</div>
          <div class="card-value">{{ summary.sessions }}</div>
          <div class="card-note">{{ days === 0 ? '全部时间' : `最近 ${days} 天` }}</div>
        </div>
      </div>

      <div class="panel">
        <div class="panel-title">每日用量（token 合计）</div>
        <div v-if="summary.daily.length" class="chart">
          <div
            v-for="(item, index) in summary.daily"
            :key="item.date"
            class="chart-col"
            :title="barTitle(index)"
          >
            <div class="chart-bar" :style="{ height: `${heights[index]}%` }" />
          </div>
        </div>
        <div v-else class="panel-empty">暂无数据</div>
        <div v-if="summary.daily.length" class="chart-axis">
          <span>{{ summary.daily[0]?.date }}</span>
          <span>{{ summary.daily[summary.daily.length - 1]?.date }}</span>
        </div>
      </div>

      <div v-if="summary.engines.length" class="panel">
        <div class="panel-title">按引擎 / 模型</div>
        <div v-for="engine in summary.engines" :key="engine.engine" class="engine-block">
          <div class="engine-head">
            <span class="engine-name">{{ engine.label }}</span>
            <span class="engine-meta">
              {{ formatTokens(totalTokens(engine.tokens)) }} tokens · {{ engine.sessions }} 会话 ·
              {{ formatCost(engine.costUsd) }}
            </span>
          </div>
          <div v-for="model in engine.models" :key="model.model" class="model-row">
            <span class="model-name" :title="model.model">{{ model.model }}</span>
            <span class="model-tokens" :title="breakdown(model.tokens)">
              {{ formatTokens(totalTokens(model.tokens)) }}
              <span class="model-sessions">/ {{ model.sessions }} 会话</span>
            </span>
            <span class="model-cost" :class="{ unpriced: model.costUsd === null }">
              {{ formatCost(model.costUsd) }}
            </span>
          </div>
          <div v-if="engine.unpricedModels.length" class="engine-note">
            未收录价格：{{ engine.unpricedModels.join('、') }}
          </div>
        </div>
      </div>
      <div v-else class="panel panel-empty">所选范围内暂无用量数据。</div>

      <ul v-if="summary.notes.length" class="notes">
        <li v-for="note in summary.notes" :key="note">{{ note }}</li>
      </ul>
    </div>
    <div v-else class="panel panel-empty">
      {{ loading ? '统计中…' : '暂无数据，点「刷新」重试。' }}
    </div>
  </section>
</template>

<style scoped src="../styles/components/UsagePane.css"></style>
