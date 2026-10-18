import { describe, expect, it } from 'vitest'
import { filterNavSessions, mergeRecentSessions } from '../src/sessionNav'
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
