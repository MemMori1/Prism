<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { api } from '../api'
import { basename, formatBytes } from '../format'
import type { ArtifactData } from '../types'

const props = defineProps<{
  /** 本地文件路径；null 时不显示。 */
  path: string | null
}>()

const emit = defineEmits<{
  (e: 'close'): void
}>()

const loading = ref(false)
const error = ref<string | null>(null)
const data = ref<ArtifactData | null>(null)

const imageUrl = computed(() => {
  const current = data.value
  if (!current?.base64) return null
  return `data:${current.mime};base64,${current.base64}`
})

const svgUrl = computed(() => {
  const current = data.value
  if (current?.kind !== 'svg' || !current.text) return null
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(current.text)}`
})

watch(
  () => props.path,
  async path => {
    error.value = null
    data.value = null
    if (!path) return
    loading.value = true
    try {
      const loaded = await api.readArtifact(path)
      if (props.path !== path) return
      data.value = loaded
    } catch (cause) {
      if (props.path !== path) return
      error.value = String(cause)
    } finally {
      if (props.path === path) loading.value = false
    }
  },
  { immediate: true },
)

function onKey(event: KeyboardEvent) {
  if (event.key === 'Escape') emit('close')
}

onMounted(() => window.addEventListener('keydown', onKey))
onBeforeUnmount(() => window.removeEventListener('keydown', onKey))

function openSystem() {
  if (props.path) void api.openPath(props.path)
}
</script>

<template>
  <Teleport to="body">
    <div v-if="path" class="artifact-overlay" @click.self="emit('close')">
      <div class="artifact-panel" role="dialog" aria-label="交付物预览">
        <header class="artifact-head">
          <span class="artifact-title">{{ data?.name || basename(path) }}</span>
          <span v-if="data" class="artifact-size">{{ formatBytes(data.size) }}</span>
          <span class="artifact-spacer" />
          <button class="artifact-btn" @click="openSystem">系统打开</button>
          <button
            class="artifact-btn artifact-close"
            aria-label="关闭"
            title="关闭（Esc）"
            @click="emit('close')"
          >
            ✕
          </button>
        </header>

        <div class="artifact-body">
          <div v-if="loading" class="artifact-hint">读取中…</div>
          <div v-else-if="error" class="artifact-hint error">{{ error }}</div>
          <img v-else-if="imageUrl" class="artifact-image" :src="imageUrl" alt="" />
          <img v-else-if="svgUrl" class="artifact-svg" :src="svgUrl" alt="" />
          <iframe
            v-else-if="data?.kind === 'html' && data.text !== undefined"
            class="artifact-frame"
            sandbox=""
            :srcdoc="data.text"
            referrerpolicy="no-referrer"
          />
          <div v-else class="artifact-hint">暂不支持预览该类型</div>
        </div>

        <footer class="artifact-foot">
          <span class="artifact-path" :title="path">{{ path }}</span>
        </footer>
      </div>
    </div>
  </Teleport>
</template>

<style scoped src="../styles/components/ArtifactViewer.css"></style>
