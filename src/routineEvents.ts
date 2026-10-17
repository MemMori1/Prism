import { listen } from '@tauri-apps/api/event'
import type { RoutineEventPayload } from './types'

/**
 * 例行任务事件总线：Rust 调度器把运行结果 / 权限请求广播过来
 * （`routine-event`），这里做前端分发。首次订阅时自动挂监听（进程内只挂一次）。
 */

type Listener = (payload: RoutineEventPayload) => void

const listeners = new Set<Listener>()
let startPromise: Promise<void> | null = null

function ensureStarted(): Promise<void> {
  if (!startPromise) {
    startPromise = listen<RoutineEventPayload>('routine-event', envelope => {
      for (const listener of listeners) {
        try {
          listener(envelope.payload)
        } catch (cause) {
          console.error('[prism] 例行任务事件处理失败', cause)
        }
      }
    }).then(() => undefined)
  }
  return startPromise
}

/** 订阅例行任务事件；返回取消订阅函数。 */
export function onRoutineEvent(listener: Listener): () => void {
  void ensureStarted()
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}
