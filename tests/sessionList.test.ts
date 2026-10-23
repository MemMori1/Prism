import { describe, expect, it } from 'vitest'
import {
  blankSessionIds,
  filterSessionsByRange,
  isBlankSession,
  rangeToSince,
  sortSessions,
} from '../src/sessionList'
import type { SessionSummary } from '../src/types'

function makeSession(partial: Partial<SessionSummary> & { id: string }): SessionSummary {
  return {
    projectId: 'p',
    title: partial.id,
    directory: '/tmp',
    model: null,
    cost: 0,
    tokensInput: 0,
    tokensOutput: 0,
    tokensReasoning: 0,
    timeCreated: 0,
    timeUpdated: 0,
    ...partial,
  }
}

describe('sortSessions', () => {
  const sessions = [
    makeSession({ id: 'a', timeUpdated: 100, timeCreated: 300, tokensOutput: 10 }),
    makeSession({ id: 'b', timeUpdated: 300, timeCreated: 100, tokensInput: 100 }),
    makeSession({ id: 'c', timeUpdated: 200, timeCreated: 200, tokensReasoning: 50 }),
  ]

  it('最近活动排序', () => {
    expect(sortSessions(sessions, 'recent').map(s => s.id)).toEqual(['b', 'c', 'a'])
  })

  it('创建时间排序', () => {
    expect(sortSessions(sessions, 'created').map(s => s.id)).toEqual(['a', 'c', 'b'])
  })

  it('Token 总量排序', () => {
    expect(sortSessions(sessions, 'tokens').map(s => s.id)).toEqual(['b', 'c', 'a'])
  })

  it('不改动原数组', () => {
    const before = sessions.map(s => s.id)
    sortSessions(sessions, 'created')
    expect(sessions.map(s => s.id)).toEqual(before)
  })
})

describe('rangeToSince', () => {
  // 2026-10-17 是周六（本地时区）
  const now = new Date(2026, 9, 17, 15, 30, 0).getTime()

  it('all → null', () => {
    expect(rangeToSince('all', now)).toBeNull()
  })

  it('today → 当天 0 点', () => {
    expect(rangeToSince('today', now)).toBe(new Date(2026, 9, 17, 0, 0, 0).getTime())
  })

  it('week → 本周一 0 点', () => {
    expect(rangeToSince('week', now)).toBe(new Date(2026, 9, 12, 0, 0, 0).getTime())
  })

  it('month → 当月 1 日 0 点', () => {
    expect(rangeToSince('month', now)).toBe(new Date(2026, 9, 1, 0, 0, 0).getTime())
  })
})

describe('filterSessionsByRange', () => {
  const sessions = [
    makeSession({ id: 'old', timeUpdated: 100 }),
    makeSession({ id: 'new', timeUpdated: 500 }),
  ]

  it('null 表示全部', () => {
    expect(filterSessionsByRange(sessions, null)).toHaveLength(2)
  })

  it('按更新时间的下限过滤', () => {
    expect(filterSessionsByRange(sessions, 200).map(s => s.id)).toEqual(['new'])
  })
})

describe('isBlankSession', () => {
  it('没有任何 token 与成本 = 空会话', () => {
    expect(isBlankSession(makeSession({ id: 'blank' }))).toBe(true)
  })

  it('有任意一路 token 就不算空', () => {
    expect(isBlankSession(makeSession({ id: 'a', tokensInput: 1 }))).toBe(false)
    expect(isBlankSession(makeSession({ id: 'b', tokensOutput: 1 }))).toBe(false)
    expect(isBlankSession(makeSession({ id: 'c', tokensReasoning: 1 }))).toBe(false)
  })

  it('有成本也不算空（引擎只记了钱没记 token 的情况）', () => {
    expect(isBlankSession(makeSession({ id: 'paid', cost: 0.0001 }))).toBe(false)
  })
})

describe('blankSessionIds', () => {
  const sessions = [
    makeSession({ id: 'blank-1' }),
    makeSession({ id: 'blank-2' }),
    makeSession({ id: 'used', tokensInput: 800 }),
    makeSession({ id: 'starred-blank' }),
    makeSession({ id: 'closed-blank' }),
  ]
  const hidden = (id: string) => id === 'closed-blank'
  const starred = (id: string) => id === 'starred-blank'

  it('只收空会话，且排除已关闭与星标', () => {
    expect(blankSessionIds(sessions, hidden, starred)).toEqual(['blank-1', 'blank-2'])
  })

  it('星标是「我要留着」的表态，不参与一键关闭', () => {
    // 上面那条已经证明被排除了；这里单独再钉一次，防止有人把过滤条件删掉。
    expect(blankSessionIds([sessions[3]], hidden, starred)).toEqual([])
  })

  it('没有可关的就返回空数组', () => {
    expect(blankSessionIds([sessions[2]], hidden, starred)).toEqual([])
  })
})
