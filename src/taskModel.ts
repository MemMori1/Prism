/**
 * 后台任务的纯逻辑（不碰 Tauri）：状态计数、过滤、合并、追加输出。
 *
 * 和 `tasks.ts` 分开是为了能单测：事件到达顺序在真机上是竞态的
 * （`task-state` 可能比最后几行 `task-output` 先到），这里把「合并时
 * tail 取更长的那份」定成规则，组件只消费结果。
 */
import type { BackgroundTask, TaskOutputPayload, TaskState } from './types'

/** 前端保留的输出行数（与 Rust 侧 TAIL_LINES 对齐）。 */
export const TASK_TAIL_LIMIT = 400

export type TaskFilter = 'all' | 'running' | 'ended'

export const TASK_STATE_LABELS: Record<TaskState, string> = {
  running: '运行中',
  done: '已完成',
  failed: '失败',
  stopped: '已停止',
}

/** 任务的样式类名（run / ok / fail / stop）。 */
export const TASK_STATE_CLASS: Record<TaskState, string> = {
  running: 'run',
  done: 'ok',
  failed: 'fail',
  stopped: 'stop',
}

export function isActive(task: BackgroundTask): boolean {
  return task.state === 'running'
}

/**
 * 坞里显示的任务：
 * - 前台终端任务（background=false）只活在终端页；
 * - Agent 自己起的命令只在**还在跑**时进坞——收口后留给终端页，
 *   否则一轮跑十几条命令会把坞刷成历史列表。
 */
export function dockTasks(tasks: BackgroundTask[]): BackgroundTask[] {
  return tasks.filter(
    task => task.background && (task.origin !== 'agent' || task.state === 'running'),
  )
}

export function taskCounts(tasks: BackgroundTask[]): {
  running: number
  ended: number
  failed: number
} {
  let running = 0
  let ended = 0
  let failed = 0
  for (const task of tasks) {
    if (isActive(task)) running += 1
    else {
      ended += 1
      if (task.state === 'failed') failed += 1
    }
  }
  return { running, ended, failed }
}

export function sortTasks(tasks: BackgroundTask[]): BackgroundTask[] {
  return [...tasks].sort((left, right) => right.startedAt - left.startedAt)
}

export function filterTasks(tasks: BackgroundTask[], filter: TaskFilter): BackgroundTask[] {
  if (filter === 'running') return tasks.filter(isActive)
  if (filter === 'ended') return tasks.filter(task => !isActive(task))
  return tasks
}

/**
 * 合并一条任务快照：
 * - 新任务直接插进列表（按开始时间倒序）；
 * - 已有任务**保留更长的 tail**——状态收口事件里的快照可能比已经流进来的输出旧。
 */
export function mergeTask(tasks: BackgroundTask[], incoming: BackgroundTask): BackgroundTask[] {
  const index = tasks.findIndex(task => task.id === incoming.id)
  if (index === -1) return sortTasks([...tasks, incoming])
  const previous = tasks[index]
  // 后端理论上都带 tail；这条兜底是为了「某条事件形状不对」时不至于把整页渲染打崩。
  const incomingTail = incoming.tail ?? []
  const tail = incomingTail.length >= previous.tail.length ? incomingTail : previous.tail
  const next = [...tasks]
  next[index] = { ...incoming, tail }
  return sortTasks(next)
}

/** 追加一行输出；任务已被移出列表就原样返回（迟到的行）。 */
export function appendTaskLine(
  tasks: BackgroundTask[],
  payload: TaskOutputPayload,
): BackgroundTask[] {
  const index = tasks.findIndex(task => task.id === payload.id)
  if (index === -1) return tasks
  const task = tasks[index]
  const tail = [...task.tail, payload.line]
  if (tail.length > TASK_TAIL_LIMIT) tail.splice(0, tail.length - TASK_TAIL_LIMIT)
  const next = [...tasks]
  next[index] = { ...task, tail }
  return next
}

/** 已跑时长（结束的用 endedAt，跑着的用 now）。 */
export function taskElapsed(task: BackgroundTask, now: number): number {
  const end = task.endedAt ?? (isActive(task) ? now : task.startedAt)
  return Math.max(0, end - task.startedAt)
}

/** 秒表样式：00:42 / 12:03 / 1:02:09。 */
export function formatElapsed(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000))
  const hours = Math.floor(total / 3600)
  const minutes = Math.floor((total % 3600) / 60)
  const seconds = total % 60
  const mm = `${minutes}`.padStart(2, '0')
  const ss = `${seconds}`.padStart(2, '0')
  return hours > 0 ? `${hours}:${mm}:${ss}` : `${mm}:${ss}`
}

/** 终端页分组：跑着的在上，已结束的按时间倒序。 */
export function groupTasks(tasks: BackgroundTask[]): {
  running: BackgroundTask[]
  ended: BackgroundTask[]
} {
  const sorted = sortTasks(tasks)
  return {
    running: sorted.filter(isActive),
    ended: sorted.filter(task => !isActive(task)),
  }
}

/** 退出码 / 停止原因的简短说明。 */
export function taskExitLabel(task: BackgroundTask): string {
  if (task.state === 'running') return ''
  if (task.state === 'stopped') return '已停止'
  if (task.exitCode === null || task.exitCode === undefined) return '已结束'
  return `exit ${task.exitCode}`
}
