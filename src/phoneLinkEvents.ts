import { listen } from '@tauri-apps/api/event'
import type { PhoneLinkEventPayload } from './types'

/**
 * 手机链接事件总线：Rust 端把「配对成功 / 解除配对 / 回合收口」广播过来
 * （`phone-link-event`），这里做前端分发。首次订阅时自动挂监听（进程内只挂一次）。
 */

type Listener = (payload: PhoneLinkEventPayload) => void

const listeners = new Set<Listener>()
let startPromise: Promise<void> | null = null

function ensureStarted(): Promise<void> {
  if (!startPromise) {
    startPromise = listen<PhoneLinkEventPayload>('phone-link-event', envelope => {
      for (const listener of listeners) {
        try {
          listener(envelope.payload)
        } catch (cause) {
          console.error('[prism] 手机链接事件处理失败', cause)
        }
      }
    }).then(() => undefined)
  }
  return startPromise
}

/** 订阅手机链接事件；返回取消订阅函数。 */
export function onPhoneLinkEvent(listener: Listener): () => void {
  void ensureStarted()
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}
