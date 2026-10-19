import { describe, expect, it } from 'vitest'
import {
  charsOfRecords,
  contextBreakdown,
  formatPercent,
  sumSlices,
  type ContextChars,
} from '../src/contextShare'

describe('contextBreakdown', () => {
  const chars: ContextChars = { system: 200, tools: 1800, tool: 6000, dialog: 2000 }

  it('四项按字符比例分摊，合计等于引擎给的总数', () => {
    const slices = contextBreakdown(10_000, 128_000, chars)
    expect(slices.map(s => s.label)).toEqual(['系统提示词', '工具定义', '工具结果', '对话消息'])
    // 10000 的 0.2% / 18% / 60% / 20%
    expect(slices.map(s => s.tokens)).toEqual([200, 1800, 6000, 2000])
    // 四舍五入会有一两点误差，允许 4 token 的偏差
    expect(Math.abs(sumSlices(slices) - 10_000)).toBeLessThanOrEqual(4)
  })

  it('百分比是「占窗口」而不是「占已用」', () => {
    const slices = contextBreakdown(10_000, 128_000, chars)
    const tool = slices.find(slice => slice.key === 'tool')
    expect(tool?.percent).toBeCloseTo(4.7, 1)
  })

  it('窗口未知时百分比为 null（不编分母）', () => {
    const slices = contextBreakdown(10_000, null, chars)
    expect(slices.every(slice => slice.percent === null)).toBe(true)
  })

  it('没有字符或没有用量＝空数组', () => {
    expect(contextBreakdown(10_000, 128_000, { system: 0, tools: 0, tool: 0, dialog: 0 })).toEqual([])
    expect(contextBreakdown(0, 128_000, chars)).toEqual([])
  })

  it('某一项为 0 时仍然列出（0.0% 也是信息）', () => {
    const slices = contextBreakdown(1000, 10_000, { ...chars, system: 0 })
    const system = slices.find(slice => slice.key === 'system')
    expect(system?.tokens).toBe(0)
    expect(system?.share).toBe(0)
  })
})

describe('charsOfRecords', () => {
  it('工具片段进「工具结果」，用户与助手的文字进「对话」', () => {
    const chars = charsOfRecords([
      { role: 'user', parts: [{ kind: 'text', text: '改一下 README' }] },
      { role: 'assistant', parts: [{ kind: 'text', text: '好的，我看看。' }] },
      { role: 'assistant', parts: [{ kind: 'tool', summary: 'README.md' }] },
    ])
    expect(chars.dialog).toBe('改一下 README'.length + '好的，我看看。'.length)
    expect(chars.tool).toBe('README.md'.length)
    expect(chars.system).toBe(0)
  })

  it('工具片段优先用 summary，没有就按空串算', () => {
    const chars = charsOfRecords([{ role: 'assistant', parts: [{ kind: 'tool', summary: null }] }])
    expect(chars.tool).toBe(0)
  })
})

describe('formatPercent', () => {
  it('一位小数，整数不带 .0', () => {
    expect(formatPercent(8)).toBe('8%')
    expect(formatPercent(8.14)).toBe('8.1%')
    expect(formatPercent(8.16)).toBe('8.2%')
    expect(formatPercent(0.04)).toBe('0%')
  })

  it('非有限值给破折号', () => {
    expect(formatPercent(Number.NaN)).toBe('—')
  })
})
