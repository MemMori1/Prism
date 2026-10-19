<script setup lang="ts">
/**
 * 上下文占用：输入框里那颗小圆环 + 百分比。
 *
 * 只有环是不够的——8% 在 17px 的环上就是一个小点，看着像「坏了」。所以旁边
 * 直接给一个数：有窗口给百分比，没有窗口给绝对量（10.2k），完全没有用量给「—」。
 *
 * 口径：取**最近一轮**的 token 用量（输入 + 缓存读 + 输出）当作当前对话大小，
 * 分母优先用引擎给的 `contextWindow`（codex 有），否则按模型名查一张常见值表；
 * 都没有就不画弧线——编一个分母比不显示更糟。
 */
import { computed } from 'vue'
import { formatTokens } from '../format'
import { formatPercent } from '../contextShare'

const props = withDefaults(
  defineProps<{
    /** 当前上下文大小（token）。 */
    used: number
    /** 模型窗口；null＝未知。 */
    window: number | null
    /** 分母是查表估的（不是引擎给的）。 */
    estimated?: boolean
    /** 环的边长（px）。 */
    size?: number
  }>(),
  { estimated: false, size: 17 },
)

const RADIUS = 9
const CIRCUMFERENCE = 2 * Math.PI * RADIUS

const ratio = computed(() => {
  if (!props.window || props.window <= 0) return null
  return Math.min(1, Math.max(0, props.used / props.window))
})

/** 70% 起提醒、90% 起告警——再往上就该考虑收尾或开新会话了。 */
const tone = computed(() => {
  if (ratio.value == null) return 'unknown'
  if (ratio.value >= 0.9) return 'danger'
  if (ratio.value >= 0.7) return 'warn'
  return 'normal'
})

/**
 * 弧长。最少画 2px：0.5% 的弧在 17px 环上不足一个像素，等于没画——
 * 数字负责准确，环只负责「有占用」这件事，所以给它一个可见下限。
 */
const dash = computed(() => {
  if (ratio.value == null || !props.used) return 0
  return Math.max(ratio.value * CIRCUMFERENCE, 2)
})

/**
 * 环旁边那个数。
 *
 * 有窗口（算得出比例）就报百分比；没有窗口但知道用量就报绝对量；
 * 什么都没统计到就一个破折号——空着会让人以为这块没做完。
 */
const label = computed(() => {
  if (!props.used) return '—'
  if (ratio.value == null) return formatTokens(props.used)
  const percent = ratio.value * 100
  // 有量但不到 1%：写 <1%，别显示成 0%（那看着像没数据）。
  if (percent > 0 && percent < 1) return '<1%'
  return formatPercent(percent)
})

const title = computed(() => {
  if (!props.used) return '上下文：还没有用量（发第一条消息后开始统计）'
  const used = formatTokens(props.used)
  if (!props.window || ratio.value == null) {
    return `上下文 ${used} · 这个模型的窗口未知，只显示已用量\n点击看明细`
  }
  const percent = Math.round(ratio.value * 100)
  const suffix = props.estimated ? '（分母按模型常见窗口估算）' : ''
  return `上下文 ${used} / ${formatTokens(props.window)} · ${percent}%${suffix}\n点击看明细`
})
</script>

<template>
  <span class="ctx-meter" :class="`tone-${tone}`" :title="title" role="status" :aria-label="title">
    <svg :width="size" :height="size" viewBox="0 0 24 24" aria-hidden="true">
      <circle class="ctx-track" cx="12" cy="12" :r="RADIUS" fill="none" stroke-width="2.6" />
      <circle
        v-if="ratio != null && used > 0"
        class="ctx-arc"
        cx="12"
        cy="12"
        :r="RADIUS"
        fill="none"
        stroke-width="2.6"
        stroke-linecap="round"
        :stroke-dasharray="`${dash} ${CIRCUMFERENCE}`"
        transform="rotate(-90 12 12)"
      />
    </svg>
    <span class="ctx-label">{{ label }}</span>
  </span>
</template>

<style scoped src="../styles/components/ContextRing.css"></style>
