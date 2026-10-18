<script setup lang="ts">
import { computed } from 'vue'
import type { TimelineRecord } from '../types'
import { renderMarkdown } from '../markdown'
import { recordText } from '../timeline'
import ToolCard from './ToolCard.vue'
import TurnMeta from './TurnMeta.vue'

const props = withDefaults(
  defineProps<{
    record: TimelineRecord
    sessionId: string
    /** 搜索定位命中的消息：短暂高亮。 */
    highlighted?: boolean
    /** 这一轮的用时（turnDurations 算好传进来；没有就不显示）。 */
    durationMs?: number | null
    /** 赛马栏自带 HUD，那边传 false 不重复画这行。 */
    showMeta?: boolean
  }>(),
  // 必须显式给默认值：Boolean 类型的 prop 没传时 Vue 会当 false。
  { highlighted: false, durationMs: null, showMeta: true },
)

const emit = defineEmits<{
  /** 工具卡里点了文件路径：交给会话视图开文件抽屉。 */
  (e: 'open-file', path: string): void
}>()

/** 复制按钮要复制的内容：这一轮的正文本体。 */
const copyText = computed(() => recordText(props.record))
</script>

<template>
  <div
    class="record"
    :class="[`role-${record.role}`, { highlighted }]"
    :data-message-id="record.id"
  >
    <!-- 用户消息：气泡 -->
    <template v-if="record.role === 'user'">
      <div class="user-bubble">
        <template v-for="(part, index) in record.parts" :key="index">
          <div
            v-if="part.kind === 'text'"
            class="user-text md"
            v-html="renderMarkdown(part.text)"
          />
        </template>
      </div>
      <TurnMeta v-if="showMeta" align="end" :copy-text="copyText" />
    </template>

    <!-- 系统提示：居中弱化 -->
    <template v-else-if="record.role === 'system'">
      <div class="system-note">
        <span v-for="(part, index) in record.parts" :key="index">
          {{ part.kind === 'text' ? part.text : '' }}
        </span>
      </div>
    </template>

    <!-- 助手消息：步骤序列 -->
    <template v-else>
      <div class="assistant-block">
        <div v-for="(part, index) in record.parts" :key="index" class="part">
          <details v-if="part.kind === 'reasoning'" class="reasoning">
            <summary>思考过程</summary>
            <div class="reasoning-body">{{ part.text }}</div>
          </details>
          <ToolCard
            v-else-if="part.kind === 'tool'"
            :part="part"
            :session-id="sessionId"
            :message-id="record.id"
            @open-file="emit('open-file', $event)"
          />
          <div
            v-else-if="part.kind === 'text' && part.text.trim()"
            class="assistant-text md"
            v-html="renderMarkdown(part.text)"
          />
        </div>

        <TurnMeta
          v-if="showMeta"
          :record="record"
          :duration-ms="durationMs"
          :copy-text="copyText"
        />
      </div>
    </template>
  </div>
</template>

<style scoped src="../styles/components/TimelineItem.css"></style>
