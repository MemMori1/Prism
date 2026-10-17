/**
 * 定时任务展示辅助：计划预设、cron 人话描述、状态标签（纯函数，可单测）。
 *
 * describeCron 只覆盖常见形态（预设 + 简单组合），描述不了就原样返回，
 * 不猜、不糊弄——用户始终能看到原始表达式。
 */

export interface SchedulePreset {
  label: string
  cron: string
}

export const SCHEDULE_PRESETS: SchedulePreset[] = [
  { label: '每 30 分钟', cron: '*/30 * * * *' },
  { label: '每小时整点', cron: '0 * * * *' },
  { label: '每天 09:00', cron: '0 9 * * *' },
  { label: '工作日 09:30', cron: '30 9 * * 1-5' },
  { label: '每周一 10:00', cron: '0 10 * * 1' },
]

const WEEKDAYS = ['周日', '周一', '周二', '周三', '周四', '周五', '周六']

/** cron → 人话；无法描述的形态原样返回。 */
export function describeCron(expression: string): string {
  const fields = expression.trim().split(/\s+/)
  if (fields.length !== 5) return expression
  const [minute, hour, dom, month, dow] = fields as [string, string, string, string, string]
  if (month !== '*') return expression

  const minuteStep = stepOf(minute)
  if (minuteStep !== null && hour === '*' && dom === '*' && dow === '*') {
    return `每 ${minuteStep} 分钟`
  }

  const minuteValue = fixedNumber(minute, 0, 59)
  if (minuteValue === null) return expression

  if (hour === '*' && dom === '*' && dow === '*') {
    return minuteValue === 0 ? '每小时整点' : `每小时第 ${minuteValue} 分`
  }

  const hourStep = stepOf(hour)
  if (hourStep !== null && dom === '*' && dow === '*') {
    return minuteValue === 0
      ? `每 ${hourStep} 小时整点`
      : `每 ${hourStep} 小时的 :${pad(minuteValue)}`
  }

  const hourValue = fixedNumber(hour, 0, 23)
  if (hourValue === null) return expression
  const time = `${pad(hourValue)}:${pad(minuteValue)}`

  const dowText = describeDow(dow)
  if (dom === '*' && dowText) return `${dowText} ${time}`
  if (dom === '*' && dow === '*') return `每天 ${time}`

  const domValue = fixedNumber(dom, 1, 31)
  if (domValue !== null && dow === '*') return `每月 ${domValue} 日 ${time}`

  return expression
}

function pad(value: number): string {
  return `${value}`.padStart(2, '0')
}

function stepOf(field: string): number | null {
  const match = /^\*\/(\d+)$/.exec(field)
  if (!match) return null
  const step = Number(match[1])
  return step > 0 ? step : null
}

function fixedNumber(field: string, min: number, max: number): number | null {
  if (!/^\d+$/.test(field)) return null
  const value = Number(field)
  return value >= min && value <= max ? value : null
}

function describeDow(field: string): string | null {
  if (field === '*') return null
  if (field === '1-5') return '工作日'
  const days: string[] = []
  for (const part of field.split(',')) {
    if (!/^\d$/.test(part)) return null
    days.push(`每${WEEKDAYS[Number(part) % 7]}`)
  }
  return days.length ? days.join('、') : null
}

/** 未来时刻 → 倒计时文案。 */
export function untilText(target: number, now: number = Date.now()): string {
  const diff = target - now
  if (diff <= 0) return '即将触发'
  const minute = 60_000
  if (diff < minute) return '不到 1 分钟'
  if (diff < 60 * minute) return `${Math.round(diff / minute)} 分钟后`
  if (diff < 24 * 60 * minute) return `${Math.round(diff / (60 * minute))} 小时后`
  return `${Math.round(diff / (24 * 60 * minute))} 天后`
}

const STATUS_LABELS: Record<string, string> = {
  succeeded: '成功',
  failed: '失败',
  interrupted: '已打断',
  timeout: '超时',
  error: '出错',
  running: '运行中',
}

export function statusLabel(status: string | null): string {
  if (!status) return '未运行'
  return STATUS_LABELS[status] ?? status
}

export type StatusTone = 'ok' | 'error' | 'warn' | 'run' | 'idle'

export function statusTone(status: string | null): StatusTone {
  switch (status) {
    case 'succeeded':
      return 'ok'
    case 'failed':
    case 'error':
      return 'error'
    case 'timeout':
    case 'interrupted':
      return 'warn'
    case 'running':
      return 'run'
    default:
      return 'idle'
  }
}
