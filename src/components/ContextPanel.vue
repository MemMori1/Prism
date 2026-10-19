<script setup lang="ts">
/**
 * 上下文明细面板：点输入框里那颗圆环弹出来。
 *
 * 一条总进度（代表窗口）+ 四段分解（系统提示 / 工具定义 / 工具结果 / 对话）。
 * 分项是**按字符数分摊**的估算，总数取自引擎；这点必须在面板里说清楚，
 * 否则用户会拿它当精确账单。
 */
import { computed } from 'vue'
import { formatTokens } from '../format'
import { formatPercent, type ContextSlice } from '../contextShare'

const props = defineProps<{
  used: number
  window: number | null
  slices: ContextSlice[]
  /** 分项只是按时间线文字估的（拿不到引擎统计）。 */
  estimated?: boolean
}>()

const percent = computed(() => {
  if (!props.window || props.window <= 0) return null
  return formatPercent((props.used / props.window) * 100)
})

/**
 * 条的总长代表**窗口**：彩色部分是这一轮的构成，剩下的灰是还空着的。
 * 不用「四段铺满」——那样看着像已经用到底了，而且分不清哪段是什么颜色。
 */
const usedWidth = computed(() => {
  if (!props.window || props.window <= 0) return 100
  return Math.min(100, (props.used / props.window) * 100)
})

/** 每段的百分比：小到 0 就写 <0.1%（写 0.0% 像没算）。 */
function percentLabel(slice: ContextSlice): string {
  if (slice.percent == null) return ''
  if (slice.percent > 0 && slice.percent < 0.05) return '<0.1%'
  return formatPercent(slice.percent)
}
</script>

<template>
  <div class="ctx-panel" role="dialog" aria-label="上下文明细">
    <div class="ctx-panel-head">
      <span class="ctx-panel-title">上下文用量</span>
      <span v-if="percent" class="ctx-panel-percent">{{ percent }}</span>
    </div>
    <!-- 「本轮构成」说清楚这条满格的是构成、不是窗口占用，否则会和右上角的 8.1% 打架 -->
    <p class="ctx-panel-size">
      {{ formatTokens(used) }}
      <template v-if="window"> / {{ formatTokens(window) }}</template>

    </p>

    <div class="ctx-panel-bar">
      <span
        v-for="slice in slices"
        :key="slice.key"
        class="ctx-panel-seg"
        :class="`seg-${slice.key}`"
        :style="{ width: `${slice.share * usedWidth}%` }"
      />
    </div>

    <ul class="ctx-panel-list">
      <li v-for="slice in slices" :key="slice.key" class="ctx-panel-row">
        <span class="ctx-panel-dot" :class="`seg-${slice.key}`" />
        <span class="ctx-panel-label">{{ slice.label }}</span>
        <span class="ctx-panel-tokens">{{ formatTokens(slice.tokens) }}</span>
        <span class="ctx-panel-share">{{ percentLabel(slice) }}</span>
      </li>
    </ul>

    <p class="ctx-panel-foot">
      <template v-if="estimated">分项按时间线文字长度估算 · 总量取自引擎</template>
      <template v-else>分项按字符数占比分摊 · 总量取自引擎</template>
    </p>
    <p class="ctx-panel-note">连接器 / MCP / 技能：本引擎不注入</p>
  </div>
</template>

<style scoped src="../styles/components/ContextPanel.css"></style>
