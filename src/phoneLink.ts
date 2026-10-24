/** 手机链接的纯展示逻辑：状态文案 / 色调 / 二维码倒计时（组件与测试共用）。 */

import type { PhoneLinkStatus } from './types'

export type PhoneLinkTone = 'ok' | 'warn' | 'error' | 'muted'

/** 左卡状态行的文案：未启用 → 启动失败 → 已连接 → 等待扫码。 */
export function statusLabel(status: PhoneLinkStatus | null): string {
  if (!status) return '…'
  if (!status.enabled) return '未启用'
  if (!status.serverRunning) return '启动失败（看日志）'
  if (status.device) return `已连接 · ${status.device.name}`
  if (status.pairing) return '等待扫码'
  return '等待配对'
}

export function statusTone(status: PhoneLinkStatus | null): PhoneLinkTone {
  if (!status) return 'muted'
  if (!status.enabled) return 'muted'
  if (!status.serverRunning) return 'error'
  if (status.device) return 'ok'
  return 'warn'
}

/** webhook 输入框占位：已配置只回显尾 4 位提示（原文永不回传 UI）。 */
export function webhookPlaceholder(status: PhoneLinkStatus | null): string {
  if (status?.webhookConfigured) {
    return `已保存 ${status.webhookHint ?? ''}（留空沿用）`
  }
  return 'https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=…'
}

/** 配对码剩余秒数（已过期或没有配对会话 → 0）。 */
export function pairingLeftSeconds(status: PhoneLinkStatus | null, now: number): number {
  const expiresAt = status?.pairing?.expiresAt
  if (!expiresAt) return 0
  return Math.max(0, Math.ceil((expiresAt - now) / 1000))
}

/** 二维码可渲染的前提：启用 + 服务在跑 + 后端给了 SVG + 配对码未过期。 */
export function qrAvailable(status: PhoneLinkStatus | null, now: number): boolean {
  return Boolean(
    status?.enabled &&
      status.serverRunning &&
      status.qrSvg &&
      status.pairing &&
      pairingLeftSeconds(status, now) > 0,
  )
}
