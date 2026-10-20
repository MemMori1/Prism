<script setup lang="ts">
/**
 * 后台任务坞（M23）：输入框正上方的一条状态条，展开是一张向上浮起的任务面板。
 *
 * 为什么在这儿：任务是「这一轮在做的事」的延长线，视线本来就落在输入区；
 * 折叠条只占 35px，面板向上盖住时间线而不是把输入框往下推。切会话强制折叠
 * （任务继续跑），新建任务自动展开，失败只把计数标红。
 *
 * 数据全部来自 `tasks.ts`（Rust 注册表的前端副本），坞和终端页读同一份。
 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { relativeTime } from '../format'
import {
  TASK_STATE_CLASS,
  TASK_STATE_LABELS,
  dockTasks,
  filterTasks,
  formatElapsed,
  taskCounts,
  taskElapsed,
  taskExitLabel,
  type TaskFilter,
} from '../taskModel'
import { bootTasks, removeTask, spawnTask, stopTask, tasks, tasksLoaded } from '../tasks'
import type { BackgroundTask } from '../types'

const props = defineProps<{
  /** 当前会话：只用来标「本会话」，任务本身跨会话共享。 */
  sessionId: string | null
}>()

const emit = defineEmits<{
  (e: 'error', message: string): void
  /** 在文件侧栏的终端页打开某个任务。 */
  (e: 'openTerminal', taskId: string): void
  /** 打开终端页（不指定任务）。 */
  (e: 'openTerminalList'): void
  /** 打开终端页并直接落在「新建任务」表单。 */
  (e: 'newTerminal'): void
}>()

const open = ref(false)
const filter = ref<TaskFilter>('all')
const openId = ref<string | null>(null)
const now = ref(Date.now())
const busy = ref<string | null>(null)
const consoleEl = ref<HTMLElement | null>(null)
const dockEl = ref<HTMLElement | null>(null)

const all = computed(() => dockTasks(tasks.value))
const counts = computed(() => taskCounts(all.value))
const visible = computed(() => filterTasks(all.value, filter.value))
const openTask = computed(
  () => visible.value.find(task => task.id === openId.value) ?? null,
)
const front = computed(
  () =>
    all.value.find(task => task.state === 'running') ??
    all.value.find(task => task.state === 'failed') ??
    all.value[0] ??
    null,
)

const filters = computed<Array<{ key: TaskFilter; label: string; count: number }>>(() => [
  { key: 'all', label: '全部', count: all.value.length },
  { key: 'running', label: '进行中', count: counts.value.running },
  { key: 'ended', label: '已结束', count: counts.value.ended },
])

let timer = 0

function toggle(): void {
  open.value = !open.value
  if (open.value) {
    void nextTick(scrollConsole)
  }
}

function close(): void {
  open.value = false
}

function setFilter(next: TaskFilter): void {
  filter.value = next
  openId.value = null
}

function toggleTask(id: string): void {
  openId.value = openId.value === id ? null : id
  if (openId.value) void nextTick(scrollConsole)
}

function scrollConsole(): void {
  const el = consoleEl.value
  if (!el) return
  el.scrollTop = el.scrollHeight
}

/** v-for 里的 ref 会变成数组，用函数 ref 只留当前展开的那个控制台。 */
function setConsoleEl(el: unknown): void {
  consoleEl.value = (el as HTMLElement | null) ?? null
}

/** 用户往上翻的时候别抢滚动位置；贴着底才跟。 */
function followTail(): void {
  const el = consoleEl.value
  if (!el) return
  if (el.scrollHeight - el.scrollTop - el.clientHeight < 60) {
    el.scrollTop = el.scrollHeight
  }
}

function scopeOf(task: BackgroundTask): string {
  if (task.origin === 'agent') return 'Agent'
  if (!task.sessionId) return '独立任务'
  if (task.sessionId === props.sessionId) return '本会话'
  return task.title || '其它会话'
}

function elapsedOf(task: BackgroundTask): string {
  return formatElapsed(taskElapsed(task, now.value))
}

function sinceOf(task: BackgroundTask): string {
  if (task.state === 'running') return `已跑 ${elapsedOf(task)}`
  return relativeTime(task.endedAt ?? task.startedAt)
}

function messageOf(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause)
}

async function runOn(id: string, action: () => Promise<void>): Promise<void> {
  if (busy.value) return
  busy.value = id
  try {
    await action()
  } catch (cause) {
    emit('error', messageOf(cause))
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
      sessionId: task.sessionId ?? props.sessionId,
      title: task.title,
      background: true,
    })
    filter.value = 'all'
    openId.value = created.id
    open.value = true
    void nextTick(scrollConsole)
  })
}

function remove(task: BackgroundTask): void {
  void runOn(task.id, () => removeTask(task.id))
}

function onKeydown(event: KeyboardEvent): void {
  const target = event.target as HTMLElement | null
  const typing = !!target?.closest('input, textarea, [contenteditable="true"]')
  if (!typing && event.key === 'Escape' && open.value) {
    // 文件侧栏开着时 Esc 先关它，别连坞一起收掉。
    if (document.querySelector('.changes-drawer')) return
    close()
    return
  }
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'b') {
    if (typing || document.querySelector('.changes-drawer')) return
    event.preventDefault()
    toggle()
  }
}

onMounted(() => {
  bootTasks()
  window.addEventListener('keydown', onKeydown)
  timer = window.setInterval(() => {
    if (counts.value.running) now.value = Date.now()
  }, 1000)
})

onBeforeUnmount(() => {
  window.removeEventListener('keydown', onKeydown)
  window.clearInterval(timer)
})

// 切会话：强制折叠（任务继续跑），不保留上一条的展开状态。
watch(
  () => props.sessionId,
  () => {
    open.value = false
    openId.value = null
  },
)

// 任务被清空时收起面板，别留一张空浮层。
watch(
  () => all.value.length,
  length => {
    if (!length) {
      open.value = false
      openId.value = null
    }
  },
)

watch(
  () => openTask.value?.tail.length,
  () => {
    if (openId.value) void nextTick(followTail)
  },
)

// 新增任务时自动展开（重跑 / 终端页 / agent 起的任务）：只认「没见过的 id」。
// 首次拉取 task_list 只是补齐已有任务，不该弹面板；删掉一条让别的顶上来也不算新增。
// 归档 / 工作台会各留一个 SessionView（v-show 切换），隐藏实例不要跟着展开。
const knownIds = new Set<string>()
let primed = false
watch(
  [tasksLoaded, () => all.value.map(task => task.id).join('|')],
  () => {
    if (!tasksLoaded.value) return
    if (!primed) {
      for (const task of all.value) knownIds.add(task.id)
      primed = true
      return
    }
    const fresh = all.value.find(task => !knownIds.has(task.id))
    for (const task of all.value) knownIds.add(task.id)
    if (!fresh || fresh.state !== 'running') return
    if (dockEl.value && dockEl.value.offsetParent === null) return
    filter.value = 'all'
    open.value = true
    openId.value = fresh.id
    void nextTick(scrollConsole)
  },
  { immediate: true },
)
</script>

<template>
  <div v-if="all.length" ref="dockEl" class="dock" :class="{ 'is-open': open }">
    <!-- 展开：贴在折叠条上方，浮在时间线上 -->
    <div v-if="open" class="dock-panel" role="dialog" aria-label="后台任务">
      <div class="dock-head">
        <span class="dock-title">后台任务</span>
        <span class="dock-sub">跨会话共享 · 切会话自动折叠</span>
        <div class="dock-seg">
          <button
            v-for="item in filters"
            :key="item.key"
            :class="{ on: filter === item.key }"
            @click="setFilter(item.key)"
          >
            {{ item.label }} {{ item.count }}
          </button>
        </div>
        <div class="dock-head-acts">
          <button class="dock-new" title="新建终端任务" @click="emit('newTerminal')">
            ＋<span class="dock-new-label"> 新任务</span>
          </button>
          <button class="dock-ico" title="收起（Esc）" aria-label="收起后台任务" @click="close">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
              <path d="M6 6l12 12M18 6 6 18" />
            </svg>
          </button>
        </div>
      </div>

      <div class="dock-list">
        <div v-if="!visible.length" class="dock-empty">
          这一栏是空的。<br />
          在终端页新建一条命令，或在输入框里以 <code>!</code> 开头跑。
        </div>

        <article
          v-for="task in visible"
          :key="task.id"
          class="task"
          :class="{ open: openId === task.id }"
        >
          <div
            class="task-row"
            role="button"
            tabindex="0"
            :aria-expanded="openId === task.id"
            :title="`${TASK_STATE_LABELS[task.state]}：${task.cmd}`"
            @click="toggleTask(task.id)"
            @keydown.enter.prevent="toggleTask(task.id)"
            @keydown.space.prevent="toggleTask(task.id)"
          >
            <span class="t-state">
              <span v-if="task.state === 'running'" class="spin" />
              <span v-else class="t-ico" :class="TASK_STATE_CLASS[task.state]">
                {{ task.state === 'done' ? '✓' : task.state === 'failed' ? '✕' : '■' }}
              </span>
            </span>

            <span class="t-main">
              <span class="t-title">
                <span class="t-cmd">{{ task.cmd }}</span>
                <span
                  class="t-scope"
                  :class="{
                    other: task.sessionId !== sessionId && task.origin !== 'agent',
                    agent: task.origin === 'agent',
                  }"
                >
                  {{ scopeOf(task) }}
                </span>
              </span>
              <span class="t-sub">
                <span>{{ task.cwd }}</span>
                <span class="sep">·</span>
                <span>{{ sinceOf(task) }}</span>
                <template v-if="taskExitLabel(task)">
                  <span class="sep">·</span>
                  <span :class="{ 't-bad': task.state === 'failed' }">{{ taskExitLabel(task) }}</span>
                </template>
              </span>
            </span>

            <span class="t-meta">
              <span v-if="task.state === 'running'" class="t-elapsed">{{ elapsedOf(task) }}</span>
              <span class="t-chip" :class="TASK_STATE_CLASS[task.state]">
                {{ TASK_STATE_LABELS[task.state] }}
              </span>
            </span>
            <span class="t-caret" aria-hidden="true">▼</span>
          </div>

          <div v-if="openId === task.id" class="task-detail">
            <div :ref="setConsoleEl" class="console">
              <div v-for="(line, index) in task.tail" :key="index" class="cline">{{ line }}</div>
              <span v-if="task.state === 'running'" class="cursor" />
            </div>
            <div class="t-acts">
              <button
                v-if="task.state === 'running'"
                class="act-btn danger"
                :disabled="busy === task.id"
                @click.stop="stop(task)"
              >
                ■ 停止
              </button>
              <button v-else class="act-btn" :disabled="busy === task.id" @click.stop="rerun(task)">
                ↻ 重跑
              </button>
              <button class="act-btn" @click.stop="emit('openTerminal', task.id)">
                在终端打开
              </button>
              <span class="push" />
              <span v-if="task.state === 'running'" class="t-note">收起坞不影响运行</span>
              <button v-else class="act-btn" :disabled="busy === task.id" @click.stop="remove(task)">
                移出列表
              </button>
            </div>
          </div>
        </article>
      </div>

      <div class="dock-foot">
        <span class="foot-hint">任务跑在独立进程里，收起坞 / 切走会话都不会中断</span>
        <span class="grow" />
        <button @click="emit('openTerminalList')">在终端里看全部 →</button>
      </div>
    </div>

    <!-- 折叠条 -->
    <div
      class="dock-strip"
      role="button"
      tabindex="0"
      :aria-expanded="open"
      aria-label="后台任务"
      @click="toggle"
      @keydown.enter.prevent="toggle"
      @keydown.space.prevent="toggle"
    >
      <span class="dock-glyph">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M3 12h4l2.5-6 4 13 2.5-7H21" />
        </svg>
        <span v-if="counts.running" class="live" />
      </span>
      <span class="dock-label">后台任务</span>

      <span class="dock-counts">
        <span v-if="counts.running" class="dock-cnt run">
          <i class="dot run" /><b>{{ counts.running }}</b> 运行中
        </span>
        <span v-if="counts.ended - counts.failed" class="dock-cnt">
          <i class="dot" /><b>{{ counts.ended - counts.failed }}</b> 已完成
        </span>
        <span v-if="counts.failed" class="dock-cnt">
          <i class="dot fail" /><b>{{ counts.failed }}</b> 失败
        </span>
      </span>

      <span v-if="front" class="dock-front">
        <span class="nm">{{ front.cmd }}</span>
        <span class="tm">
          {{ front.state === 'running' ? elapsedOf(front) : relativeTime(front.endedAt ?? front.startedAt) }}
        </span>
      </span>

      <span class="dock-actions">
        <button
          class="dock-ico"
          title="新建终端任务"
          aria-label="新建终端任务"
          @click.stop="emit('newTerminal')"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
            <path d="M12 5v14M5 12h14" />
          </svg>
        </button>
        <span class="dock-ico chev" aria-hidden="true">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="m6 15 6-6 6 6" />
          </svg>
        </span>
        <button
          class="dock-ico"
          title="收起（Esc）"
          aria-label="收起后台任务"
          @click.stop="close"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
            <path d="M6 6l12 12M18 6 6 18" />
          </svg>
        </button>
      </span>
    </div>
  </div>
</template>

<style scoped src="../styles/components/BackgroundTasks.css"></style>
