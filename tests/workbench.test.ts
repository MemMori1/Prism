import { describe, expect, it } from 'vitest'
import {
  clampColumnWidth,
  parseWorkbenchLayout,
  serializeWorkbenchLayout,
  WORKBENCH_DEFAULT_WIDTH,
  WORKBENCH_MAX_WIDTH,
  WORKBENCH_MIN_WIDTH,
} from '../src/workbench'

describe('clampColumnWidth', () => {
  it('夹到最小 / 最大区间', () => {
    expect(clampColumnWidth(100)).toBe(WORKBENCH_MIN_WIDTH)
    expect(clampColumnWidth(10_000)).toBe(WORKBENCH_MAX_WIDTH)
    expect(clampColumnWidth(520)).toBe(520)
  })

  it('取整；非法数字回落默认宽', () => {
    expect(clampColumnWidth(520.6)).toBe(521)
    expect(clampColumnWidth(Number.NaN)).toBe(WORKBENCH_DEFAULT_WIDTH)
    expect(clampColumnWidth(Number.POSITIVE_INFINITY)).toBe(WORKBENCH_DEFAULT_WIDTH)
  })
})

describe('parseWorkbenchLayout', () => {
  it('正常解析并夹宽', () => {
    const raw = JSON.stringify([
      { sessionId: 'a', width: 500 },
      { sessionId: 'b', width: 10 },
    ])
    expect(parseWorkbenchLayout(raw)).toEqual([
      { sessionId: 'a', width: 500 },
      { sessionId: 'b', width: WORKBENCH_MIN_WIDTH },
    ])
  })

  it('缺宽度时用默认宽', () => {
    expect(parseWorkbenchLayout(JSON.stringify([{ sessionId: 'x' }]))).toEqual([
      { sessionId: 'x', width: WORKBENCH_DEFAULT_WIDTH },
    ])
  })

  it('坏数据容错：null / 非 JSON / 非数组 / 条目缺 id', () => {
    expect(parseWorkbenchLayout(null)).toEqual([])
    expect(parseWorkbenchLayout('{oops')).toEqual([])
    expect(parseWorkbenchLayout('"str"')).toEqual([])
    expect(
      parseWorkbenchLayout(JSON.stringify([null, 42, { width: 500 }, { sessionId: '' }, { sessionId: 'ok' }])),
    ).toEqual([{ sessionId: 'ok', width: WORKBENCH_DEFAULT_WIDTH }])
  })
})

describe('serializeWorkbenchLayout', () => {
  it('往返一致且夹宽', () => {
    const entries = [
      { sessionId: 'a', width: 500.4 },
      { sessionId: 'b', width: 5 },
    ]
    const parsed = parseWorkbenchLayout(serializeWorkbenchLayout(entries))
    expect(parsed).toEqual([
      { sessionId: 'a', width: 500 },
      { sessionId: 'b', width: WORKBENCH_MIN_WIDTH },
    ])
  })
})
