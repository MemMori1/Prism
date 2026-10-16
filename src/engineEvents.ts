import { listen } from '@tauri-apps/api/event'
import type { EngineEvent } from './types'

/**
 * 引擎事件总线：Rust 侧把 SSE 事件原样广播过来，这里做前端分发。
 * 首次订阅时自动挂上 Tauri 监听（进程内只挂一次）。
 */

type Listener = (event: EngineEvent) => void

const listeners = new Set<Listener>()
let startPromise: Promise<void> | null = null

function ensureStarted(): Promise<void> {
  if (!startPromise) {
    startPromise = listen<EngineEvent>('engine-event', envelope => {
      for (const listener of listeners) {
        try {
          listener(envelope.payload)
        } catch (cause) {
          console.error('[prism] 引擎事件处理失败', cause)
        }
      }
    }).then(() => undefined)
  }
  return startPromise
}

/** 订阅引擎事件；返回取消订阅函数。 */
export function onEngineEvent(listener: Listener): () => void {
  void ensureStarted()
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}
