import { describe, expect, it } from 'vitest'
import {
  NAV_ITEMS,
  NAV_ORDER_KEY,
  isAppView,
  loadNavOrder,
  normalizeNavOrder,
  reorderNavItems,
  saveNavOrder,
} from '../src/nav'

describe('左侧导航', () => {
  it('视图键唯一，且每项都带提示与图标', () => {
    const views = NAV_ITEMS.map(item => item.view)
    expect(new Set(views).size).toBe(views.length)
    for (const item of NAV_ITEMS) {
      expect(item.label).not.toBe('')
      expect(item.hint).not.toBe('')
      expect(item.icon.length).toBeGreaterThan(0)
    }
  })

  it('持久化视图白名单覆盖全部导航项', () => {
    for (const item of NAV_ITEMS) {
      expect(isAppView(item.view)).toBe(true)
    }
  })

  it('拒绝空值与未知视图', () => {
    expect(isAppView(null)).toBe(false)
    expect(isAppView('home ')).toBe(false)
    expect(isAppView('settings')).toBe(false)
  })
})

function memoryStorage(initial: string | null = null) {
  let value = initial
  return {
    getItem(key: string) {
      return key === NAV_ORDER_KEY ? value : null
    },
    setItem(key: string, next: string) {
      if (key === NAV_ORDER_KEY) value = next
    },
    read() {
      return value
    },
  }
}

describe('左侧导航排序', () => {
  const defaultOrder = normalizeNavOrder(null)

  it('首页固定第一，未知项与重复项丢弃，缺失项补齐', () => {
    const order = normalizeNavOrder(['usage', 'home', 'nope', 'usage', 'search'])
    expect(order[0]).toBe('home')
    expect(order.slice(1, 3)).toEqual(['usage', 'search'])
    expect(order).toHaveLength(NAV_ITEMS.length)
    expect([...order].sort()).toEqual(NAV_ITEMS.map(item => item.view).sort())
  })

  it('把图标拖到首页之后或某个图标之前', () => {
    const top = reorderNavItems(defaultOrder, 'race', 1)
    expect(top.slice(0, 3)).toEqual(['home', 'race', 'archive'])

    const middle = reorderNavItems(defaultOrder, 'archive', 3)
    expect(middle.slice(0, 4)).toEqual(['home', 'workbench', 'archive', 'race'])
    expect([...middle].sort()).toEqual([...defaultOrder].sort())
  })

  it('首页不可移动，越界间隙会被夹紧', () => {
    expect(reorderNavItems(defaultOrder, 'home', 3)).toEqual(defaultOrder)
    expect(reorderNavItems(defaultOrder, 'race', 99).at(-1)).toBe('race')
    expect(reorderNavItems(defaultOrder, 'race', -99)[1]).toBe('race')
    expect(reorderNavItems(defaultOrder, 'race', Number.NaN)).toEqual(defaultOrder)
  })

  it('排序持久化，坏数据回退到默认顺序', () => {
    const storage = memoryStorage('{bad json')
    expect(loadNavOrder(storage)).toEqual(defaultOrder)

    const custom = reorderNavItems(defaultOrder, 'usage', 1)
    saveNavOrder(custom, storage)
    expect(loadNavOrder(storage)).toEqual(custom)
    expect(storage.read()).toContain('"usage"')
  })

  it('存储不可用时读写都不抛错', () => {
    const broken = {
      getItem() {
        throw new Error('blocked')
      },
      setItem() {
        throw new Error('quota')
      },
    }
    expect(loadNavOrder(broken)).toEqual(defaultOrder)
    expect(() => saveNavOrder(defaultOrder, broken)).not.toThrow()
    expect(() => saveNavOrder(defaultOrder, null)).not.toThrow()
  })
})
