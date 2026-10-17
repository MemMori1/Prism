import { describe, expect, it } from 'vitest'
import { NAV_ITEMS, isAppView } from '../src/nav'

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
