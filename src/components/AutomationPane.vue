<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { api } from '../api'
import { fullTime, relativeTime } from '../format'
import { onRoutineEvent } from '../routineEvents'
import { describeCron, statusLabel, statusTone, untilText } from '../routines'
import { engineLabel } from '../workshop'
import type { RoutineDefinition, RoutineLogEntry } from '../types'
import RoutineDialog from './RoutineDialog.vue'

const emit = defineEmits<{
  (e: 'error', message: string): void
  (e: 'openSession', payload: { engine: string; directory: string; sessionId: string }): void
}>()

const routines = ref<RoutineDefinition[]>([])
const selectedId = ref<string | null>(null)
const logs = ref<RoutineLogEntry[]>([])
const loading = ref(false)
const logsLoading = ref(false)
const dialogMode = ref<'create' | 'edit' | null>(null)
const busyId = ref<string | null>(null)

const selected = computed(
  () => routines.value.find(item => item.id === selectedId.value) ?? null,
)

function report(cause: unknown) {
  emit('error', cause instanceof Error ? cause.message : String(cause))
}

async function load(preferId?: string) {
  loading.value = true
  try {
    routines.value = await api.routinesList()
    const target = preferId ?? selectedId.value
    const exists = target ? routines.value.some(item => item.id === target) : false
    selectedId.value = exists ? (target as string) : (routines.value[0]?.id ?? null)
    await loadLogs()
  } catch (cause) {
    report(cause)
  } finally {
    loading.value = false
  }
}

async function loadLogs() {
  const id = selectedId.value
  if (!id) {
    logs.value = []
    return
  }
  logsLoading.value = true
  try {
    const loaded = await api.routineLogs(id)
    if (selectedId.value !== id) return
    logs.value = loaded
  } catch (cause) {
    report(cause)
  } finally {
    logsLoading.value = false
  }
}

function selectRoutine(id: string) {
  if (selectedId.value === id) return
  selectedId.value = id
  void loadLogs()
}

async function runNow(item: RoutineDefinition) {
  if (busyId.value) return
  busyId.value = item.id
  try {
    await api.routineRun(item.id)
    // 运行开始后状态（running）由列表刷新反映，不必马上等结果。
    window.setTimeout(() => {
      void load(item.id)
    }, 800)
  } catch (cause) {
    report(cause)
  } finally {
    busyId.value = null
  }
}

async function toggleEnabled(item: RoutineDefinition) {
  try {
    await api.routineUpdate(item.id, { enabled: !item.enabled })
    await load(item.id)
  } catch (cause) {
    report(cause)
  }
}

async function remove(item: RoutineDefinition) {
  if (!window.confirm(`删除任务「${item.name}」？（执行日志会保留在 ~/.prism）`)) return
  try {
    await api.routineDelete(item.id)
    await load()
  } catch (cause) {
    report(cause)
  }
}

function openLogSession(entry: RoutineLogEntry) {
  if (!entry.sessionId) return
  emit('openSession', {
    engine: entry.engine,
    directory: entry.directory,
    sessionId: entry.sessionId,
  })
}

function onRoutineSaved(id: string) {
  dialogMode.value = null
  void load(id)
}

function durationText(entry: RoutineLogEntry): string {
  if (!entry.finishedAt) return '—'
  const seconds = Math.max(0, Math.round((entry.finishedAt - entry.startedAt) / 1000))
  if (seconds < 60) return `${seconds}s`
  return `${Math.floor(seconds / 60)}m${seconds % 60}s`
}

const stopEvents = onRoutineEvent(() => {
  // 运行结果 / 权限请求都会到：刷新列表与当日志（选中项）。
  void load(selectedId.value ?? undefined)
})

onMounted(() => {
  void load()
})

onBeforeUnmount(stopEvents)
</script>

<template>
  <div class="automation-pane">
    <section class="routine-list">
      <header class="list-head">
        <span class="list-title">定时任务</span>
        <button class="new-btn" @click="dialogMode = 'create'">新建</button>
      </header>
      <div class="list-body">
        <div v-if="loading && !routines.length" class="hint">加载中…</div>
        <div v-else-if="!routines.length" class="hint">
          还没有定时任务。点「新建」安排一个——例如「工作日 09:30 汇总昨日进展」。
        </div>
        <button
          v-for="item in routines"
          :key="item.id"
          class="routine-row"
          :class="{ active: item.id === selectedId }"
          @click="selectRoutine(item.id)"
        >
          <span class="row-main">
            <span class="row-name">{{ item.name }}</span>
            <span class="row-meta">
              {{ describeCron(item.schedule) }} · {{ engineLabel(item.engine) }}
            </span>
          </span>
          <span
            class="state-dot"
            :class="item.running ? 'tone-run' : item.enabled ? `tone-${statusTone(item.lastStatus)}` : 'tone-idle'"
            :title="item.running ? '运行中' : item.enabled ? statusLabel(item.lastStatus) : '已停用'"
          />
        </button>
      </div>
    </section>

    <section class="routine-detail">
      <div v-if="!selected" class="hint detail-hint">左侧选择一个任务查看详情</div>
      <template v-else>
        <header class="detail-head">
          <div class="detail-title">
            <span class="name">{{ selected.name }}</span>
            <span class="chip" :class="`tone-${statusTone(selected.lastStatus)}`">
              {{ statusLabel(selected.lastStatus) }}
            </span>
            <span v-if="!selected.enabled" class="chip tone-idle">已停用</span>
            <span v-if="selected.running" class="chip tone-run">运行中</span>
          </div>
          <div class="detail-actions">
            <button
              class="mini-btn"
              :disabled="selected.running || busyId === selected.id"
              @click="runNow(selected)"
            >
              立即运行
            </button>
            <button class="mini-btn" @click="toggleEnabled(selected)">
              {{ selected.enabled ? '停用' : '启用' }}
            </button>
            <button class="mini-btn" @click="dialogMode = 'edit'">编辑</button>
            <button class="mini-btn danger" @click="remove(selected)">删除</button>
          </div>
        </header>

        <dl class="detail-grid">
          <dt>计划</dt>
          <dd>
            <code class="mono">{{ selected.schedule }}</code>
            <span class="faint">（{{ describeCron(selected.schedule) }}）</span>
          </dd>
          <dt>引擎</dt>
          <dd>{{ engineLabel(selected.engine) }}</dd>
          <dt>下次触发</dt>
          <dd>
            <template v-if="selected.nextRun">
              {{ fullTime(selected.nextRun) }}
              <span class="faint">（{{ untilText(selected.nextRun) }}）</span>
            </template>
            <template v-else>—</template>
          </dd>
          <dt>上次运行</dt>
          <dd>
            <template v-if="selected.lastTrigger">
              {{ fullTime(selected.lastTrigger) }}
              <span class="faint">（{{ relativeTime(selected.lastTrigger) }}）</span>
            </template>
            <template v-else>—</template>
          </dd>
        </dl>

        <div class="block">
          <div class="block-title">提示词</div>
          <pre class="prompt-text">{{ selected.prompt }}</pre>
        </div>

        <div class="block log-block">
          <div class="block-title">
            执行日志
            <span v-if="logsLoading" class="faint">加载中…</span>
            <span v-else-if="!logs.length" class="faint">暂无执行记录</span>
          </div>
          <div v-for="entry in logs" :key="entry.startedAt" class="log-row">
            <span class="chip" :class="`tone-${statusTone(entry.status)}`">
              {{ statusLabel(entry.status) }}
            </span>
            <span class="log-time">{{ fullTime(entry.startedAt) }}</span>
            <span class="log-dur">{{ durationText(entry) }}</span>
            <span class="log-error" :title="entry.error ?? ''">{{ entry.error ?? '' }}</span>
            <button v-if="entry.sessionId" class="mini-btn" @click="openLogSession(entry)">
              打开会话
            </button>
          </div>
        </div>
      </template>
    </section>

    <RoutineDialog
      v-if="dialogMode"
      :mode="dialogMode"
      :routine="dialogMode === 'edit' ? selected : null"
      @close="dialogMode = null"
      @saved="onRoutineSaved"
      @error="report"
    />
  </div>
</template>

<style scoped src="../styles/components/AutomationPane.css"></style>
