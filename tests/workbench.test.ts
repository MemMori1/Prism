import { describe, expect, it } from 'vitest'
import {
  clampColumnWidth,
  parseWorkbenchLayout,
  restoreColumnAction,
  serializeWorkbenchLayout,
  workbenchShowsStartPage,
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

describe('restoreColumnAction（恢复布局不许静默丢列）', () => {
  it('只有后端确认会话不存在才丢列', () => {
    expect(restoreColumnAction('missing')).toBe('drop')
  })

  it('查询失败必须留着——后端一次抖动不能替用户把列写没了', () => {
    expect(restoreColumnAction('error')).toBe('keep')
  })

  it('正常拿到概要也是留着（画成聊天框）', () => {
    expect(restoreColumnAction('ok')).toBe('keep')
  })
})

describe('workbenchShowsStartPage（进工作台默认落在两栏「新建会话」页）', () => {
  it('有列时也是两栏页在前台——localStorage 存着列也不落到多列画布', () => {
    expect(workbenchShowsStartPage(true, 1)).toBe(true)
    expect(workbenchShowsStartPage(true, 4)).toBe(true)
  })

  it('点「⊞ 并排看」后才看多列画布', () => {
    expect(workbenchShowsStartPage(false, 1)).toBe(false)
    expect(workbenchShowsStartPage(false, 4)).toBe(false)
  })

  it('一列都没有时永远是两栏页（没有列可看）', () => {
    expect(workbenchShowsStartPage(false, 0)).toBe(true)
    expect(workbenchShowsStartPage(true, 0)).toBe(true)
  })
})
