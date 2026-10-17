<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { api } from '../api'
import { onEngineEvent } from '../engineEvents'
import { createLiveTurn, reduceLiveTurn, type LiveTurn } from '../liveTurn'
import type { ProjectItem, TimelineRecord } from '../types'
import { basename, formatCost, formatTokens } from '../format'
import LiveTurnView from './LiveTurn.vue'
import TimelineItem from './TimelineItem.vue'

const props = defineProps<{
  projects: ProjectItem[]
}>()

const emit = defineEmits<{
  (e: 'error', message: string): void
  (e: 'open-session', payload: { engine: string; directory: string; sessionId: string }): void
}>()

const ENGINE_LABELS: Record<string, string> = { opencode: 'OpenCode', codex: 'Codex' }
const RACE_ENGINES = ['opencode', 'codex']
const TERMINAL_EVENTS = new Set([
  'session.execution.succeeded',
  'session.execution.failed',
  'session.execution.interrupted',
])

type RaceStatus = 'starting' | 'running' | 'stopping' | 'done' | 'failed' | 'stopped'

interface RaceTarget {
  key: number
  engine: string
}

interface RaceColumn {
  key: number
  engine: string
  directory: string
  sessionId: string | null
  status: RaceStatus
  live: LiveTurn
  startedAt: number | null
  elapsedMs: number
  record: TimelineRecord | null
  error: string
  finalized: boolean
}

const prompt = ref('')
const directory = ref('')
const targets = ref<RaceTarget[]>([
  { key: 1, engine: 'opencode' },
  { key: 2, engine: 'codex' },
])
let nextKey = 3
const columns = ref<RaceColumn[]>([])

watch(
  () => props.projects,
  list => {
    if (!directory.value && list.length) {
      directory.value = list[0].worktree
    }
  },
  { immediate: true },
)

/** 可选工作目录（按路径去重，引擎重复出现的只留一条）。 */
const directoryOptions = computed(() => {
  const seen = new Set<string>()
  const options: Array<{ value: string; label: string }> = []
  for (const project of props.projects) {
    if (seen.has(project.worktree)) continue
    seen.add(project.worktree)
    options.push({
      value: project.worktree,
      label: `${basename(project.worktree)} · ${ENGINE_LABELS[project.engine] ?? project.engine}`,
    })
  }
  return options
})

const racing = computed(() =>
  columns.value.some(
    column =>
      column.status === 'starting' || column.status === 'running' || column.status === 'stopping',
  ),
)
const canStart = computed(
  () => !!prompt.value.trim() && !!directory.value && targets.value.length > 0 && !racing.value,
)
const canRerun = computed(() => !racing.value && columns.value.length > 0 && !!prompt.value.trim())
const winnerKey = computed(() => {
  const finished = columns.value.filter(column => column.status === 'done' && column.elapsedMs > 0)
  if (finished.length < 2) return null
  return finished.reduce((best, column) => (column.elapsedMs < best.elapsedMs ? column : best)).key
})

function addTarget() {
  if (targets.value.length >= 4) return
  targets.value = [...targets.value, { key: nextKey, engine: 'opencode' }]
  nextKey += 1
}

function removeTarget(key: number) {
  if (targets.value.length <= 1) return
  targets.value = targets.value.filter(target => target.key !== key)
}

function delay(ms: number): Promise<void> {
  return new Promise(resolve => window.setTimeout(resolve, ms))
}

// ---------- 计时 / 轮询 ----------

let tickTimer: number | null = null
let pollTimer: number | null = null

function startTimers() {
  stopTimers()
  tickTimer = window.setInterval(() => {
    const now = Date.now()
    for (const column of columns.value) {
      if (column.status === 'running' && column.startedAt) {
        column.elapsedMs = now - column.startedAt
      }
    }
    if (!racing.value) stopTimers()
  }, 500)
  pollTimer = window.setInterval(() => {
    void pollColumns()
  }, 1500)
}

function stopTimers() {
  if (tickTimer !== null) {
    window.clearInterval(tickTimer)
    tickTimer = null
  }
  if (pollTimer !== null) {
    window.clearInterval(pollTimer)
    pollTimer = null
  }
}

/** 兜底：事件可能丢，直接看时间线里是否已有完成的助手记录。 */
async function pollColumns() {
  for (const column of columns.value) {
    if (column.status !== 'running' || !column.sessionId || column.finalized) continue
    const sessionId = column.sessionId
    try {
      const page = await api.readTimeline(sessionId, undefined, 6)
      if (column.sessionId !== sessionId) continue
      const finish = [...page.records]
        .reverse()
        .find(
          record =>
            record.role === 'assistant' &&
            record.completed &&
            (!column.startedAt || record.timeCreated >= column.startedAt - 5000),
        )
      if (finish) {
        await finalize(column, undefined, finish)
      }
    } catch {
      // 轮询失败不打扰用户（引擎可能刚写入）。
    }
  }
}

// ---------- 引擎事件 ----------

const unsubscribe = onEngineEvent(event => {
  const sessionId = typeof event.data?.sessionID === 'string' ? event.data.sessionID : null
  if (!sessionId) return
  const column = columns.value.find(item => item.sessionId === sessionId)
  if (!column) return
  const before = column.live
  const after = reduceLiveTurn(before, event)
  if (after !== before) column.live = after
  if (TERMINAL_EVENTS.has(event.type)) {
    void finalize(column, event.type)
  }
})

onBeforeUnmount(() => {
  unsubscribe()
  stopTimers()
})

/** 收尾：冻结用时、取最终记录（DB 可能略滞后，重试几次）。 */
async function finalize(
  column: RaceColumn,
  eventType: string | undefined,
  record?: TimelineRecord,
) {
  if (column.finalized) return
  column.finalized = true
  if (column.startedAt) column.elapsedMs = Date.now() - column.startedAt
  if (record) column.record = record

  const sessionId = column.sessionId
  for (let attempt = 0; sessionId && !column.record && attempt < 4; attempt += 1) {
    await delay(attempt === 0 ? 200 : 600)
    if (column.sessionId !== sessionId) return
    try {
      const page = await api.readTimeline(sessionId, undefined, 8)
      const assistant = [...page.records]
        .reverse()
        .find(
          item =>
            item.role === 'assistant' &&
            item.completed &&
            (!column.startedAt || item.timeCreated >= column.startedAt - 5000),
        )
      if (assistant) column.record = assistant
    } catch {
      // 忽略瞬时错误，继续重试。
    }
  }

  if (eventType?.endsWith('.failed')) {
    column.status = 'failed'
    column.error = column.live.error ?? '执行失败'
  } else if (eventType?.endsWith('.interrupted')) {
    column.status = 'stopped'
  } else {
    column.status = 'done'
  }
  if (!racing.value) stopTimers()
}

function failColumn(column: RaceColumn, cause: unknown) {
  column.status = 'failed'
  column.finalized = true
  column.error = cause instanceof Error ? cause.message : String(cause)
  if (!racing.value) stopTimers()
}

// ---------- 启动 / 停止 / 重置 ----------

async function start() {
  if (!canStart.value) return
  const text = prompt.value.trim()
  const dir = directory.value

  columns.value = targets.value.map(target => ({
    key: target.key,
    engine: target.engine,
    directory: dir,
    sessionId: null,
    status: 'starting',
    live: createLiveTurn(),
    startedAt: null,
    elapsedMs: 0,
    record: null,
    error: '',
    finalized: false,
  }))
  startTimers()

  // 先并行建会话，再同时开跑（保证各列起跑时间一致）。
  await Promise.all(
    columns.value.map(async column => {
      try {
        const created = await api.sessionCreate(dir, column.engine)
        column.sessionId = created.sessionId
      } catch (cause) {
        failColumn(column, cause)
      }
    }),
  )
  const ready = columns.value.filter(column => column.sessionId)
  await delay(250)
  const now = Date.now()
  for (const column of ready) {
    column.status = 'running'
    column.startedAt = now
  }
  await Promise.all(
    ready.map(async column => {
      try {
        await api.sessionSend(column.sessionId as string, text)
      } catch (cause) {
        failColumn(column, cause)
      }
    }),
  )
  if (!ready.length) stopTimers()
}

async function stop(column: RaceColumn) {
  if (!column.sessionId || column.status !== 'running') return
  column.status = 'stopping'
  try {
    await api.sessionInterrupt(column.sessionId)
  } catch (cause) {
    emit('error', `中断失败：${cause instanceof Error ? cause.message : String(cause)}`)
  }
  await finalize(column, 'session.execution.interrupted')
}

function stopAll() {
  for (const column of columns.value) {
    if (column.status === 'running') void stop(column)
  }
}

function reset() {
  stopTimers()
  columns.value = []
}

function openArchive(column: RaceColumn) {
  if (!column.sessionId) return
  emit('open-session', {
    engine: column.engine,
    directory: column.directory,
    sessionId: column.sessionId,
  })
}

const STATUS_LABELS: Record<RaceStatus, string> = {
  starting: '启动中',
  running: '执行中',
  stopping: '停止中',
  done: '完成',
  failed: '失败',
  stopped: '已停止',
}

function elapsedLabel(ms: number): string {
  if (!ms) return '--'
  const seconds = Math.floor(ms / 1000)
  const minutes = Math.floor(seconds / 60)
  return minutes ? `${minutes}m${String(seconds % 60).padStart(2, '0')}s` : `${seconds}s`
}

function opencodeLabel(engine: string): string {
  return ENGINE_LABELS[engine] ?? engine
}
</script>

<template>
  <section class="race">
    <div v-if="!columns.length" class="race-config">
      <h2 class="race-title">赛马</h2>
      <p class="race-desc">
        同一个问题同时发给多个引擎，答案、用时、token 与成本并排对比。开跑即新建各自会话，随时可中断。
      </p>

      <label class="field">
        <span class="field-label">问题</span>
        <textarea
          v-model="prompt"
          class="prompt-input"
          rows="4"
          placeholder="例如：给这个仓库加一个 /healthz 健康检查端点，并写一条测试。"
        />
      </label>

      <label class="field">
        <span class="field-label">工作目录</span>
        <select v-model="directory" class="dir-select">
          <option v-for="option in directoryOptions" :key="option.value" :value="option.value">
            {{ option.label }}
          </option>
        </select>
      </label>

      <div class="field">
        <span class="field-label">参赛选手（{{ targets.length }} 列）</span>
        <div class="targets">
          <div v-for="target in targets" :key="target.key" class="target">
            <select v-model="target.engine" class="engine-select">
              <option v-for="engine in RACE_ENGINES" :key="engine" :value="engine">
                {{ opencodeLabel(engine) }}
              </option>
            </select>
            <button
              class="target-remove"
              title="移除这列"
              :disabled="targets.length <= 1"
              @click="removeTarget(target.key)"
            >
              ✕
            </button>
          </div>
          <button class="target-add" :disabled="targets.length >= 4" @click="addTarget">
            ＋ 加一列
          </button>
        </div>
      </div>

      <button class="start-btn" :disabled="!canStart" @click="start">🏁 开跑</button>
      <p v-if="!props.projects.length" class="race-hint">
        还没有可用工作目录：先在归档里浏览一次项目，或让引擎在该目录新建过会话。
      </p>
    </div>

    <template v-else>
      <header class="race-bar">
        <span class="race-quest" :title="prompt">{{ prompt }}</span>
        <button v-if="racing" class="bar-btn danger" @click="stopAll">⏹ 全部停止</button>
        <button v-else class="bar-btn" @click="start">🔁 同题再跑</button>
        <button class="bar-btn" @click="reset">重新配置</button>
      </header>

      <div class="race-lanes">
        <div v-for="column in columns" :key="column.key" class="lane">
          <header class="lane-head">
            <span class="lane-engine">{{ opencodeLabel(column.engine) }}</span>
            <span v-if="column.key === winnerKey" class="lane-crown" title="完成最快">🏁</span>
            <span class="lane-status" :class="`st-${column.status}`">
              {{ STATUS_LABELS[column.status] }}
            </span>
            <span class="lane-elapsed">⏱ {{ elapsedLabel(column.elapsedMs) }}</span>
            <button
              v-if="column.status === 'running'"
              class="lane-btn"
              title="中断这列"
              @click="stop(column)"
            >
              停止
            </button>
            <button
              class="lane-btn"
              title="在归档里打开这个会话"
              :disabled="!column.sessionId"
              @click="openArchive(column)"
            >
              档案
            </button>
          </header>
          <div v-if="column.record?.tokens" class="lane-hud">
            <span class="hud-item" title="未缓存输入">
              ↑ {{ formatTokens(column.record.tokens.input) }}
            </span>
            <span v-if="column.record.tokens.cacheRead" class="hud-item" title="缓存读">
              ⚡ {{ formatTokens(column.record.tokens.cacheRead) }}
            </span>
            <span class="hud-item" title="输出">↓ {{ formatTokens(column.record.tokens.output) }}</span>
            <span v-if="column.record.tokens.reasoning" class="hud-item" title="思考">
              ✦ {{ formatTokens(column.record.tokens.reasoning) }}
            </span>
            <span v-if="column.record.tokens.cost" class="hud-item hud-cost" title="该轮成本">
              {{ formatCost(column.record.tokens.cost) }}
            </span>
          </div>

          <div class="lane-body">
            <div v-if="column.error" class="lane-error">{{ column.error }}</div>
            <TimelineItem
              v-else-if="column.record && column.sessionId"
              :record="column.record"
              :session-id="column.sessionId"
              :highlighted="false"
            />
            <LiveTurnView v-else-if="column.live.parts.length || column.live.running" :turn="column.live" />
            <div v-else class="lane-hint">
              {{ column.status === 'starting' ? '正在新建会话…' : '等待输出…' }}
            </div>
          </div>
        </div>
      </div>
    </template>
  </section>
</template>

<style scoped src="../styles/components/RacePane.css"></style>
