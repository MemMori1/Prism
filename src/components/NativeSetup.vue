<script setup lang="ts">
/**
 * 新建原生会话前的配置弹窗：内容与 设置 → 模型管理 完全一致（同一个组件），
 * 只是包一层对话框，配完直接接着建会话。
 */
import { onBeforeUnmount, onMounted, ref } from 'vue'
import type { NativeConfigView } from '../types'
import ModelProviders from './ModelProviders.vue'

const emit = defineEmits<{
  (e: 'close'): void
  (e: 'saved'): void
}>()

const ready = ref(false)

onMounted(() => window.addEventListener('keydown', onKeydown))
onBeforeUnmount(() => window.removeEventListener('keydown', onKeydown))

function onKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape') emit('close')
}

function onChanged(config: NativeConfigView) {
  ready.value = config.ready
}

function done() {
  if (ready.value) emit('saved')
  else emit('close')
}
</script>

<template>
  <div class="ns-overlay" role="presentation" @click.self="emit('close')">
    <section class="ns-panel ns-panel-wide" role="dialog" aria-modal="true" aria-label="配置原生引擎">
      <header class="ns-head">
        <div class="ns-mark" aria-hidden="true">
          <svg viewBox="0 0 24 24" width="18" height="18">
            <path
              d="M12 3.4 20.6 12 12 20.6 3.4 12 12 3.4Z"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linejoin="round"
            />
            <path d="M12 8.1 15.9 12 12 15.9 8.1 12 12 8.1Z" fill="currentColor" opacity="0.55" />
          </svg>
        </div>
        <div class="ns-title">
          <h2>Prism 原生 · 模型服务</h2>
          <p>内置 agent 直连你的模型服务，不经过任何 CLI。配好就能直接建会话。</p>
        </div>
        <button class="ns-close" title="关闭" @click="emit('close')">✕</button>
      </header>

      <div class="ns-body">
        <ModelProviders @changed="onChanged" />
      </div>

      <footer class="ns-foot ns-foot-bar">
        <span class="ns-tip">
          想改这些设置，之后从 <strong>设置 → 模型</strong> 进来即可。
        </span>
        <span class="ns-spacer" />
        <button type="button" class="ns-ghost" @click="emit('close')">取消</button>
        <button type="button" class="ns-primary" :disabled="!ready" @click="done">
          {{ ready ? '完成，开始会话' : '先配一个模型' }}
        </button>
      </footer>
    </section>
  </div>
</template>

<style scoped src="../styles/components/NativeSetup.css"></style>
