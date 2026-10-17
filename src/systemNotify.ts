/**
 * 系统通知（macOS）：仅当窗口不在前台（或最小化）时发送。
 *
 * 权限：首次发送前询问；用户明确拒绝后不再反复请求（记住本次运行）。
 * 环境不支持 / 权限失败一律静默降级——应用内通知条目仍然可见。
 */
import { getCurrentWindow } from '@tauri-apps/api/window'
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from '@tauri-apps/plugin-notification'

/** 用户明确拒绝后本次运行内不再请求。 */
let denied = false

export async function systemNotify(title: string, body: string): Promise<void> {
  try {
    const focused = await getCurrentWindow().isFocused()
    if (focused) return
  } catch {
    // 非 Tauri 环境（如纯浏览器调试）：不发系统通知。
    return
  }

  try {
    if (denied) return
    let granted = await isPermissionGranted()
    if (!granted) {
      const result = await requestPermission()
      granted = result === 'granted'
      if (result === 'denied') denied = true
    }
    if (granted) {
      await sendNotification({ title, body })
    }
  } catch {
    // 权限 / 平台问题：静默降级。
  }
}

/** 测试辅助：重置拒权缓存。 */
export function resetSystemNotifyForTest(): void {
  denied = false
}
