import { describe, expect, it } from 'vitest'
import { formatDuration } from '../src/format'

describe('formatDuration（每轮用时）', () => {
  it('不足 10 秒给一位小数，好看出快慢', () => {
    expect(formatDuration(320)).toBe('0.3s')
    expect(formatDuration(8_500)).toBe('8.5s')
  })

  it('10 秒到 1 分钟按秒取整', () => {
    expect(formatDuration(12_400)).toBe('12s')
    expect(formatDuration(59_600)).toBe('60s')
  })

  it('过分钟给 分+秒，过小时给 时+分', () => {
    expect(formatDuration(204_000)).toBe('3m24s')
    expect(formatDuration(3_900_000)).toBe('1h05m')
  })

  it('0 / 负数给空串（调用方据此不显示）', () => {
    expect(formatDuration(0)).toBe('')
    expect(formatDuration(-5)).toBe('')
  })
})
