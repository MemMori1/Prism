/**
 * 「最近打开」：网页与文件。
 *
 * 纯前端记忆（localStorage），不落引擎档案——它只是「下次少输一会儿 / 少找一会儿」
 * 的便利，丢了不影响任何已有数据。key 的命名与 `projectPins` / `panes` 一致：
 * 一个功能一个 key，带版本号，结构变了直接换 v2。
 */

import { ref } from 'vue'

export interface RecentPage {
  url: string
  title: string
  /** 最近一次打开的时间（毫秒）。 */
  at: number
}

export interface RecentFile {
  path: string
  /** 打开时所在的项目目录：换项目时优先列本项目的文件。 */
  directory: string
  at: number
}

export const PAGES_KEY = 'prism-recent-pages-v1'
export const FILES_KEY = 'prism-recent-files-v1'

/** 存多少条：够翻，又不至于把 localStorage 撑大。 */
export const RECENT_LIMIT = 30
/** 空状态里一次列几条。 */
export const RECENT_SHOWN = 6

/** 无窗环境（node 单测）里没有 localStorage：拿不到就当没存过。 */
function storage(): Storage | null {
  try {
    return typeof window === 'undefined' ? null : window.localStorage
  } catch {
    return null
  }
}

/** 文件改动 / 网页状态都可能连着来，这里只做「去重 + 提到最前 + 截断」。 */
export function pushRecent<T>(
  list: T[],
  item: T,
  keyOf: (value: T) => string,
  limit = RECENT_LIMIT,
): T[] {
  const key = keyOf(item)
  if (!key) return list
  return [item, ...list.filter(entry => keyOf(entry) !== key)].slice(0, limit)
}

export const recentPages = ref<RecentPage[]>([])
export const recentFiles = ref<RecentFile[]>([])

let loaded = false

function read<T>(key: string): T[] {
  const store = storage()
  if (!store) return []
  try {
    const raw = store.getItem(key)
    if (!raw) return []
    const parsed: unknown = JSON.parse(raw)
    return Array.isArray(parsed) ? (parsed as T[]) : []
  } catch {
    // 坏数据按空处理：这是便利功能，不该因为它崩掉面板。
    return []
  }
}

function write(key: string, value: unknown): void {
  const store = storage()
  if (!store) return
  try {
    store.setItem(key, JSON.stringify(value))
  } catch {
    // 配额满了就算了，下一次写入再试。
  }
}

/** 打开面板的浏览器标签时调一次：把盘上的记录读进内存。 */
export function ensureRecent(): void {
  if (loaded) return
  loaded = true
  recentPages.value = read<RecentPage>(PAGES_KEY)
  recentFiles.value = read<RecentFile>(FILES_KEY)
}

function usablePage(url: string): boolean {
  return /^https?:\/\//i.test(url)
}

/** 记一条网页（`about:blank` 之类不记，那不是「打开过」）。 */
export function rememberPage(url: string, title: string, now = Date.now()): void {
  if (!usablePage(url)) return
  const item: RecentPage = { url, title: title.trim(), at: now }
  recentPages.value = pushRecent(recentPages.value, item, page => page.url)
  write(PAGES_KEY, recentPages.value)
}

/** 记一条文件（同一路径只留最新一条，时间刷新）。 */
export function rememberFile(path: string, directory: string, now = Date.now()): void {
  if (!path) return
  const item: RecentFile = { path, directory, at: now }
  recentFiles.value = pushRecent(recentFiles.value, item, file => file.path)
  write(FILES_KEY, recentFiles.value)
}

/** 挑要展示的网页（纯函数，方便单测）：最新的在前。 */
export function pickPages(list: RecentPage[], limit = RECENT_SHOWN): RecentPage[] {
  return [...list].sort((a, b) => b.at - a.at).slice(0, limit)
}

/** 挑要展示的文件：先本项目，再其它项目（跨项目也能一键回去看）。 */
export function pickFiles(
  list: RecentFile[],
  directory: string | null,
  limit = RECENT_SHOWN,
): RecentFile[] {
  const sorted = [...list].sort((a, b) => b.at - a.at)
  if (!directory) return sorted.slice(0, limit)
  const same = sorted.filter(file => file.directory === directory)
  const other = sorted.filter(file => file.directory !== directory)
  return [...same, ...other].slice(0, limit)
}

export function pagesToShow(limit = RECENT_SHOWN): RecentPage[] {
  return pickPages(recentPages.value, limit)
}

export function filesToShow(directory: string | null, limit = RECENT_SHOWN): RecentFile[] {
  return pickFiles(recentFiles.value, directory, limit)
}

/** 显示用：`https://a.b/c` → `a.b`；解析不了就原样。 */
export function hostOf(url: string): string {
  try {
    const parsed = new URL(url)
    return parsed.host || url
  } catch {
    return url
  }
}

/** 显示用：把项目目录前缀去掉，只留相对路径。 */
export function relativePath(path: string, directory: string | null): string {
  if (!directory) return path
  const clean = directory.replace(/\/+$/, '')
  return clean && path.startsWith(`${clean}/`) ? path.slice(clean.length + 1) : path
}
