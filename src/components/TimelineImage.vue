<script setup lang="ts">
/**
 * 时间线里的一张图：按 path 现取（消息里只存引用，不塞 base64）。
 *
 * 为什么要单独一个组件：一张图一次 IPC，加载失败也只影响这一张；
 * 放在时间线里直接读，会让每条消息都拖着一次请求。
 */
import { onMounted, ref } from 'vue'
import { api } from '../api'

const props = defineProps<{
  name: string
  mime: string
  path: string
  bytes: number
}>()

const src = ref('')
const failed = ref(false)

function sizeLabel(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`
}

onMounted(async () => {
  try {
    const data = await api.readArtifact(props.path)
    if (data.kind === 'image' && data.base64) {
      src.value = `data:${data.mime};base64,${data.base64}`
    } else if (data.kind === 'svg' && data.text) {
      src.value = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(data.text)}`
    } else {
      failed.value = true
    }
  } catch {
    failed.value = true
  }
})

/** 点图交给系统看图程序：嵌在时间线里的小图看不出细节。 */
function open() {
  if (!props.path) return
  void api.openPath(props.path).catch(() => {})
}
</script>

<template>
  <figure class="msg-image" :title="`${name} · ${sizeLabel(bytes)}`">
    <button v-if="src" class="msg-image-btn" type="button" :aria-label="`打开图片 ${name}`" @click="open">
      <img :src="src" :alt="name" loading="lazy" />
    </button>
    <span v-else class="msg-image-note">{{ failed ? '图片读不出来' : '加载中…' }}</span>
    <figcaption v-if="name" class="msg-image-name">{{ name }}</figcaption>
  </figure>
</template>

<style scoped src="../styles/components/TimelineImage.css"></style>
