import { invoke } from '@tauri-apps/api/core'

/** 是否运行在 Tauri 桌面壳内（纯浏览器预览时不注入，保留浏览器原生 F12）。 */
function inTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window
}

/**
 * 开合当前窗口的开发者工具。
 *
 * Tauri 2.12 的 JS API 没有暴露独立的 open/close devtools 方法，
 * 走 webview 插件的内部切换命令 `plugin:webview|internal_toggle_devtools`：
 * - debug 构建默认可用；
 * - release 构建需在 src-tauri/Cargo.toml 给 tauri 开启 "devtools" 特性；
 * - 权限 core:webview:allow-internal-toggle-devtools 已包含在 core:webview:default。
 */
export async function toggleDevtools(): Promise<void> {
  if (!inTauri()) return
  await invoke('plugin:webview|internal_toggle_devtools')
}

/** 调试模式：按 F12 开合开发者工具（macOS 上可能是 fn+F12）。 */
export function initDevtoolsHotkey(): void {
  if (!inTauri()) return
  window.addEventListener('keydown', event => {
    if (event.key !== 'F12') return
    // 拦住默认行为，避免焦点里的输入控件先把它吃掉。
    event.preventDefault()
    void toggleDevtools().catch(() => {
      // 开合失败不打扰使用（例如 release 构建没开 devtools 特性）。
    })
  })
}
