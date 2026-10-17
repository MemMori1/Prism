// 配额面板的纯展示助手（独立成文件便于单测）。
import type { QuotaCredits, QuotaWindow } from './types'

/** 窗口时长标签：300 → 5 小时窗口；10080 → 7 天窗口。 */
export function windowLabel(minutes: number | null): string {
  if (minutes === null || minutes <= 0) return '配额窗口'
  if (minutes % 1440 === 0) return `${minutes / 1440} 天窗口`
  if (minutes % 60 === 0) return `${minutes / 60} 小时窗口`
  return `${minutes} 分钟窗口`
}

/** 已用百分比：12.34 → 12.3%。 */
export function usedPercentText(percent: number): string {
  const value = Math.max(0, percent)
  const text = (Math.round(value * 10) / 10).toFixed(1).replace(/\.0$/, '')
  return `${text}%`
}

/** 条形宽度（0–100 夹紧）。 */
export function barWidth(percent: number): number {
  return Math.min(100, Math.max(0, percent))
}

/** 距重置的倒计时文案；null = 无重置时间。 */
export function resetInText(resetsAtMs: number | null, now = Date.now()): string | null {
  if (resetsAtMs === null) return null
  const diff = resetsAtMs - now
  if (diff <= 0) return '已到重置时间'
  const minutes = Math.floor(diff / 60_000)
  const days = Math.floor(minutes / 1440)
  const hours = Math.floor((minutes % 1440) / 60)
  const remain = minutes % 60
  if (days > 0) return `${days} 天 ${hours} 小时后重置`
  if (hours > 0) return remain > 0 ? `${hours} 小时 ${remain} 分后重置` : `${hours} 小时后重置`
  if (remain > 0) return `${remain} 分钟后重置`
  return '1 分钟内重置'
}

/** 窗口行副文案：已用 % + 重置倒计时。 */
export function windowMetaText(window: QuotaWindow, now = Date.now()): string {
  const parts = [usedPercentText(window.usedPercent)]
  const reset = resetInText(window.resetsAtMs, now)
  if (reset) parts.push(reset)
  return parts.join(' · ')
}

/** 套餐显示名（未收录原样回显）。 */
export function planLabel(planType: string | null): string | null {
  if (!planType) return null
  const known: Record<string, string> = {
    free: 'Free',
    plus: 'Plus',
    pro: 'Pro',
    business: 'Business',
    enterprise: 'Enterprise',
    edu: 'Edu',
  }
  return known[planType] ?? planType
}

/** credits 行文案。 */
export function creditsText(credits: QuotaCredits): string {
  if (credits.unlimited) return '不限量'
  if (credits.balance !== null) return credits.balance
  return credits.hasCredits ? '可用' : '无'
}

/** 取数时刻显示：HH:mm。 */
export function fetchedAtText(ms: number): string {
  const date = new Date(ms)
  const pad = (value: number) => String(value).padStart(2, '0')
  return `${pad(date.getHours())}:${pad(date.getMinutes())}`
}
