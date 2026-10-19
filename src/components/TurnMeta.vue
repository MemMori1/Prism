<script setup lang="ts">
/**
 * 一轮对话脚下的统一 meta 行：模型 / 用时 / token（输入、缓存、输出、思考）/
 * 成本 / 上下文用量 / 复制按钮。归档记录（record）与实时轮次（runningSince）
 * 共用一套渲染，样式统一在这里，别处只引用。
 */
import { computed, onUnmounted, ref, watch } from 'vue'
import type { TimelineRecord } from '../types'
import { formatCost, formatDuration, formatTokens, fullTime } from '../format'
import MetaIcon from './MetaIcon.vue'
import ModelIcon from './ModelIcon.vue'
import { recordModelKey } from '../composer'

const props = withDefaults(
  defineProps<{
    /** 归档记录：模型 / tokens 从它来。 */
    record?: TimelineRecord | null
    /** 归档轮次的用时（由 turnDurations 算好传进来）。 */
    durationMs?: number | null
    /** 正在跑的轮次：从这一刻起实时计时。 */
    runningSince?: number | null
    /** live 轮次没有 record 时，由父组件给当前模型名。 */
    model?: string | null
    /** 复制按钮要复制的内容；为空则不显示按钮。 */
    copyText?: string | null
    /** 靠右对齐（用户气泡）。 */
    align?: 'start' | 'end'
  }>(),
  {
    record: null,
    durationMs: null,
    runningSince: null,
    model: null,
    copyText: null,
    align: 'start',
  },
)

const tokens = computed(() => props.record?.tokens ?? null)

/** 有真数据才占位（全 0 的记录不画 token 组）。 */
const hasTokens = computed(() => {
  const current = tokens.value
  if (!current) return false
  return Boolean(
    current.input ||
      current.cacheRead ||
      current.cacheWrite ||
      current.output ||
      current.reasoning ||
      current.cost,
  )
})

/** 上下文用量（窗口已知且用量为正时显示）。 */
const context = computed(() => {
  const current = tokens.value
  if (!current?.contextWindow || current.contextWindow <= 0) return null
  const used = current.input + current.cacheRead + current.output
  if (used <= 0) return null
  return {
    used,
    percent: Math.min(100, Math.round((used / current.contextWindow) * 100)),
  }
})

const modelLabel = computed(() => props.record?.model || props.model || '')
/** 图标按模型服务走：归档记录能拼出 `provider::model`，实时轮次只有模型名也能认。 */
const modelKey = computed(
  () => (props.record ? recordModelKey(props.record) : null) ?? props.model ?? '',
)
const modelTitle = computed(() =>
  [props.record?.modelProvider, modelLabel.value, props.record?.variant]
    .filter(Boolean)
    .join(' · '),
)

/** 实时轮次每秒刷一次计时（没有 runningSince 就不起 timer）。 */
const now = ref(Date.now())
let ticker: number | null = null

function stopTicker() {
  if (ticker !== null) {
    window.clearInterval(ticker)
    ticker = null
  }
}

watch(
  () => props.runningSince,
  since => {
    stopTicker()
    if (!since) return
    now.value = Date.now()
    ticker = window.setInterval(() => {
      now.value = Date.now()
    }, 1000)
  },
  { immediate: true },
)

const durationLabel = computed(() => {
  if (props.runningSince) return formatDuration(now.value - props.runningSince)
  return formatDuration(props.durationMs ?? 0)
})

const durationTitle = computed(() => {
  if (props.runningSince) return `本轮开始于 ${fullTime(props.runningSince)}`
  if (props.record?.timeCreated) return `消息时间 ${fullTime(props.record.timeCreated)}`
  return ''
})

const copyText = computed(() => props.copyText?.trim() ?? '')
const copied = ref(false)
let copyTimer: number | null = null

function stopCopyTimer() {
  if (copyTimer !== null) {
    window.clearTimeout(copyTimer)
    copyTimer = null
  }
}

async function copy() {
  const text = copyText.value
  if (!text) return
  try {
    await navigator.clipboard.writeText(text)
  } catch {
    // 剪贴板 API 不可用时退回选区复制。
    const area = document.createElement('textarea')
    area.value = text
    area.style.position = 'fixed'
    area.style.opacity = '0'
    document.body.append(area)
    area.select()
    try {
      document.execCommand('copy')
    } finally {
      area.remove()
    }
  }
  copied.value = true
  stopCopyTimer()
  copyTimer = window.setTimeout(() => {
    copied.value = false
  }, 1400)
}

onUnmounted(() => {
  stopTicker()
  stopCopyTimer()
})

const visible = computed(() =>
  Boolean(modelLabel.value || durationLabel.value || hasTokens.value || copyText.value),
)
</script>

<template>
  <div v-if="visible" class="turn-meta" :class="`align-${align}`">
    <span v-if="modelLabel" class="tm-item tm-model" :title="modelTitle">
      <ModelIcon v-if="modelKey" :model="modelKey" :size="12" />
      <MetaIcon v-else name="model" :size="12" />
      <span class="tm-ellipsis">{{ modelLabel }}</span>
    </span>

    <span v-if="durationLabel" class="tm-item" :title="durationTitle">
      <MetaIcon name="clock" />
      {{ durationLabel }}
    </span>

    <template v-if="tokens && hasTokens">
      <span class="tm-item" title="未缓存输入">
        <MetaIcon name="input" />
        {{ formatTokens(tokens.input) }}
      </span>
      <span v-if="tokens.cacheRead" class="tm-item" title="缓存读">
        <MetaIcon name="cache" />
        {{ formatTokens(tokens.cacheRead) }}
      </span>
      <span class="tm-item" title="输出">
        <MetaIcon name="output" />
        {{ formatTokens(tokens.output) }}
      </span>
      <span v-if="tokens.reasoning" class="tm-item" title="思考">
        <MetaIcon name="reasoning" />
        {{ formatTokens(tokens.reasoning) }}
      </span>
      <span v-if="tokens.cost" class="tm-item tm-cost" title="该轮成本">
        {{ formatCost(tokens.cost) }}
      </span>
      <span
        v-if="context"
        class="tm-item tm-ctx"
        :title="`上下文 ≈ ${formatTokens(context.used)} / 窗口 ${formatTokens(tokens.contextWindow ?? 0)} tokens`"
      >
        <span class="tm-ctx-bar">
          <span class="tm-ctx-fill" :style="{ width: `${context.percent}%` }" />
        </span>
        {{ context.percent }}%
      </span>
    </template>

    <button
      v-if="copyText"
      class="tm-copy"
      :class="{ copied }"
      :title="copied ? '已复制' : '复制这一轮'"
      :aria-label="copied ? '已复制' : '复制这一轮'"
      @click="copy"
    >
      <MetaIcon :name="copied ? 'check' : 'copy'" />
    </button>
  </div>
</template>

<style scoped src="../styles/components/TurnMeta.css"></style>
