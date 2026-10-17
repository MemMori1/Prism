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
  /** 运行中（live）工具自带详情，跳过 DB 拉取。 */
  liveDetail?: ToolDetail | null
}>()

const expanded = ref(false)
const loading = ref(false)
const failed = ref<string | null>(null)
const detail = ref<ToolDetail | null>(null)

/** live 详情优先，其次是按需拉取的 DB 详情。 */
const resolved = computed<ToolDetail | null>(() => props.liveDetail ?? detail.value)

/** 历史数据里偶有缺失 id 的工具片段，兜底避免拿空参数调后端。 */
const callId = computed(() => {
  const part = props.part as { callId?: string; call_id?: string }
  return part.callId ?? part.call_id ?? ''
})

async function toggle() {
  expanded.value = !expanded.value
  if (!expanded.value || props.liveDetail || detail.value || loading.value) return
  if (!callId.value) {
    failed.value = '这条工具调用没有 id，无法加载详情'
    return
  }
  loading.value = true
  failed.value = null
  try {
    detail.value = await api.readToolDetail(props.sessionId, props.messageId, callId.value)
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
  /** Claude Code 的入参风格（snake_case）。 */
  file_path?: string
  pattern?: string
  query?: string
  url?: string
  content?: string
  oldString?: string
  newString?: string
  old_string?: string
  new_string?: string
  description?: string
  prompt?: string
  id?: string
  questions?: Array<{ question?: string }>
  [key: string]: unknown
}

const input = computed<ToolInput>(() => (resolved.value?.input as ToolInput | null) ?? {})

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
  if (name.includes('shell') || name.includes('exec') || name.includes('bash')) return '❯'
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
    case 'bash':
      push('命令', source.command)
      push('工作目录', source.workdir)
      break
    case 'execute':
      push('代码', source.code)
      break
    case 'read':
      push('文件', source.path ?? source.file_path)
      break
    case 'write':
      push('写入', source.path ?? source.file_path)
      push('内容', source.content)
      break
    case 'edit':
      push('文件', source.path ?? source.file_path)
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
    case 'task':
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
  const metadata = resolved.value?.metadata as
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

/** edit 未带 patch 时的兜底：修改前 / 修改后对照（兼容 Claude 的 snake_case 入参）。 */
const editFallback = computed(() => {
  if (props.part.name !== 'edit' || diffFiles.value.length > 0) return null
  const before = input.value.oldString ?? input.value.old_string
  const after = input.value.newString ?? input.value.new_string
  if (typeof before !== 'string' && typeof after !== 'string') return null
  return { before: before ?? '（空）', after: after ?? '（空）' }
})

/** 主块已完整覆盖的简单工具不再重复展示原始 JSON。 */
const rawInput = computed(() => {
  const source = input.value
  if (!source || Object.keys(source).length === 0) return null
  if (['read', 'grep', 'glob', 'webfetch', 'shell', 'bash', 'write'].includes(props.part.name)) {
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

      <template v-else-if="resolved">
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
        <div v-if="resolved.error" class="tool-error">{{ resolved.error }}</div>

        <!-- 输出 -->
        <template v-if="resolved.output">
          <div class="block-label">输出{{ resolved.truncated ? '（已截断）' : '' }}</div>
          <pre class="tool-pre output">{{ resolved.output }}</pre>
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

<style scoped src="../styles/components/ToolCard.css"></style>
