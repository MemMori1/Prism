import { describe, expect, it } from 'vitest'
import { barHeights, formatCost, formatTokens, totalTokens } from '../src/usageFormat'
import type { TokenTotals } from '../src/types'

describe('formatTokens', () => {
  it('紧凑显示且负数归零', () => {
    expect(formatTokens(0)).toBe('0')
    expect(formatTokens(999)).toBe('999')
    expect(formatTokens(1500)).toBe('1.5K')
    expect(formatTokens(2_500_000)).toBe('2.5M')
    expect(formatTokens(-5)).toBe('0')
  })
})

describe('formatCost', () => {
  it('三态显示', () => {
    expect(formatCost(null)).toBe('未收录')
    expect(formatCost(0)).toBe('免费')
    expect(formatCost(1.234)).toBe('$1.23')
    expect(formatCost(0.004)).toBe('<$0.01')
  })
})

describe('totalTokens', () => {
  it('各列相加', () => {
    const tokens: TokenTotals = { input: 1, output: 2, reasoning: 3, cacheRead: 4, cacheWrite: 5 }
    expect(totalTokens(tokens)).toBe(15)
  })
})

describe('barHeights', () => {
  it('相对峰值，零值占位', () => {
    expect(barHeights([0, 50, 100])).toEqual([2, 50, 100])
  })

  it('全零给最小高度', () => {
    expect(barHeights([0, 0, 0])).toEqual([2, 2, 2])
  })

  it('非零小值保底 3%', () => {
    const heights = barHeights([1, 1000])
    expect(heights[0]).toBeCloseTo(3)
    expect(heights[1]).toBeCloseTo(100)
  })
})
