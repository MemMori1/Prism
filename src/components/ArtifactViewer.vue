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

<style scoped>
.artifact-overlay {
  position: fixed;
  inset: 0;
  z-index: 60;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 36px;
  background: rgb(12 10 24 / 0.5);
}

.artifact-panel {
  display: flex;
  flex-direction: column;
  width: min(1040px, 92vw);
  height: min(760px, 88vh);
  background: var(--panel);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-lg);
  overflow: hidden;
  box-shadow: 0 24px 80px rgb(12 10 24 / 0.35);
}

.artifact-head {
  flex: none;
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 9px 14px;
  border-bottom: 1px solid var(--border);
}

.artifact-title {
  font-size: 13px;
  font-weight: 650;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.artifact-size {
  flex: none;
  font-size: 11px;
  color: var(--faint);
}

.artifact-spacer {
  flex: 1;
}

.artifact-btn {
  flex: none;
  border: 1px solid var(--border);
  background: transparent;
  color: var(--muted);
  border-radius: var(--radius-sm);
  padding: 3px 10px;
  font-size: 11.5px;
  cursor: pointer;
}

.artifact-btn:hover {
  color: var(--text);
  border-color: var(--border-strong);
}

.artifact-body {
  flex: 1;
  min-height: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  overflow: auto;
  background: var(--panel-2);
}

.artifact-image,
.artifact-svg {
  max-width: 100%;
  max-height: 100%;
  object-fit: contain;
}

.artifact-frame {
  width: 100%;
  height: 100%;
  border: none;
  background: #fff;
}

.artifact-hint {
  color: var(--faint);
  font-size: 12.5px;
}

.artifact-hint.error {
  color: var(--red);
  max-width: 70%;
  text-align: center;
}

.artifact-foot {
  flex: none;
  padding: 6px 14px;
  border-top: 1px solid var(--border);
  font-size: 11px;
  color: var(--faint);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
</style>
