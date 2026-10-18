<script setup lang="ts">
/**
 * 新建会话草稿：选完 Agent 先给输入框，**发出第一条消息才真的建会话**。
 *
 * 为什么要这一步：以前点完 Agent 就建，聊不聊都会在「当前进行的会话」和归档里
 * 留下一个没名字的空壳，而现在前后端都没有删会话的能力——空壳只会越堆越多。
 *
 * 发送交给父级：建会话 → 开一列 → 由那一列的聊天框把这条消息发出去，
 * 这样流式输出、审批、收口走的都是和平时一模一样的那条路。
 */
import { computed, onMounted, ref } from 'vue'
import { basename } from '../format'

const props = withDefaults(
  defineProps<{
    /** 这次要用的 Agent 显示名（如「Prism 原生」）。 */
    engineLabel: string
    /** 这次要用的工作目录（绝对路径）。 */
    directory: string
    /** 正在建会话 / 正在发第一条消息。 */
    busy?: boolean
    error?: string | null
  }>(),
  { busy: false, error: null },
)

const emit = defineEmits<{
  (e: 'send', text: string): void
  /** 换个 Agent：回选择器。 */
  (e: 'change'): void
}>()

const text = ref('')
const box = ref<HTMLTextAreaElement | null>(null)
const focused = ref(false)

const canSend = computed(() => !!text.value.trim() && !props.busy)
const dirName = computed(() => basename(props.directory) || props.directory)

onMounted(() => box.value?.focus())

function send() {
  if (!canSend.value) return
  emit('send', text.value.trim())
}

/** 和会话页输入框同一套手感：Enter 发送，Shift+Enter 换行。 */
function onKeydown(event: KeyboardEvent) {
  if (event.key !== 'Enter' || event.shiftKey) return
  event.preventDefault()
  send()
}
</script>

<template>
  <section class="draft-pane" aria-label="发送第一条消息">
    <div class="draft-inner">
      <div class="draft-mark" aria-hidden="true">
        <svg viewBox="0 0 24 24" width="19" height="19">
          <path
            d="M13.4 3.2c.5 3.9 2.2 5.6 6.1 6.1-3.9.5-5.6 2.2-6.1 6.1-.5-3.9-2.2-5.6-6.1-6.1 3.9-.5 5.6-2.2 6.1-6.1Z"
            fill="none"
            stroke="currentColor"
            stroke-width="1.6"
            stroke-linejoin="round"
          />
          <path
            d="M6.4 14.6c.3 2.2 1.3 3.2 3.5 3.5-2.2.3-3.2 1.3-3.5 3.5-.3-2.2-1.3-3.2-3.5-3.5 2.2-.3 3.2-1.3 3.5-3.5Z"
            fill="none"
            stroke="currentColor"
            stroke-width="1.4"
            stroke-linejoin="round"
          />
        </svg>
      </div>

      <h2>用 {{ engineLabel }} 开始</h2>
      <p class="draft-desc">
        目录<b :title="directory">{{ dirName }}</b
        >· 发出第一条消息才真的建会话——不聊就不会留下空会话。
      </p>

      <div class="draft-box" :class="{ focused }">
        <textarea
          ref="box"
          v-model="text"
          class="draft-input"
          rows="3"
          :disabled="busy"
          placeholder="写下第一条指令…"
          @focus="focused = true"
          @blur="focused = false"
          @keydown="onKeydown"
        />
        <div class="draft-bar">
          <button
            class="draft-change"
            type="button"
            :disabled="busy"
            title="回选择器，换成别的 Agent"
            @click="emit('change')"
          >
            换个 Agent
          </button>
          <span class="draft-hint">Enter 发送 · Shift+Enter 换行</span>
          <button
            class="send-btn"
            type="button"
            :disabled="!canSend"
            :title="busy ? '正在建会话…' : '发送（Enter）'"
            :aria-label="busy ? '正在建会话' : '发送'"
            @click="send"
          >
            <svg
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="2.1"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <path d="M12 19V5M5.5 11.5 12 5l6.5 6.5" />
            </svg>
          </button>
        </div>
      </div>

      <p v-if="busy" class="draft-status">正在建会话…</p>
      <p v-if="error" class="draft-error">{{ error }}</p>
    </div>
  </section>
</template>

<style scoped src="../styles/components/NewSessionDraft.css"></style>
