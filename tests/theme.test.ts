import { describe, expect, it } from 'vitest'
import { normalizeTheme, resolveTheme, THEME_KEY } from '../src/theme'

describe('theme', () => {
  it('defaults unknown or missing values to system', () => {
    expect(normalizeTheme(null)).toBe('system')
    expect(normalizeTheme('')).toBe('system')
    expect(normalizeTheme('sepia')).toBe('system')
    expect(THEME_KEY).toBe('prism-theme')
  })

  it('keeps explicit light and dark modes', () => {
    expect(normalizeTheme('light')).toBe('light')
    expect(normalizeTheme('dark')).toBe('dark')
  })

  it('resolves system against the OS preference', () => {
    expect(resolveTheme('system', true)).toBe('dark')
    expect(resolveTheme('system', false)).toBe('light')
    expect(resolveTheme('light', true)).toBe('light')
    expect(resolveTheme('dark', false)).toBe('dark')
  })
})
