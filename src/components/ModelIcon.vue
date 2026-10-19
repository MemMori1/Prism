<script setup lang="ts">
/**
 * 模型服务的标志（官方单色版）。
 *
 * 两种画法共用这一个出口：官方形状是**填充**（多段轮廓要 evenodd 才不糊成实心），
 * 没有官方形状的通用模型是**描边**。调用方只管给模型键和尺寸。
 */
import { computed } from 'vue'
import { MODEL_ICONS, modelFamily } from '../modelIcons'

const props = withDefaults(
  defineProps<{
    /** 模型键（`deepseek/deepseek-chat` 或裸模型名）。 */
    model: string
    /** 边长（px）。品牌标志细节多，小于 12 会糊。 */
    size?: number
  }>(),
  { size: 14 },
)

const icon = computed(() => MODEL_ICONS[modelFamily(props.model)])
</script>

<template>
  <svg
    class="mm-icon"
    viewBox="0 0 24 24"
    :width="size"
    :height="size"
    :fill="icon.mode === 'fill' ? 'currentColor' : 'none'"
    :stroke="icon.mode === 'stroke' ? 'currentColor' : 'none'"
    :stroke-width="icon.mode === 'stroke' ? 1.7 : undefined"
    stroke-linecap="round"
    stroke-linejoin="round"
    :fill-rule="icon.evenodd ? 'evenodd' : undefined"
    :clip-rule="icon.evenodd ? 'evenodd' : undefined"
    aria-hidden="true"
  >
    <path v-for="(segment, index) in icon.paths" :key="index" :d="segment" />
  </svg>
</template>
