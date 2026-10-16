<script setup lang="ts">
import { computed, ref } from 'vue'
import { api } from '../api'
import type { TimelinePart, ToolDetail } from '../types'
import { parsePatch, patchStats, type DiffRow } from '../diff'
import { basename } from '../format'

const props = defineProps<{
  part: Extract<TimelinePart, { kind: 'tool' }>
  sessionId: string
  messageId: string
}>()

const expanded = ref(false)
const loading = ref(false)
const failed = ref<string | null>(null)
const detail = ref<ToolDetail | null>(null)

async function toggle() {
  expanded.value = !expanded.value
  if (!expanded.value || detail.value || loading.value) return
  loading.value = true
  failed.value = null
  try {
    detail.value = await api.readToolDetail(props.sessionId, props.messageId, props.part.callId)
  } catch (cause) {
    failed.value = cause instanceof Error ? cause.message : String(cause)
  } finally {
    loading.value = false
  }
}

interface ToolInput {
  command?: string
  workdir?: string
  code?: string
  path?: string
  pattern?: string
  query?: string
  url?: string
  content?: string
  oldString?: string
  newString?: string
  description?: string
  prompt?: string
  id?: string
  questions?: Array<{ question?: string }>
  [key: string]: unknown
}

const input = computed<ToolInput>(() => (detail.value?.input as ToolInput | null) ?? {})

const statusInfo = computed(() => {
  switch (props.part.status) {
    case 'completed':
      return { label: '完成', tone: 'ok' as const }
    case 'error':
    case 'failed':
      return { label: '失败', tone: 'error' as const }
    case 'pending':
    case 'running':
    case 'in_progress':
      return { label: '进行中', tone: 'warn' as const }
    default:
      return props.part.status
        ? { label: props.part.status, tone: 'muted' as const }
        : null
  }
})

/** 常见工具给一个小图标字符，未知工具用通用符号。 */
const toolGlyph = computed(() => {
  const name = props.part.name.toLowerCase()
  if (name.includes('shell') || name.includes('exec')) return '❯'
  if (name.includes('read')) return '◉'
  if (name.includes('edit') || name.includes('write') || name.includes('patch')) return '✎'
  if (name.includes('grep') || name.includes('search') || name.includes('glob')) return '⌕'
  if (name.includes('fetch') || name.includes('http') || name.includes('web')) return '⇄'
  if (name.includes('task') || name.includes('subagent')) return '⚙'
  return '▸'
})

/** 工具专属的主输入展示。 */
const primaryBlocks = computed<Array<{ label: string; text: string }>>(() => {
  const source = input.value
  const blocks: Array<{ label: string; text: string }> = []
  const push = (label: string, text: unknown) => {
    if (typeof text === 'string' && text.trim()) blocks.push({ label, text })
  }

  switch (props.part.name) {
    case 'shell':
      push('命令', source.command)
      push('工作目录', source.workdir)
      break
    case 'execute':
      push('代码', source.code)
      break
    case 'read':
      push('文件', source.path)
      break
    case 'write':
      push('写入', source.path)
      push('内容', source.content)
      break
    case 'edit':
      push('文件', source.path)
      break
    case 'grep':
    case 'glob':
      push('模式', source.pattern)
      push('路径', source.path)
      break
    case 'websearch':
      push('查询', source.query)
      break
    case 'webfetch':
      push('地址', source.url)
      break
    case 'skill':
      push('技能', source.id)
      break
    case 'question': {
      const questions = (source.questions ?? [])
        .map(item => item.question)
        .filter((text): text is string => typeof text === 'string' && text.length > 0)
      if (questions.length) blocks.push({ label: '问题', text: questions.join('\n') })
      break
    }
    case 'subagent':
      push('描述', source.description)
      push('指令', source.prompt)
      break
    default:
      break
  }
  return blocks
})

/** edit 的 unified diff（来自 metadata.files[].patch）。 */
const diffFiles = computed(() => {
  const metadata = detail.value?.metadata as
    | { files?: Array<{ file?: string; patch?: string }> }
    | null
  const files = metadata?.files ?? []
  return files
    .filter(file => typeof file.patch === 'string' && file.patch.length > 0)
    .map(file => {
      const rows = parsePatch(file.patch as string)
      return { file: file.file ?? '', rows, stats: patchStats(rows) }
    })
})

const fileLabel = (file: string) => basename(file) || file

/** edit 未带 patch 时的兜底：修改前 / 修改后对照。 */
const editFallback = computed(() => {
  if (props.part.name !== 'edit' || diffFiles.value.length > 0) return null
  const { oldString, newString } = input.value
  if (typeof oldString !== 'string' && typeof newString !== 'string') return null
  return { before: oldString ?? '（空）', after: newString ?? '（空）' }
})

/** 主块已完整覆盖的简单工具不再重复展示原始 JSON。 */
const rawInput = computed(() => {
  const source = input.value
  if (!source || Object.keys(source).length === 0) return null
  if (['read', 'grep', 'glob', 'webfetch', 'shell', 'write'].includes(props.part.name)) {
    return null
  }
  return JSON.stringify(source, null, 2)
})

function signOf(type: DiffRow['type']): string {
  if (type === 'add') return '+'
  if (type === 'del') return '−'
  if (type === 'hunk') return '@'
  return ' '
}
</script>

<template>
  <div class="tool-card" :class="{ expanded }">
    <button class="tool-head" @click="toggle">
      <span class="tool-glyph">{{ toolGlyph }}</span>
      <span class="tool-name">{{ part.name }}</span>
      <span v-if="part.summary" class="tool-summary" :title="part.summary">{{ part.summary }}</span>
      <span v-if="statusInfo" class="tool-status" :class="`tone-${statusInfo.tone}`">
        {{ statusInfo.label }}
      </span>
      <span class="tool-chevron">{{ expanded ? '▾' : '▸' }}</span>
    </button>

    <div v-if="expanded" class="tool-body">
      <div v-if="loading" class="tool-note">加载详情…</div>
      <div v-else-if="failed" class="tool-error">{{ failed }}</div>

      <template v-else-if="detail">
        <!-- diff 视图 -->
        <div v-for="(file, index) in diffFiles" :key="index" class="diff-block">
          <div class="diff-head">
            <span class="diff-file" :title="file.file">{{ fileLabel(file.file) }}</span>
            <span class="diff-stat">
              <span class="stat-add">+{{ file.stats.added }}</span>
              <span class="stat-del">−{{ file.stats.removed }}</span>
            </span>
          </div>
          <div class="diff-rows">
            <div
              v-for="(row, rowIndex) in file.rows"
              :key="rowIndex"
              class="diff-row"
              :class="`t-${row.type}`"
            >
              <span class="diff-sign">{{ signOf(row.type) }}</span>
              <span class="diff-text">{{ row.text || ' ' }}</span>
            </div>
          </div>
        </div>

        <!-- edit 兜底对照 -->
        <div v-if="editFallback" class="fallback-block">
          <div class="block-label">修改前</div>
          <pre class="tool-pre diff-del-bg">{{ editFallback.before }}</pre>
          <div class="block-label">修改后</div>
          <pre class="tool-pre diff-add-bg">{{ editFallback.after }}</pre>
        </div>

        <!-- 主输入块 -->
        <template v-for="(block, index) in primaryBlocks" :key="`p-${index}`">
          <div class="block-label">{{ block.label }}</div>
          <pre class="tool-pre">{{ block.text }}</pre>
        </template>

        <!-- 错误 -->
        <div v-if="detail.error" class="tool-error">{{ detail.error }}</div>

        <!-- 输出 -->
        <template v-if="detail.output">
          <div class="block-label">输出{{ detail.truncated ? '（已截断）' : '' }}</div>
          <pre class="tool-pre output">{{ detail.output }}</pre>
        </template>

        <!-- 原始输入 -->
        <details v-if="rawInput" class="raw-input">
          <summary>原始输入 JSON</summary>
          <pre class="tool-pre">{{ rawInput }}</pre>
        </details>
      </template>
    </div>
  </div>
</template>

<style scoped>
.tool-card {
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--panel-2);
  max-width: 780px;
  transition: border-color 0.15s ease;
}

.tool-card.expanded {
  border-color: var(--border-strong);
}

.tool-head {
  display: flex;
  align-items: center;
  gap: 7px;
  width: 100%;
  text-align: left;
  padding: 6px 10px;
  border-radius: var(--radius-sm);
}

.tool-head:hover {
  background: color-mix(in srgb, var(--accent) 5%, transparent);
}

.tool-glyph {
  color: var(--accent);
  font-family: var(--font-mono);
  font-size: 12px;
  width: 14px;
  text-align: center;
  flex: none;
}

.tool-name {
  font-family: var(--font-mono);
  font-size: 11.5px;
  font-weight: 600;
  color: var(--text);
  flex: none;
}

.tool-summary {
  font-family: var(--font-mono);
  font-size: 11px;
  color: var(--muted);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  flex: 1;
  min-width: 0;
}

.tool-status {
  font-size: 10.5px;
  padding: 0 7px;
  border-radius: 999px;
  line-height: 16px;
  flex: none;
}

.tool-status.tone-ok {
  color: var(--green);
  background: var(--green-soft);
}

.tool-status.tone-error {
  color: var(--red);
  background: var(--red-soft);
}

.tool-status.tone-warn {
  color: var(--amber);
  background: var(--amber-soft);
}

.tool-status.tone-muted {
  color: var(--muted);
  background: var(--panel);
  border: 1px solid var(--border);
}

.tool-chevron {
  color: var(--faint);
  font-size: 10px;
  flex: none;
}

.tool-body {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 4px 10px 10px;
}

.tool-note {
  color: var(--faint);
  font-size: 11.5px;
}

.tool-error {
  border: 1px solid color-mix(in srgb, var(--red) 35%, transparent);
  background: var(--red-soft);
  color: var(--red);
  border-radius: 6px;
  padding: 6px 10px;
  font-size: 12px;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}

.block-label {
  font-size: 10.5px;
  font-weight: 600;
  color: var(--faint);
  letter-spacing: 0.4px;
  margin-bottom: -4px;
}

.tool-pre {
  margin: 0;
  padding: 8px 10px;
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--panel);
  font-family: var(--font-mono);
  font-size: 11.5px;
  line-height: 1.6;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  max-height: 340px;
  overflow-y: auto;
}

.diff-block {
  border: 1px solid var(--border);
  border-radius: 6px;
  overflow: hidden;
  background: var(--panel);
}

.diff-head {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 5px 10px;
  background: var(--panel-2);
  border-bottom: 1px solid var(--border);
  font-family: var(--font-mono);
  font-size: 11px;
}

.diff-file {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-weight: 600;
}

.diff-stat {
  margin-left: auto;
  display: flex;
  gap: 8px;
  flex: none;
}

.stat-add {
  color: var(--green);
}

.stat-del {
  color: var(--red);
}

.diff-rows {
  max-height: 360px;
  overflow-y: auto;
  font-family: var(--font-mono);
  font-size: 11.5px;
  line-height: 1.55;
}

.diff-row {
  display: flex;
  gap: 6px;
  padding: 0 10px;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}

.diff-row.t-add {
  background: var(--green-soft);
}

.diff-row.t-del {
  background: var(--red-soft);
}

.diff-row.t-hunk {
  background: var(--panel-2);
  color: var(--faint);
  font-size: 10.5px;
}

.diff-sign {
  width: 10px;
  flex: none;
  color: var(--faint);
  user-select: none;
}

.diff-row.t-add .diff-sign {
  color: var(--green);
}

.diff-row.t-del .diff-sign {
  color: var(--red);
}

.diff-text {
  min-width: 0;
}

.diff-del-bg {
  background: color-mix(in srgb, var(--red-soft) 55%, var(--panel));
}

.diff-add-bg {
  background: color-mix(in srgb, var(--green-soft) 55%, var(--panel));
}

.fallback-block {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.raw-input summary {
  cursor: pointer;
  user-select: none;
  font-size: 11px;
  color: var(--faint);
  padding: 2px 0;
}

.raw-input {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
</style>
