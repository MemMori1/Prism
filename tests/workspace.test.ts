import { describe, expect, it } from 'vitest'
import {
  newColumn,
  newPage,
  pagesFromLegacyLayout,
  parseWorkspaceState,
  serializeWorkspaceState,
  WORKSPACE_KEY,
} from '../src/workspace'
import { WORKBENCH_DEFAULT_WIDTH, WORKBENCH_MIN_WIDTH } from '../src/workbench'

describe('newPage / newColumn 默认值', () => {
  it('新页面：还没挑 Agent、还没选目录、两栏页在前台、没有列', () => {
    const page = newPage()
    expect(page.agent).toBeNull()
    expect(page.directory).toBeNull()
    expect(page.startOpen).toBe(true)
    expect(page.focusedId).toBeNull()
    expect(page.columns).toEqual([])
  })

  it('页面 id 不重复', () => {
    expect(newPage().id).not.toBe(newPage().id)
  })

  it('新列：夹宽、概要还没有、没在跑', () => {
    const column = newColumn('a', 10)
    expect(column.width).toBe(WORKBENCH_MIN_WIDTH)
    expect(column.session).toBeNull()
    expect(column.status).toEqual({ running: false, waiting: 0 })
  })
})

describe('parseWorkspaceState', () => {
  const raw = JSON.stringify({
    currentId: 'p2',
    pages: [
      {
        id: 'p1',
        agent: 'opencode',
        directory: '/tmp/x',
        startOpen: false,
        focusedId: 'oc1',
        columns: [
          { sessionId: 'oc1', width: 500 },
          { sessionId: 'oc2', width: 9999 },
        ],
      },
      { id: 'p2', agent: null, directory: null, startOpen: true, focusedId: null, columns: [] },
    ],
  })

  it('正常解析：页面、列宽、当前页', () => {
    const state = parseWorkspaceState(raw)
    expect(state?.currentId).toBe('p2')
    expect(state?.pages.map(page => page.id)).toEqual(['p1', 'p2'])
    expect(state?.pages[0].agent).toBe('opencode')
    expect(state?.pages[0].directory).toBe('/tmp/x')
    expect(state?.pages[0].startOpen).toBe(false)
    expect(state?.pages[0].focusedId).toBe('oc1')
    expect(state?.pages[0].columns.map(column => column.width)).toEqual([500, 900])
    expect(state?.pages[1].agent).toBeNull()
    expect(state?.pages[1].columns).toEqual([])
  })

  it('运行期字段不落盘：概要与状态重建后是空的', () => {
    const state = parseWorkspaceState(raw)
    expect(state?.pages[0].columns[0].session).toBeNull()
    expect(state?.pages[0].columns[0].status).toEqual({ running: false, waiting: 0 })
  })

  it('聚焦的列不在本页时当没有（列可能已经被关掉）', () => {
    const state = parseWorkspaceState(
      JSON.stringify({ currentId: 'p1', pages: [{ id: 'p1', focusedId: 'gone', columns: [] }] }),
    )
    expect(state?.pages[0].focusedId).toBeNull()
  })

  it('当前页 id 对不上时回落到第一页', () => {
    const state = parseWorkspaceState(
      JSON.stringify({ currentId: 'nope', pages: [{ id: 'p1' }, { id: 'p2' }] }),
    )
    expect(state?.currentId).toBe('p1')
  })

  it('缺省的页面字段：startOpen 默认 true，agent/directory 默认 null', () => {
    const state = parseWorkspaceState(JSON.stringify({ pages: [{ id: 'p1' }] }))
    expect(state?.pages[0].startOpen).toBe(true)
    expect(state?.pages[0].agent).toBeNull()
    expect(state?.pages[0].directory).toBeNull()
  })

  it('坏数据容错：null / 非 JSON / 非对象 / 没有页面 / 页面缺 id', () => {
    expect(parseWorkspaceState(null)).toBeNull()
    expect(parseWorkspaceState('{oops')).toBeNull()
    expect(parseWorkspaceState('"str"')).toBeNull()
    expect(parseWorkspaceState('[]')).toBeNull()
    expect(parseWorkspaceState(JSON.stringify({ pages: [] }))).toBeNull()
    expect(parseWorkspaceState(JSON.stringify({ pages: [null, 42, { agent: 'x' }] }))).toBeNull()
  })

  it('页面里的坏列直接丢掉，好列留着', () => {
    const state = parseWorkspaceState(
      JSON.stringify({
        pages: [{ id: 'p1', columns: [null, 7, { width: 10 }, { sessionId: '' }, { sessionId: 'ok' }] }],
      }),
    )
    expect(state?.pages[0].columns.map(column => column.sessionId)).toEqual(['ok'])
  })
})

describe('serializeWorkspaceState', () => {
  it('往返一致：页面、列、当前页都留得住', () => {
    const page = newPage('codex', '/tmp/repo')
    page.startOpen = false
    page.focusedId = 'a'
    page.columns = [newColumn('a', 512), newColumn('b', 5)]
    const state = { pages: [page], currentId: page.id }

    const back = parseWorkspaceState(serializeWorkspaceState(state))
    expect(back?.currentId).toBe(page.id)
    expect(back?.pages[0].agent).toBe('codex')
    expect(back?.pages[0].directory).toBe('/tmp/repo')
    expect(back?.pages[0].startOpen).toBe(false)
    expect(back?.pages[0].focusedId).toBe('a')
    expect(back?.pages[0].columns.map(column => ({ ...column, width: column.width }))).toEqual([
      { sessionId: 'a', width: 512, session: null, status: { running: false, waiting: 0 } },
      { sessionId: 'b', width: WORKBENCH_MIN_WIDTH, session: null, status: { running: false, waiting: 0 } },
    ])
  })

  it('存的是页面数据（顶栏导航也读它），不是旧版列数组', () => {
    const page = newPage('native')
    const parsed = JSON.parse(serializeWorkspaceState({ pages: [page], currentId: page.id }))
    expect(Object.keys(parsed).sort()).toEqual(['currentId', 'pages'])
    expect(parsed.pages[0]).toMatchObject({ agent: 'native', directory: null, columns: [] })
  })
})

describe('pagesFromLegacyLayout（旧版单画布迁移：一列一页）', () => {
  it('每列迁成一页，列宽保留，Agent 留空等加载概要再认', () => {
    const pages = pagesFromLegacyLayout(
      JSON.stringify([
        { sessionId: 'native:1', width: 480 },
        { sessionId: 'oc1', width: 700 },
      ]),
    )
    expect(pages).toHaveLength(2)
    expect(pages[0].columns).toEqual([newColumn('native:1', 480)])
    expect(pages[1].columns).toEqual([newColumn('oc1', 700)])
    expect(pages[0].agent).toBeNull()
    expect(pages[1].agent).toBeNull()
    expect(pages[0].startOpen).toBe(false)
    expect(pages[0].focusedId).toBe('native:1')
  })

  it('没有旧布局 / 坏数据＝没有页面（调用方补一个空白页）', () => {
    expect(pagesFromLegacyLayout(null)).toEqual([])
    expect(pagesFromLegacyLayout('{oops')).toEqual([])
    expect(pagesFromLegacyLayout(JSON.stringify([]))).toEqual([])
  })
})

describe('存储键', () => {
  it('新键与旧键分开——迁移只读一次旧键', () => {
    expect(WORKSPACE_KEY).toBe('prism-workspace-v1')
  })
})
