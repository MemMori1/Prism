import { describe, expect, it } from 'vitest'
import { SETTINGS_SECTIONS, isSettingsSection } from '../src/settingsSections'

describe('设置工作区分类', () => {
  it('分类顺序与截图一致，且每项都有标题、说明与图标', () => {
    expect(SETTINGS_SECTIONS.map(section => section.label)).toEqual([
      '系统',
      '外观',
      '模型',
      '引擎',
      '工具链',
      '渠道',
      '手机链接',
      '智能增强',
      '扩展能力',
      '权限',
    ])
    for (const section of SETTINGS_SECTIONS) {
      expect(section.title).not.toBe('')
      expect(section.description).not.toBe('')
      expect(section.icon.length).toBeGreaterThan(0)
    }
  })

  it('分类 id 唯一，且校验函数覆盖全部条目', () => {
    const ids = SETTINGS_SECTIONS.map(section => section.id)
    expect(new Set(ids).size).toBe(ids.length)
    for (const id of ids) expect(isSettingsSection(id)).toBe(true)
  })

  it('拒绝空值与未知分类', () => {
    expect(isSettingsSection(null)).toBe(false)
    expect(isSettingsSection('system ')).toBe(false)
    expect(isSettingsSection('general')).toBe(false)
  })
})
