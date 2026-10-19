/**
 * 渲染级错误的兜底。
 *
 * 组件在 render / computed 里抛异常时，Vue 会把那棵子树卸掉——用户看到的就是
 * 一块白屏或者半个界面，而控制台在 Tauri 窗口里根本看不到。这里把错误收成一条
 * 可显示的消息（App 顶部那些红条），并保留原始对象到控制台。
 */
import type { App } from 'vue'
import { shallowRef } from 'vue'

export const renderError = shallowRef<string | null>(null)

let timer = 0

export function reportRenderError(cause: unknown): void {
  console.error('[render]', cause)
  const message = cause instanceof Error ? cause.message : String(cause)
  renderError.value = message || '未知错误'
  window.clearTimeout(timer)
  timer = window.setTimeout(() => {
    renderError.value = null
  }, 10_000)
}

export function clearRenderError(): void {
  window.clearTimeout(timer)
  renderError.value = null
}

/** 装到 app 上：组件内异常 + 没人接的 window.onerror 都收进来。 */
export function installRenderErrorHooks(app: App): void {
  app.config.errorHandler = cause => reportRenderError(cause)
  window.addEventListener('error', event => {
    if (event.error) reportRenderError(event.error)
  })
}
