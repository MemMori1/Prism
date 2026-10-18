<script setup lang="ts">
import { computed } from 'vue'
import { ENGINE_GLYPHS } from '../engineIcons'
import type { EngineChoice } from '../sessionStart'

const props = withDefaults(
  defineProps<{
    choices: EngineChoice[]
    title?: string
    description?: string
    selecting?: string | null
    hint?: string | null
    error?: string | null
    /** 整块锁住（工作台：还没选工作目录）：行点了没反应，视觉上也压暗。 */
    locked?: boolean
    /** 上次用的 Agent：那一行标「上次使用」，新建会话默认就是它。 */
    recent?: string | null
  }>(),
  {
    title: '选择本次任务使用的 Agent',
    description: '工作目录已经准备好。选择一个 Agent 后，就可以输入第一条消息。',
    selecting: null,
    hint: null,
    error: null,
    locked: false,
    recent: null,
  },
)

const emit = defineEmits<{
  (e: 'select', engine: string): void
}>()

/** 行内状态标签：只有需要提醒的才出现，能用的行不加噪。 */
function chipOf(choice: EngineChoice): { text: string; tone: string } | null {
  if (choice.checking) return { text: '检测中', tone: 'busy' }
  if (choice.needsSetup) return { text: '未配置 API Key', tone: 'warn' }
  if (!choice.available) return { text: '当前不可用', tone: 'off' }
  return null
}

/** 行尾转圈：健康检查中 / 正在创建会话。 */
function spinning(choice: EngineChoice): boolean {
  return props.selecting === choice.id || choice.checking
}

const rows = computed(() =>
  props.choices.map(choice => ({
    choice,
    chip: chipOf(choice),
    spinning: spinning(choice),
    /** 上次用的那一行：标出来，省得每次新建都要重新挑。 */
    recent: !!props.recent && props.recent === choice.id,
    /** 品牌 mark（没有就退回下面的首字母）。 */
    glyph: ENGINE_GLYPHS[choice.icon] ?? null,
  })),
)

function pick(choice: EngineChoice) {
  if (props.locked || !choice.available || choice.checking || props.selecting) return
  emit('select', choice.id)
}
</script>

<template>
  <section class="engine-picker" :class="{ locked }" aria-label="选择本次任务使用的 Agent">
    <div class="engine-picker-inner">
      <div class="picker-mark" aria-hidden="true">
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

      <h2>{{ title }}</h2>
      <p class="picker-desc">{{ description }}</p>

      <!-- 一行一个 Agent：以后加引擎只是多一行，不会把版面挤爆（超出就在整块里滚）。 -->
      <div class="engine-list">
        <button
          v-for="row in rows"
          :key="row.choice.id"
          class="engine-row"
          :class="{
            unavailable: !row.choice.available && !row.choice.checking,
            choosing: selecting === row.choice.id,
          }"
          :disabled="locked || !row.choice.available || row.choice.checking || !!selecting"
          :aria-label="row.choice.label"
          @click="pick(row.choice)"
        >
          <span class="engine-tile" :class="`icon-${row.choice.icon}`" aria-hidden="true">
            <!-- Prism 原生：自家 mark（棱镜 + 入射光，出射三束用品牌色）。 -->
            <svg
              v-if="row.choice.icon === 'prism'"
              viewBox="0 0 24 24"
              width="21"
              height="21"
            >
              <path
                d="M9.8 4.4 18.9 19.2H0.7L9.8 4.4Z"
                fill="none"
                stroke="currentColor"
                stroke-width="1.5"
                stroke-linejoin="round"
              />
              <path
                d="M0.9 11.9h6.4"
                fill="none"
                stroke="currentColor"
                stroke-width="1.5"
                stroke-linecap="round"
              />
              <path
                d="M14.6 12.9 22.4 8.2"
                fill="none"
                stroke="#8b5cf6"
                stroke-width="1.5"
                stroke-linecap="round"
              />
              <path
                d="M15.6 12.9h7.6"
                fill="none"
                stroke="#22d3ee"
                stroke-width="1.5"
                stroke-linecap="round"
              />
              <path
                d="M14.6 12.9l7.8 4.7"
                fill="none"
                stroke="#fb7185"
                stroke-width="1.5"
                stroke-linecap="round"
              />
            </svg>
            <!-- 各家品牌 mark（见 engineIcons.ts），用引擎色填充。 -->
            <svg v-else-if="row.glyph" viewBox="0 0 24 24" width="21" height="21">
              <path v-for="(d, index) in row.glyph" :key="index" :d="d" fill="currentColor" />
            </svg>
            <!-- 没配图标的引擎：首字母兜底，不会开天窗。 -->
            <span v-else class="letter-glyph">{{ row.choice.label.slice(0, 1) }}</span>
          </span>

          <span class="engine-main">
            <span class="engine-title">
              <span class="engine-name">{{ row.choice.label }}</span>
              <span v-if="row.chip" class="engine-chip" :class="row.chip.tone">
                {{ row.chip.text }}
              </span>
              <span v-if="row.recent" class="engine-chip recent">上次使用</span>
            </span>
            <span class="engine-desc">{{ row.choice.description }}</span>
          </span>

          <span class="engine-go" aria-hidden="true">
            <svg v-if="row.spinning" class="spin" viewBox="0 0 24 24" width="15" height="15">
              <path
                d="M12 3.8a8.2 8.2 0 1 0 8.2 8.2"
                fill="none"
                stroke="currentColor"
                stroke-width="2.2"
                stroke-linecap="round"
              />
            </svg>
            <svg v-else viewBox="0 0 24 24" width="16" height="16">
              <path
                d="m9.6 5.6 6.4 6.4-6.4 6.4"
                fill="none"
                stroke="currentColor"
                stroke-width="1.9"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
            </svg>
          </span>
        </button>
      </div>

      <p v-if="hint" class="picker-hint">{{ hint }}</p>
      <p v-if="error" class="picker-error" role="alert">{{ error }}</p>
    </div>
  </section>
</template>

<style scoped src="../styles/components/EngineChoicePanel.css"></style>
