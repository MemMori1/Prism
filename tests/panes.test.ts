import { describe, expect, it } from 'vitest'
import {
  DEFAULT_PROJECTS,
  DEFAULT_SESSIONS,
  MAX_PROJECTS,
  MAX_SESSIONS,
  MIN_MAIN,
  MIN_PROJECTS,
  MIN_SESSIONS,
  fitWidths,
  maxWidthFor,
  resizeWidths,
  sanitizeWidths,
} from '../src/panes'

/** 宽容一点的窗口：两栏都还有拉伸余地。 */
const ROOMY = 1400

describe('sanitizeWidths', () => {
  it('没有记录时用默认值', () => {
    expect(sanitizeWidths(null)).toEqual({ projects: DEFAULT_PROJECTS, sessions: DEFAULT_SESSIONS })
    expect(sanitizeWidths(undefined)).toEqual({
      projects: DEFAULT_PROJECTS,
      sessions: DEFAULT_SESSIONS,
    })
  })

  it('把手改过的值夹回合法区间', () => {
    expect(sanitizeWidths({ projects: 10, sessions: 9999 })).toEqual({
      projects: MIN_PROJECTS,
      sessions: MAX_SESSIONS,
    })
  })

  it('非数字 / 缺字段时回落到默认值', () => {
    expect(sanitizeWidths({ projects: '300', sessions: null })).toEqual({
      projects: DEFAULT_PROJECTS,
      sessions: DEFAULT_SESSIONS,
    })
    expect(sanitizeWidths({ projects: Number.NaN })).toEqual({
      projects: DEFAULT_PROJECTS,
      sessions: DEFAULT_SESSIONS,
    })
  })
})

describe('resizeWidths', () => {
  const start = { projects: DEFAULT_PROJECTS, sessions: DEFAULT_SESSIONS }

  it('向右拖项目栏就加宽，会话栏不动', () => {
    expect(resizeWidths('projects', start, 60, ROOMY)).toEqual({
      projects: DEFAULT_PROJECTS + 60,
      sessions: DEFAULT_SESSIONS,
    })
  })

  it('不会拖到低于最小宽度', () => {
    expect(resizeWidths('projects', start, -500, ROOMY).projects).toBe(MIN_PROJECTS)
    expect(resizeWidths('sessions', start, -500, ROOMY).sessions).toBe(MIN_SESSIONS)
  })

  it('不会把主区域挤到低于最小宽度', () => {
    const available = DEFAULT_PROJECTS + DEFAULT_SESSIONS + MIN_MAIN + 40
    const next = resizeWidths('projects', start, 400, available)
    // 最多只能再拿 40px，剩下的留给主区域
    expect(next.projects).toBe(DEFAULT_PROJECTS + 40)
    const used = next.projects + next.sessions
    expect(available - used).toBe(MIN_MAIN)
  })

  it('会话栏同样受主区域下限约束', () => {
    const available = DEFAULT_PROJECTS + DEFAULT_SESSIONS + MIN_MAIN + 10
    const next = resizeWidths('sessions', start, 300, available)
    expect(next.sessions).toBe(DEFAULT_SESSIONS + 10)
  })

  it('窗口极窄时仍不低于最小值（宁可挤主区域也不把栏压没）', () => {
    const next = resizeWidths('projects', start, -300, 600)
    expect(next.projects).toBe(MIN_PROJECTS)
  })

  it('还没量到宽度时不卡在最小值', () => {
    expect(resizeWidths('projects', start, 80, 0).projects).toBe(DEFAULT_PROJECTS + 80)
  })
})

describe('maxWidthFor', () => {
  it('上限取「配置上限」与「预算剩余」中更小的那个', () => {
    const widths = { projects: 200, sessions: 300 }
    // 宽敞：只剩配置上限起作用
    expect(maxWidthFor('projects', widths, 2000)).toBe(MAX_PROJECTS)
    // 刚好够：projects 最多能到 available - handles - MIN_MAIN - sessions
    const available = 200 + 300 + MIN_MAIN + 50
    expect(maxWidthFor('projects', widths, available)).toBe(250)
  })
})

describe('fitWidths', () => {
  it('空间够时原样返回', () => {
    const widths = { projects: 240, sessions: 300 }
    expect(fitWidths(widths, ROOMY)).toEqual(widths)
  })

  it('空间不够先收会话栏', () => {
    // 两栏 240 + 320 = 560，预算 460 → 差 100，正好由会话栏出（320 → 220）
    const available = 460 + MIN_MAIN
    const next = fitWidths({ projects: 240, sessions: 320 }, available)
    expect(next.projects).toBe(240)
    expect(next.sessions).toBe(MIN_SESSIONS)
    expect(next.projects + next.sessions).toBe(460)
  })

  it('会话栏到底后继续收项目栏', () => {
    // 两栏 300 + 400 = 700，预算 500 → 会话栏先让 180，剩下的 20 由项目栏出
    const available = 500 + MIN_MAIN
    const next = fitWidths({ projects: 300, sessions: 400 }, available)
    expect(next.sessions).toBe(MIN_SESSIONS)
    expect(next.projects).toBe(280)
    expect(next.projects + next.sessions).toBe(500)
  })

  it('窗口小到极限时两栏都停在最小值，不出现负宽度', () => {
    const next = fitWidths({ projects: 216, sessions: 264 }, 400)
    expect(next).toEqual({ projects: MIN_PROJECTS, sessions: MIN_SESSIONS })
  })

  it('记录里超限的宽度先夹回区间', () => {
    expect(fitWidths({ projects: 9999, sessions: 1 }, ROOMY)).toEqual({
      projects: MAX_PROJECTS,
      sessions: MIN_SESSIONS,
    })
  })
})
