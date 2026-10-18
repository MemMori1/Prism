<script setup lang="ts">
/**
 * 每轮对话脚下那行 meta 用的一套线性图标（24 网格、跟着字色走），
 * 赛马 HUD 也复用，保证「输入/输出 token」在全 app 长一个样。
 */
type IconName = 'copy' | 'check' | 'input' | 'output' | 'cache' | 'reasoning' | 'clock' | 'model'

const props = withDefaults(defineProps<{ name: IconName; size?: number }>(), { size: 13 })
</script>

<template>
  <svg
    class="meta-icon"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="1.7"
    stroke-linecap="round"
    stroke-linejoin="round"
    :width="props.size"
    :height="props.size"
    aria-hidden="true"
  >
    <template v-if="name === 'copy'">
      <rect x="9" y="9.4" width="10.6" height="10.6" rx="2.6" />
      <path d="M15.6 6.5A2.6 2.6 0 0 0 13.4 4.4H6.6A2.6 2.6 0 0 0 4 7v6.8a2.6 2.6 0 0 0 2.1 2.5" />
    </template>
    <path v-else-if="name === 'check'" d="M5.2 12.6 9.8 17.2 18.8 7.4" />
    <template v-else-if="name === 'input'">
      <path d="M12 19.2V5.4" />
      <path d="M6.6 10.8 12 5.4l5.4 5.4" />
    </template>
    <template v-else-if="name === 'output'">
      <path d="M12 4.8v13.8" />
      <path d="M17.4 13.2 12 18.6l-5.4-5.4" />
    </template>
    <path v-else-if="name === 'cache'" d="M13.4 3 6 13.4h5.1L10.6 21 18 10.6h-5.1L13.4 3Z" />
    <path
      v-else-if="name === 'reasoning'"
      d="M12 5.1c.6 4.7 2.2 6.3 6.9 6.9-4.7.6-6.3 2.2-6.9 6.9-.6-4.7-2.2-6.3-6.9-6.9 4.7-.6 6.3-2.2 6.9-6.9Z"
    />
    <template v-else-if="name === 'clock'">
      <circle cx="12" cy="12" r="8.2" />
      <path d="M12 7.6V12l3.2 2" />
    </template>
    <template v-else-if="name === 'model'">
      <rect x="5.6" y="5.6" width="12.8" height="12.8" rx="3" />
      <path d="M12 2.8v2.8M12 18.4v2.8M2.8 12h2.8M18.4 12h2.8" />
    </template>
  </svg>
</template>

<style scoped src="../styles/components/MetaIcon.css"></style>
