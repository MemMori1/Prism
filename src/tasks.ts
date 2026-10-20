/**
 * 后台任务仓库（M23）：Rust 注册表在前端的副本。
 *
 * - 首次使用时 `bootTasks()` 挂三条事件：`task-output`（逐行）、`task-state`
 *   （整条快照）、`task-removed`（id），再拉一次 `task_list` 补齐已有任务。
 * - 组件不自己 listen，全部从这里读 `tasks`——坞和终端页看的是同一份数据。
 * - 不做持久化：进程随应用退出，历史没有意义。
 */
import { computed, ref } from 'vue'
import { listen } from '@tauri-apps/api/event'
import { api } from './api'
import { appendTaskLine, mergeTask, sortTasks, taskCounts } from './taskModel'
import type { BackgroundTask, TaskOutputPayload, TaskSpawnInput } from './types'

const items = ref<BackgroundTask[]>([])
const loaded = ref(false)
/** task_list 失败的原因（旧内核 / 命令未注册时前端要能说清楚）。 */
const loadError = ref<string | null>(null)
let booted = false
let refreshing: Promise<void> | null = null

export const tasks = items
export const tasksLoaded = loaded
export const tasksLoadError = loadError
export const taskStats = computed(() => taskCounts(items.value))
/** 跑着的任务数（终端 tab 的角标）。 */
export const activeTaskCount = computed(
  () => items.value.filter(task => task.state === 'running').length,
)

/** 挂事件 + 拉一次列表；重复调用无副作用。 */
export function bootTasks(): void {
  if (booted) return
  booted = true

  void listen<TaskOutputPayload>('task-output', event => {
    items.value = appendTaskLine(items.value, event.payload)
  }).catch(cause => {
    console.error('[prism] 订阅任务输出失败', cause)
  })

  void listen<BackgroundTask>('task-state', event => {
    items.value = mergeTask(items.value, event.payload)
  }).catch(cause => {
    console.error('[prism] 订阅任务状态失败', cause)
  })

  void refreshTasks()
}

/** 重新拉全部任务（失败保留旧列表，不弹错）。 */
export function refreshTasks(): Promise<void> {
  if (refreshing) return refreshing
  refreshing = (async () => {
    try {
      items.value = sortTasks(await api.taskList())
      loaded.value = true
      loadError.value = null
    } catch (cause) {
      loadError.value = cause instanceof Error ? cause.message : String(cause)
      console.error('[prism] 读取后台任务失败', cause)
    } finally {
      refreshing = null
    }
  })()
  return refreshing
}

/** 起一个任务：返回快照，并立刻落进列表（不等事件）。 */
export async function spawnTask(input: TaskSpawnInput): Promise<BackgroundTask> {
  const task = await api.taskSpawn(input)
  items.value = mergeTask(items.value, task)
  return task
}

export async function stopTask(id: string): Promise<void> {
  await api.taskStop(id)
  // 进程组收口有宽限期（SIGTERM → 900ms → SIGKILL），先乐观标成「已停止」，
  // 让按钮 / 分组立刻响应；真正状态由 `task-state` 事件纠正。
  items.value = items.value.map(task =>
    task.id === id && task.state === 'running'
      ? { ...task, state: 'stopped', endedAt: Date.now() }
      : task,
  )
}

/** 移出列表（只对已结束的任务有效）。 */
export async function removeTask(id: string): Promise<void> {
  await api.taskRemove(id)
  items.value = items.value.filter(task => task.id !== id)
}

export function taskById(id: string | null): BackgroundTask | null {
  if (!id) return null
  return items.value.find(task => task.id === id) ?? null
}
