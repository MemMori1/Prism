import { describe, expect, it } from 'vitest'
import { PANE_LABEL, closeTab, makeTab, moveTab, pickTab } from '../src/paneTabs'
import type { PaneTab } from '../src/paneTabs'

function tab(id: string, kind: PaneTab['kind']): PaneTab {
  return makeTab(id, kind)
}

describe('PANE_LABEL', () => {
  it('每种标签都有自己的名字', () => {
    expect(PANE_LABEL).toEqual({
      empty: '新标签页',
      files: '文件',
      term: '终端',
      web: '浏览器',
    })
  })
})

describe('pickTab', () => {
  it('当前是空标签就直接用它（新增标签页后选内容 → 替换当前标签）', () => {
    const tabs = [tab('a', 'files'), tab('b', 'empty')]
    const picked = pickTab(tabs, 'b', 'term', 'c')
    expect(picked.tabs).toBe(tabs)
    expect(picked.tabId).toBe('b')
  })

  it('当前不是空标签时复用同类型标签，不堆重复的', () => {
    const tabs = [tab('a', 'files'), tab('b', 'term')]
    expect(pickTab(tabs, 'b', 'files', 'c').tabId).toBe('a')
    expect(pickTab(tabs, 'b', 'term', 'c').tabId).toBe('b')
  })

  it('浏览器标签也能复用（不会开第二个浏览器）', () => {
    const tabs = [tab('a', 'files'), tab('b', 'web')]
    expect(pickTab(tabs, 'a', 'web', 'c').tabId).toBe('b')
    expect(pickTab(tabs, 'a', 'web', 'c').tabs).toHaveLength(2)
  })

  it('没有同类型的就新建一个', () => {
    const tabs = [tab('a', 'files')]
    const picked = pickTab(tabs, 'a', 'term', 'b')
    expect(picked.tabs.map(item => item.id)).toEqual(['a', 'b'])
    expect(picked.tabs[1].kind).toBe('term')
  })

  it('空标签优先于复用（不会把内容塞到别的文件标签里）', () => {
    const tabs = [tab('a', 'files'), tab('b', 'empty')]
    const picked = pickTab(tabs, 'b', 'files', 'c')
    expect(picked.tabId).toBe('b')
    expect(picked.tabs).toHaveLength(2)
  })
})

describe('closeTab', () => {
  it('关掉当前标签后落到右边的邻居', () => {
    const tabs = [tab('a', 'files'), tab('b', 'term'), tab('c', 'files')]
    const next = closeTab(tabs, 'b', 'b')
    expect(next.tabs.map(item => item.id)).toEqual(['a', 'c'])
    expect(next.activeId).toBe('c')
    expect(next.closed).toBe(false)
  })

  it('关掉最后一个时落到左边', () => {
    const tabs = [tab('a', 'files'), tab('b', 'term')]
    expect(closeTab(tabs, 'b', 'b').activeId).toBe('a')
  })

  it('关掉不是当前的标签，当前选择不动', () => {
    const tabs = [tab('a', 'files'), tab('b', 'term')]
    const next = closeTab(tabs, 'a', 'b')
    expect(next.activeId).toBe('a')
    expect(next.tabs.map(item => item.id)).toEqual(['a'])
  })

  it('关掉最后一个标签 = 收起侧栏', () => {
    const next = closeTab([tab('a', 'files')], 'a', 'a')
    expect(next.tabs).toEqual([])
    expect(next.closed).toBe(true)
  })

  it('id 不存在时原样返回', () => {
    const tabs = [tab('a', 'files')]
    const next = closeTab(tabs, 'a', 'nope')
    expect(next.tabs).toBe(tabs)
    expect(next.closed).toBe(false)
  })
})

describe('moveTab', () => {
  const ids = (list: PaneTab[]) => list.map(item => item.id)

  it('往后拖：插到目标标签的右边', () => {
    const tabs = [tab('a', 'files'), tab('b', 'term'), tab('c', 'files'), tab('d', 'term')]
    expect(ids(moveTab(tabs, 'a', 'c', true))).toEqual(['b', 'c', 'a', 'd'])
  })

  it('往前拖：插到目标标签的左边', () => {
    const tabs = [tab('a', 'files'), tab('b', 'term'), tab('c', 'files')]
    expect(ids(moveTab(tabs, 'c', 'b', false))).toEqual(['a', 'c', 'b'])
  })

  it('拖到自己身上不动', () => {
    const tabs = [tab('a', 'files'), tab('b', 'term')]
    expect(moveTab(tabs, 'a', 'a', true)).toBe(tabs)
  })

  it('id 不认识时原样返回', () => {
    const tabs = [tab('a', 'files')]
    expect(moveTab(tabs, 'a', 'zzz', true)).toBe(tabs)
    expect(moveTab(tabs, 'zzz', 'a', true)).toBe(tabs)
  })

  it('不修改传入的数组（纯函数）', () => {
    const tabs = [tab('a', 'files'), tab('b', 'term')]
    moveTab(tabs, 'a', 'b', true)
    expect(ids(tabs)).toEqual(['a', 'b'])
  })
})
