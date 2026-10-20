<script setup lang="ts">
/**
 * 终端页（M23）：任务坞的「深水区」。
 *
 * 左＝终端任务列表（运行中 / 已结束），右＝完整输出 + 操作，或新建任务表单。
 * 和任务坞读同一份 `tasks`，坞里点「在终端打开」就是落到这里并选中那条。
 * 底部的输入行不是持久 shell：Enter 会在同一目录新起一条任务（前台任务不占坞）。
 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { copyPlainText } from '../clipboard'
import { relativeTime } from '../format'
import {
  TASK_STATE_CLASS,
  TASK_STATE_LABELS,
  formatElapsed,
  groupTasks,
  taskElapsed,
} from '../taskModel'
import { bootTasks, removeTask, spawnTask, stopTask, tasks, tasksLoadError } from '../tasks'
import type { BackgroundTask } from '../types'

const props = defineProps<{
  /** 会话工作目录（新建任务的默认 cwd）。 */
  directory: string | null
  sessionId: string | null
  /** 从任务坞点进来时直接选中这条。 */
  initialTaskId?: string | null
  /** 外部（任务坞「＋ 新任务」/ 标签页）每加一次就打开表单。 */
  newTaskTick?: number
}>()

const emit = defineEmits<{
  /** 当前选中的任务命令：标签上用它。 */
  (e: 'title', title: string | null): void
}>()

const selectedId = ref<string | null>(props.initialTaskId ?? null)
const form = ref(false)
const command = ref('')
const mode = ref<'bg' | 'fg'>('bg')
const runValue = ref('')
const now = ref(Date.now())
const busy = ref<string | null>(null)
const note = ref<string | null>(null)
const failure = ref<string | null>(null)
const consoleEl = ref<HTMLElement | null>(null)
const cmdInput = ref<HTMLTextAreaElement | null>(null)

bootTasks()

const groups = computed(() => groupTasks(tasks.value))
const groupsView = computed(() => [
  { key: 'running', label: '运行中', items: groups.value.running },
  { key: 'ended', label: '已结束', items: groups.value.ended },
])
const selected = computed<BackgroundTask | null>(() => {
  const list = tasks.value
  return list.find(task => task.id === selectedId.value) ?? list[0] ?? null
})

let timer = 0
let noteTimer = 0
let failTimer = 0

function setConsoleEl(el: unknown): void {
  consoleEl.value = (el as HTMLElement | null) ?? null
}

function scrollConsole(): void {
  const el = consoleEl.value
  if (el) el.scrollTop = el.scrollHeight
}

function followTail(): void {
  const el = consoleEl.value
  if (!el) return
  if (el.scrollHeight - el.scrollTop - el.clientHeight < 60) {
    el.scrollTop = el.scrollHeight
  }
}

function flash(text: string): void {
  note.value = text
  window.clearTimeout(noteTimer)
  noteTimer = window.setTimeout(() => {
    if (note.value === text) note.value = null
  }, 1800)
}

function messageOf(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause)
}

/** 终端页自己消化错误：侧栏里没有全局横幅，起任务失败要就地看见。 */
function fail(message: string): void {
  failure.value = message
  window.clearTimeout(failTimer)
  failTimer = window.setTimeout(() => {
    if (failure.value === message) failure.value = null
  }, 6000)
}

function stateClass(task: BackgroundTask): string {
  return TASK_STATE_CLASS[task.state]
}

function stateLabel(task: BackgroundTask): string {
  return TASK_STATE_LABELS[task.state]
}

function elapsedOf(task: BackgroundTask): string {
  return formatElapsed(taskElapsed(task, now.value))
}

function sinceOf(task: BackgroundTask): string {
  if (task.state === 'running') return `已跑 ${elapsedOf(task)}`
  return relativeTime(task.endedAt ?? task.startedAt)
}

function scopeOf(task: BackgroundTask): string {
  if (task.origin === 'agent') return 'Agent'
  if (!task.sessionId) return '独立任务'
  if (task.sessionId === props.sessionId) return '本会话'
  return task.title || '其它会话'
}

function selectTask(id: string): void {
  selectedId.value = id
  form.value = false
  void nextTick(scrollConsole)
}

function openForm(): void {
  form.value = true
  command.value = ''
  void nextTick(() => cmdInput.value?.focus())
}

function cancelForm(): void {
  form.value = false
}

async function runOn(id: string, action: () => Promise<void>): Promise<void> {
  if (busy.value) return
  busy.value = id
  try {
    await action()
  } catch (cause) {
    fail(messageOf(cause))
  } finally {
    busy.value = null
  }
}

function stop(task: BackgroundTask): void {
  void runOn(task.id, () => stopTask(task.id))
}

function rerun(task: BackgroundTask): void {
  void runOn(task.id, async () => {
    const created = await spawnTask({
      cmd: task.cmd,
      cwd: task.cwd,
      sessionId: props.sessionId,
      background: true,
    })
    selectedId.value = created.id
  })
}

function closeTask(task: BackgroundTask): void {
  void runOn(task.id, () => removeTask(task.id))
}

async function copyAll(): Promise<void> {
  const task = selected.value
  if (!task) return
  try {
    await copyPlainText(task.tail.join('\n'))
    flash('已复制全部输出')
  } catch (cause) {
    fail(messageOf(cause))
  }
}

async function startTask(): Promise<void> {
  const cmd = command.value.trim()
  const cwd = props.directory || selected.value?.cwd || ''
  if (!cmd) {
    fail('先写一条命令')
    return
  }
  if (!cwd) {
    fail('这个会话没有工作目录，无法起任务')
    return
  }
  if (busy.value) return
  busy.value = 'new'
  try {
    const created = await spawnTask({
      cmd,
      cwd,
      sessionId: props.sessionId,
      background: mode.value === 'bg',
    })
    selectedId.value = created.id
    form.value = false
    command.value = ''
    flash(mode.value === 'bg' ? '已加入后台任务坞' : '前台任务已启动')
    await nextTick()
    scrollConsole()
  } catch (cause) {
    fail(messageOf(cause))
  } finally {
    busy.value = null
  }
}

async function runCommand(): Promise<void> {
  const task = selected.value
  const cmd = runValue.value.trim()
  if (!task || !cmd || task.state === 'running' || busy.value) return
  busy.value = 'run'
  try {
    const created = await spawnTask({
      cmd,
      cwd: task.cwd,
      sessionId: props.sessionId,
      background: true,
    })
    selectedId.value = created.id
    runValue.value = ''
  } catch (cause) {
    fail(messageOf(cause))
  } finally {
    busy.value = null
  }
}

watch(
  () => props.initialTaskId,
  id => {
    if (id) {
      selectedId.value = id
      form.value = false
    }
  },
)

watch(
  () => props.newTaskTick ?? 0,
  (next, previous) => {
    if (next > (previous ?? 0)) openForm()
  },
)

watch(
  () => tasks.value.length,
  () => {
    if (!tasks.value.length) {
      selectedId.value = null
      form.value = false
      return
    }
    if (!selectedId.value || !tasks.value.some(task => task.id === selectedId.value)) {
      selectedId.value = tasks.value[0].id
    }
  },
)

watch(
  () => selected.value?.cmd,
  cmd => emit('title', cmd || null),
  { immediate: true },
)

watch(
  () => selected.value?.tail.length,
  () => {
    if (selected.value?.id === selectedId.value) void nextTick(followTail)
  },
)

onMounted(() => {
  bootTasks()
  if (!selectedId.value && tasks.value.length) selectedId.value = tasks.value[0].id
  timer = window.setInterval(() => {
    if (tasks.value.some(task => task.state === 'running')) now.value = Date.now()
  }, 1000)
})

onBeforeUnmount(() => {
  window.clearInterval(timer)
  window.clearTimeout(noteTimer)
  window.clearTimeout(failTimer)
})
</script>

<template>
  <div class="term-pane">
    <aside class="term-list">
      <div class="tl-head">
        <span>终端任务</span>
        <span class="tl-count">{{ tasks.length }}</span>
        <span class="grow" />
        <button class="tl-add" title="新建终端任务" aria-label="新建终端任务" @click="openForm">
          ＋
        </button>
      </div>

      <div class="tl-scroll">
        <template v-for="group in groupsView" :key="group.key">
          <div v-if="group.items.length" class="tl-group">
            {{ group.label }}<span class="n">{{ group.items.length }}</span>
          </div>
          <button
            v-for="task in group.items"
            :key="task.id"
            class="tl-item"
            :class="{ on: selected?.id === task.id && !form }"
            :title="task.cmd"
            @click="selectTask(task.id)"
          >
            <span class="tl-dot" :class="stateClass(task)" />
            <span class="tl-main">
              <span class="tl-cmd">{{ task.cmd }}</span>
              <span class="tl-sub">{{ sinceOf(task) }} · {{ scopeOf(task) }}</span>
            </span>
          </button>
        </template>

        <div v-if="!tasks.length" class="tl-empty">
          还没有终端任务<br />
          <span>点左上「＋」起一个</span>
        </div>
      </div>
    </aside>

    <div class="term-main">
      <div v-if="tasksLoadError" class="term-error" role="alert">
        后台任务接口不可用：{{ tasksLoadError }}。如果刚更新过 Prism，请退出并重新打开再试。
      </div>
      <div v-if="failure" class="term-error" role="alert">{{ failure }}</div>
      <!-- 新建任务：直接在右栏换表单，不叠第二层浮层 -->
      <form v-if="form" class="term-form" @submit.prevent="startTask">
        <div class="tf-kicker">New terminal task</div>
        <h2 class="tf-title">新建终端任务</h2>
        <p class="tf-desc">
          命令会在项目目录下起一个独立进程。后台任务会出现在输入框上方的任务坞，切会话也不会中断。
        </p>

        <div class="tf-field">
          <div class="tf-label">命令 <span class="hint">Enter 换行 · ⌘↵ 启动</span></div>
          <div class="tf-cmd">
            <span class="tf-dollar">$</span>
            <textarea
              ref="cmdInput"
              v-model="command"
              spellcheck="false"
              placeholder="pnpm build"
              @keydown.esc.stop="cancelForm"
              @keydown.meta.enter.prevent="startTask"
              @keydown.ctrl.enter.prevent="startTask"
            ></textarea>
          </div>
        </div>

        <div class="tf-field">
          <div class="tf-label">工作目录</div>
          <div class="tf-row">
            <div class="tf-dir" :title="directory ?? ''">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                <path d="M3.5 7.5A1.5 1.5 0 0 1 5 6h4l2 2.5h8a1.5 1.5 0 0 1 1.5 1.5v8A1.5 1.5 0 0 1 19 19.5H5A1.5 1.5 0 0 1 3.5 18V7.5Z" />
              </svg>
              <span>{{ directory || '（这个会话没有工作目录）' }}</span>
            </div>
          </div>
        </div>

        <div class="tf-field">
          <div class="tf-label">运行方式</div>
          <div class="tf-modes">
            <button
              type="button"
              class="tf-mode"
              :class="{ on: mode === 'bg' }"
              @click="mode = 'bg'"
            >
              <span class="tf-radio" />
              <span class="tf-mode-main">
                <span class="tf-mode-name">后台任务</span>
                <span class="tf-mode-sub">进任务坞，可随时收起；切会话继续跑</span>
              </span>
            </button>
            <button
              type="button"
              class="tf-mode"
              :class="{ on: mode === 'fg' }"
              @click="mode = 'fg'"
            >
              <span class="tf-radio" />
              <span class="tf-mode-main">
                <span class="tf-mode-name">前台终端</span>
                <span class="tf-mode-sub">只在终端页显示，不占任务坞</span>
              </span>
            </button>
          </div>
        </div>

        <div class="tf-foot">
          <span class="hint">
            <span class="kbd">⌘</span> <span class="kbd">↵</span> 启动 · <span class="kbd">Esc</span> 取消
          </span>
          <button type="button" class="btn-ghost" @click="cancelForm">取消</button>
          <button type="submit" class="btn-primary" :disabled="busy === 'new'">
            {{ busy === 'new' ? '启动中…' : '启动任务' }}
          </button>
        </div>
      </form>

      <div v-else-if="selected" class="term-viewer">
        <div class="tv-head">
          <span class="tv-title" :title="selected.cmd">{{ selected.cmd }}</span>
          <span class="tv-cwd" :title="selected.cwd">{{ selected.cwd }}</span>
          <span v-if="selected.origin === 'agent'" class="tv-origin">Agent</span>
          <span class="t-chip" :class="stateClass(selected)">{{ stateLabel(selected) }}</span>
          <span class="grow" />
          <span v-if="note" class="tv-note">{{ note }}</span>
          <div class="tv-actions">
            <button
              v-if="selected.state === 'running'"
              class="act-btn danger"
              :disabled="busy === selected.id"
              @click="stop(selected)"
            >
              ■ 停止
            </button>
            <button
              v-else
              class="act-btn"
              :disabled="busy === selected.id"
              @click="rerun(selected)"
            >
              ↻ 重跑
            </button>
            <button class="act-btn" @click="copyAll">复制全部</button>
            <button
              v-if="selected.state !== 'running'"
              class="act-btn"
              :disabled="busy === selected.id"
              @click="closeTask(selected)"
            >
              关闭
            </button>
          </div>
        </div>

        <div :ref="setConsoleEl" class="tv-console">
          <div v-for="(line, index) in selected.tail" :key="index" class="tv-line">{{ line }}</div>
          <span v-if="selected.state === 'running'" class="cursor" />
        </div>

        <div class="tv-input-row">
          <span class="tv-prompt">$</span>
          <input
            v-model="runValue"
            class="tv-input"
            spellcheck="false"
            :disabled="selected.state === 'running' || busy === 'run'"
            :placeholder="
              selected.state === 'running'
                ? '这条还在跑，先停止再输入'
                : '继续在这个目录里跑一条命令…'
            "
            @keydown.enter.prevent="runCommand"
          />
          <span class="tv-kbd">
            {{ selected.state === 'running' ? '运行中 · 同一进程组' : 'Enter 运行 · 新建一条任务' }}
          </span>
        </div>
      </div>

      <div v-else class="term-empty">
        <div class="term-empty-mark">⌘</div>
        <p>还没有终端任务</p>
        <p class="term-empty-sub">起一条命令，输出会实时出现在这里；后台任务同时进任务坞。</p>
        <button class="btn-primary" @click="openForm">＋ 新建终端任务</button>
      </div>
    </div>
  </div>
</template>

<style scoped src="../styles/components/TerminalPane.css"></style>
