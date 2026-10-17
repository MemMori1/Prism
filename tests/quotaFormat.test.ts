import { describe, expect, it } from 'vitest'
import {
  barWidth,
  creditsText,
  fetchedAtText,
  planLabel,
  resetInText,
  usedPercentText,
  windowLabel,
  windowMetaText,
} from '../src/quotaFormat'

describe('windowLabel', () => {
  it('时长归类', () => {
    expect(windowLabel(300)).toBe('5 小时窗口')
    expect(windowLabel(10080)).toBe('7 天窗口')
    expect(windowLabel(2880)).toBe('2 天窗口')
    expect(windowLabel(90)).toBe('90 分钟窗口')
    expect(windowLabel(null)).toBe('配额窗口')
    expect(windowLabel(0)).toBe('配额窗口')
  })
})

describe('usedPercentText / barWidth', () => {
  it('百分比与夹紧', () => {
    expect(usedPercentText(12.34)).toBe('12.3%')
    expect(usedPercentText(100)).toBe('100%')
    expect(usedPercentText(-1)).toBe('0%')
    expect(barWidth(150)).toBe(100)
    expect(barWidth(-5)).toBe(0)
    expect(barWidth(42.5)).toBe(42.5)
  })
})

describe('resetInText', () => {
  const now = 1_790_000_000_000
  it('相对时间分档', () => {
    expect(resetInText(now + 3 * 3_600_000 + 12 * 60_000, now)).toBe('3 小时 12 分后重置')
    expect(resetInText(now + 3 * 3_600_000, now)).toBe('3 小时后重置')
    expect(resetInText(now + 2 * 86_400_000 + 3_600_000, now)).toBe('2 天 1 小时后重置')
    expect(resetInText(now + 12 * 60_000, now)).toBe('12 分钟后重置')
    expect(resetInText(now + 30_000, now)).toBe('1 分钟内重置')
    expect(resetInText(now - 1, now)).toBe('已到重置时间')
    expect(resetInText(null, now)).toBeNull()
  })
})

describe('planLabel / creditsText / fetchedAtText / windowMetaText', () => {
  it('套餐与 credits', () => {
    expect(planLabel('plus')).toBe('Plus')
    expect(planLabel('pro')).toBe('Pro')
    expect(planLabel('team-x')).toBe('team-x')
    expect(planLabel(null)).toBeNull()
    expect(creditsText({ hasCredits: true, unlimited: true, balance: null })).toBe('不限量')
    expect(creditsText({ hasCredits: true, unlimited: false, balance: '12.34' })).toBe('12.34')
    expect(creditsText({ hasCredits: true, unlimited: false, balance: null })).toBe('可用')
    expect(creditsText({ hasCredits: false, unlimited: false, balance: null })).toBe('无')
  })

  it('取数时刻为 HH:mm', () => {
    expect(fetchedAtText(Date.now())).toMatch(/^\d{2}:\d{2}$/)
  })

  it('窗口行文案拼接', () => {
    const now = 1_790_000_000_000
    expect(windowMetaText({ usedPercent: 12.5, windowMinutes: 300, resetsAtMs: now + 3_600_000 }, now)).toBe(
      '12.5% · 1 小时后重置',
    )
    expect(windowMetaText({ usedPercent: 0, windowMinutes: null, resetsAtMs: null }, now)).toBe('0%')
  })
})
