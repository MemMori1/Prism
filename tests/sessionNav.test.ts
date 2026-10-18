import { describe, expect, it } from 'vitest'
import {
  filterNavSessions,
  mergeRecentSessions,
  navAgentList,
  sessionsOfEngine,
} from '../src/sessionNav'
import type { SessionSummary } from '../src/types'

function session(id: string, title: string | null, timeUpdated: number): SessionSummary {
  return {
    engine: 'native',
    id,
    projectId: 'native:/tmp/a',
    title,
    directory: '/tmp/a',
    model: null,
    cost: 0,
    tokensInput: 0,
    tokensOutput: 0,
    tokensReasoning: 0,
    timeCreated: timeUpdated,
    timeUpdated,
  }
}

describe('mergeRecentSessions', () => {
  it('按更新时间倒序，跨项目混排', () => {
    const merged = mergeRecentSessions([
      { projectName: 'Prism', sessions: [session('s1', '旧的', 100)] },
      { projectName: 'hsbg-helper', sessions: [session('s2', '新的', 300)] },
    ])
    expect(merged.map(item => item.session.id)).toEqual(['s2', 's1'])
    expect(merged[0].projectName).toBe('hsbg-helper')
  })

  it('同一条会话只留一份（重复项目）', () => {
    const merged = mergeRecentSessions([
      { projectName: 'A', sessions: [session('s1', 'x', 100)] },
      { projectName: 'B', sessions: [session('s1', 'x', 100)] },
    ])
    expect(merged).toHaveLength(1)
    expect(merged[0].projectName).toBe('A')
  })

  it('截到 limit 条；limit 非正数＝空', () => {
    const groups = [
      {
        projectName: 'A',
        sessions: [session('s1', null, 3), session('s2', null, 2), session('s3', null, 1)],
      },
    ]
    expect(mergeRecentSessions(groups, 2).map(item => item.session.id)).toEqual(['s1', 's2'])
    expect(mergeRecentSessions(groups, 0)).toEqual([])
  })

  it('没有会话时返回空数组', () => {
    expect(mergeRecentSessions([])).toEqual([])
    expect(mergeRecentSessions([{ projectName: 'A', sessions: [] }])).toEqual([])
  })
})

describe('filterNavSessions', () => {
  const items = mergeRecentSessions([
    { projectName: 'hsbg-helper', sessions: [session('s1', '修 README', 200)] },
    { projectName: 'Prism', sessions: [session('s2', '顶部导航', 100), session('s3', null, 50)] },
  ])

  it('空词（含只有空格）＝全要', () => {
    expect(filterNavSessions(items, '')).toHaveLength(3)
    expect(filterNavSessions(items, '   ')).toHaveLength(3)
  })

  it('标题命中，大小写不敏感', () => {
    expect(filterNavSessions(items, 'readme').map(item => item.session.id)).toEqual(['s1'])
  })

  it('项目名也能命中', () => {
    expect(filterNavSessions(items, 'hsbg').map(item => item.session.id)).toEqual(['s1'])
    expect(filterNavSessions(items, 'prism').map(item => item.session.id)).toEqual(['s2', 's3'])
  })

  it('没命中＝空；标题为 null 不会炸', () => {
    expect(filterNavSessions(items, 'zzz')).toEqual([])
    expect(filterNavSessions(items, 's3')).toEqual([])
  })
})

describe('sessionsOfEngine（面板右栏只列选中 Agent 的会话）', () => {
  const items = [
    { session: { ...session('a', 'A', 300), engine: 'native' }, projectName: 'Prism' },
    { session: { ...session('b', 'B', 200), engine: 'opencode' }, projectName: 'Prism' },
    { session: { ...session('c', 'C', 100), engine: 'native' }, projectName: 'hsbg-helper' },
  ]

  it('只留这个 Agent 的，别的引擎再近也不出现', () => {
    expect(sessionsOfEngine(items, 'native').map(item => item.session.id)).toEqual(['a', 'c'])
    expect(sessionsOfEngine(items, 'opencode').map(item => item.session.id)).toEqual(['b'])
  })

  it('顺序还是最近优先（交给调用方前已经排好）', () => {
    expect(sessionsOfEngine(items, 'native')[0].session.id).toBe('a')
  })

  it('还没挑 Agent（null）＝没有可列的会话', () => {
    expect(sessionsOfEngine(items, null)).toEqual([])
  })

  it('这个 Agent 一条都没有＝空', () => {
    expect(sessionsOfEngine(items, 'codex')).toEqual([])
  })
})

describe('navAgentList（面板左列）', () => {
  const items = [
    { session: { ...session('a', 'A', 300), engine: 'native' }, projectName: 'Prism' },
    { session: { ...session('b', 'B', 200), engine: 'opencode' }, projectName: 'Prism' },
    { session: { ...session('c', 'C', 100), engine: 'native' }, projectName: 'hsbg-helper' },
  ]
  const known = [
    { id: 'native', available: true },
    { id: 'codex', available: false },
    { id: 'opencode', available: true },
  ]

  it('顺序照给定的来，会话数按最近会话算', () => {
    expect(navAgentList(items, known).map(item => [item.id, item.count])).toEqual([
      ['native', 2],
      ['codex', 0],
      ['opencode', 1],
    ])
  })

  it('一条会话都没有的 Agent 也留着（否则用户没法在左边点它）', () => {
    expect(navAgentList([], known)).toHaveLength(3)
  })

  it('可用性透传（置灰用）', () => {
    expect(navAgentList(items, known).find(item => item.id === 'codex')?.available).toBe(false)
  })

  it('最近会话里冒出来的陌生引擎补在后面，不会漏', () => {
    const extra = [{ session: { ...session('d', 'D', 400), engine: 'gemini' }, projectName: 'X' }]
    const list = navAgentList(extra, known)
    expect(list.map(item => item.id)).toEqual(['native', 'codex', 'opencode', 'gemini'])
    expect(list[3]).toEqual({ id: 'gemini', count: 1, available: true })
  })

  it('目录里重复的 id 只算一次', () => {
    const list = navAgentList(items, [...known, { id: 'native', available: true }])
    expect(list.filter(item => item.id === 'native')).toHaveLength(1)
  })
})
