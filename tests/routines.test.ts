import { describe, expect, it } from 'vitest'
import { describeCron, statusLabel, statusTone, untilText } from '../src/routines'

describe('describeCron', () => {
  it('覆盖全部预设', () => {
    expect(describeCron('*/30 * * * *')).toBe('每 30 分钟')
    expect(describeCron('0 * * * *')).toBe('每小时整点')
    expect(describeCron('0 9 * * *')).toBe('每天 09:00')
    expect(describeCron('30 9 * * 1-5')).toBe('工作日 09:30')
    expect(describeCron('0 10 * * 1')).toBe('每周一 10:00')
  })

  it('常见组合', () => {
    expect(describeCron('15 * * * *')).toBe('每小时第 15 分')
    expect(describeCron('0 */2 * * *')).toBe('每 2 小时整点')
    expect(describeCron('0 9 1 * *')).toBe('每月 1 日 09:00')
    expect(describeCron('0 10 * * 7')).toBe('每周日 10:00')
    expect(describeCron('0 9 * * 0,6')).toBe('每周日、每周六 09:00')
  })

  it('描述不了的形态原样返回', () => {
    expect(describeCron('0 9 * *')).toBe('0 9 * *') // 段数不对
    expect(describeCron('5,20 9 * * *')).toBe('5,20 9 * * *') // 分钟列表
    expect(describeCron('0 9 1 3 *')).toBe('0 9 1 3 *') // 指定月份
    expect(describeCron('0 9-11 * * *')).toBe('0 9-11 * * *') // 小时区间
  })
})

describe('untilText', () => {
  it('按量级给出倒计时', () => {
    const now = 1_000_000_000_000
    expect(untilText(now - 1, now)).toBe('即将触发')
    expect(untilText(now + 30_000, now)).toBe('不到 1 分钟')
    expect(untilText(now + 5 * 60_000, now)).toBe('5 分钟后')
    expect(untilText(now + 3 * 3_600_000, now)).toBe('3 小时后')
    expect(untilText(now + 2 * 86_400_000, now)).toBe('2 天后')
  })
})

describe('statusLabel / statusTone', () => {
  it('状态映射完整', () => {
    expect(statusLabel(null)).toBe('未运行')
    expect(statusLabel('succeeded')).toBe('成功')
    expect(statusLabel('mystery')).toBe('mystery')
    expect(statusTone('succeeded')).toBe('ok')
    expect(statusTone('failed')).toBe('error')
    expect(statusTone('timeout')).toBe('warn')
    expect(statusTone('running')).toBe('run')
    expect(statusTone(null)).toBe('idle')
  })
})
