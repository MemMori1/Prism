/** 时间与数字的展示格式化。 */

export function relativeTime(ms: number): string {
  if (!ms) return ''
  const diff = Date.now() - ms
  const minute = 60_000
  const hour = 60 * minute
  const day = 24 * hour

  if (diff < minute) return '刚刚'
  if (diff < hour) return `${Math.floor(diff / minute)} 分钟前`
  if (diff < day) return `${Math.floor(diff / hour)} 小时前`
  if (diff < 2 * day) return '昨天'
  if (diff < 30 * day) return `${Math.floor(diff / day)} 天前`
  return fullDate(ms)
}

export function fullDate(ms: number): string {
  if (!ms) return ''
  const date = new Date(ms)
  const month = `${date.getMonth() + 1}`.padStart(2, '0')
  const day = `${date.getDate()}`.padStart(2, '0')
  return `${date.getFullYear()}-${month}-${day}`
}

export function fullTime(ms: number): string {
  if (!ms) return ''
  const date = new Date(ms)
  const hour = `${date.getHours()}`.padStart(2, '0')
  const minute = `${date.getMinutes()}`.padStart(2, '0')
  return `${fullDate(ms)} ${hour}:${minute}`
}

export function formatTokens(count: number): string {
  if (!count) return '0'
  if (count < 1000) return `${count}`
  if (count < 1_000_000) return `${(count / 1000).toFixed(1)}k`
  return `${(count / 1_000_000).toFixed(2)}M`
}

/** 用时：0.8s / 12s / 3m24s / 1h05m（0 或负数返回空串，调用方据此不显示）。 */
export function formatDuration(ms: number): string {
  if (!ms || ms <= 0) return ''
  if (ms < 10_000) return `${(ms / 1000).toFixed(1)}s`
  if (ms < 60_000) return `${Math.round(ms / 1000)}s`
  const seconds = Math.round(ms / 1000)
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes}m${`${seconds % 60}`.padStart(2, '0')}s`
  const hours = Math.floor(minutes / 60)
  return `${hours}h${`${minutes % 60}`.padStart(2, '0')}m`
}

export function formatCost(cost: number): string {
  if (!cost) return ''
  if (cost < 0.01) return `$${cost.toFixed(4)}`
  return `$${cost.toFixed(2)}`
}

export function formatBytes(size: number): string {
  if (!size) return '0 B'
  if (size < 1024) return `${size} B`
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`
  return `${(size / (1024 * 1024)).toFixed(1)} MB`
}

export function basename(path: string): string {
  const trimmed = path.replace(/\/+$/, '')
  const index = trimmed.lastIndexOf('/')
  return index === -1 ? trimmed : trimmed.slice(index + 1) || '/'
}
