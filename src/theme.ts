/**
 * 主题模式：system 跟随系统，light / dark 显式覆盖。
 *
 * 主题本身的颜色全部在 src/styles/tokens.css 中定义：
 * - 浅色：:root
 * - 深色：:root[data-theme='dark']
 * - 跟随系统：不写 data-theme，由 prefers-color-scheme 决定
 * 以后新增主题时，只需增加 token 块并在 applyTheme 中允许对应 data-theme 值。
 */

export type ThemeMode = 'system' | 'light' | 'dark'

export const THEME_KEY = 'prism-theme'

export const THEME_MODES: readonly ThemeMode[] = ['system', 'light', 'dark']

export function normalizeTheme(value: string | null): ThemeMode {
  return value === 'light' || value === 'dark' ? value : 'system'
}

export function resolveTheme(mode: ThemeMode, prefersDark: boolean): 'light' | 'dark' {
  if (mode === 'system') return prefersDark ? 'dark' : 'light'
  return mode
}

export function applyTheme(mode: ThemeMode, root: HTMLElement = document.documentElement): void {
  if (mode === 'system') {
    delete root.dataset.theme
  } else {
    root.dataset.theme = mode
  }
}

export function readTheme(storage: Pick<Storage, 'getItem'> = localStorage): ThemeMode {
  try {
    return normalizeTheme(storage.getItem(THEME_KEY))
  } catch {
    return 'system'
  }
}

export function setThemeMode(mode: ThemeMode): void {
  try {
    localStorage.setItem(THEME_KEY, mode)
  } catch {
    // 隐私模式 / 存储被禁用时仍然应用当前会话主题。
  }
  applyTheme(mode)
}

export function initTheme(): void {
  applyTheme(readTheme())
}
