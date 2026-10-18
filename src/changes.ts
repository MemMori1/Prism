/**
 * 变更面板的纯逻辑：状态字母、路径拆分、文本行号。
 *
 * 放在这里是为了能单测——组件只管画。
 */

import type { ChangeFile } from './types'

/** 状态 → 一个字母（和 git status 同一套语义，列表左边那列）。 */
export function statusMark(status: ChangeFile['status']): string {
  switch (status) {
    case 'added':
      return 'A'
    case 'deleted':
      return 'D'
    case 'renamed':
      return 'R'
    default:
      return 'M'
  }
}

/** 状态 → 一句中文（悬停提示 / 无障碍标签）。 */
export function statusLabel(status: ChangeFile['status']): string {
  switch (status) {
    case 'added':
      return '新增'
    case 'deleted':
      return '删除'
    case 'renamed':
      return '重命名'
    default:
      return '修改'
  }
}

/** `src/styles/a.css` → 目录 `src/styles` + 文件名 `a.css`（列表里目录淡化）。 */
export function splitPath(rel: string): { dir: string; name: string } {
  const cut = rel.lastIndexOf('/')
  if (cut === -1) return { dir: '', name: rel }
  return { dir: rel.slice(0, cut), name: rel.slice(cut + 1) }
}

/** 预览用：这一行要不要按 JSON 上色（只看扩展名）。 */
export function isJsonPath(rel: string | null | undefined): boolean {
  return !!rel && rel.toLowerCase().endsWith('.json')
}

/**
 * 文本 → 带行号的数组。
 *
 * 末尾的换行不再多出一行空行（编辑器口径：文件以 \n 结尾就是 N 行）。
 */
export function numberedLines(text: string): Array<{ no: number; text: string }> {
  if (!text) return []
  const body = text.endsWith('\n') ? text.slice(0, -1) : text
  return body.split('\n').map((line, index) => ({ no: index + 1, text: line }))
}

/** 二进制 / 空文件的提示文案（没有内容可画时用）。 */
export function emptyTextHint(data: { binary: boolean; truncated: boolean } | null): string | null {
  if (!data) return null
  if (data.binary) return '这是二进制文件，不做文本预览'
  if (data.truncated) return '文件太大，只显示开头一段'
  return null
}

/**
 * 全部文件列表的关键词过滤（大小写不敏感，路径与文件名都算命中）。
 *
 * 项目文件动辄几千条，一次全画出来只会卡：截到 limit 条，剩下的靠关键词缩小。
 */
export function filterProjectFiles<T extends { path: string; name: string }>(
  files: T[],
  query: string,
  limit = 400,
): { items: T[]; total: number } {
  const needle = query.trim().toLowerCase()
  const matched = needle
    ? files.filter(
        file =>
          file.path.toLowerCase().includes(needle) || file.name.toLowerCase().includes(needle),
      )
    : files
  return { items: matched.slice(0, limit), total: matched.length }
}

/** 相对路径 → 绝对路径（会话目录 + 相对路径，兼容尾斜杠）。 */
export function joinPath(directory: string, rel: string): string {
  if (rel.startsWith('/')) return rel
  return `${directory.replace(/\/+$/, '')}/${rel.replace(/^\.\//, '')}`
}

/** 常见的源码 / 文本扩展名：没有斜杠时靠它判断「这是个文件」。 */
const CODE_EXT = new Set([
  'md', 'markdown', 'txt', 'json', 'jsonc', 'toml', 'yaml', 'yml', 'ini', 'env',
  'ts', 'tsx', 'js', 'jsx', 'mjs', 'cjs', 'vue', 'svelte', 'css', 'scss', 'less',
  'html', 'htm', 'svg', 'rs', 'py', 'go', 'java', 'kt', 'kts', 'swift', 'c', 'h',
  'cc', 'cpp', 'hpp', 'cs', 'rb', 'php', 'lua', 'sh', 'bash', 'zsh', 'fish',
  'ps1', 'sql', 'lock', 'gradle', 'xml', 'plist',
])

/**
 * 这段文本像不像一个文件路径（决定聊天里的行内代码要不要变成可点链接）。
 *
 * 保守一点：带空白、带协议、以 `-` 开头、`1.2.3` 这种纯版本号都不算——
 * 误标成链接比漏标更烦人。
 */
export function looksLikePath(raw: string): boolean {
  const text = raw.trim()
  if (!text || text.length > 240) return false
  if (/\s/.test(text)) return false
  if (text.includes('://')) return false
  if (text.startsWith('-') || text.startsWith('@') || text.startsWith('#')) return false
  if (/^[a-z][a-z0-9+.-]*:/i.test(text)) return false
  if (text.startsWith('//')) return false
  if (text.includes('/')) return true

  if (/^(README|LICENSE|Dockerfile|Makefile|Procfile)(\.[a-z]+)?$/i.test(text)) return true
  const dot = text.lastIndexOf('.')
  if (dot <= 0 || dot === text.length - 1) return false
  return CODE_EXT.has(text.slice(dot + 1).toLowerCase())
}

/** 预览方式：markdown 渲染、图片显示、其余按代码高亮。 */
export type FileKind = 'markdown' | 'image' | 'text'

const IMAGE_EXT = new Set(['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp', 'avif', 'svg', 'ico'])

export function fileKindOf(rel: string | null | undefined): FileKind {
  if (!rel) return 'text'
  const clean = rel.split(/[?#]/)[0]
  const dot = clean.lastIndexOf('.')
  if (dot === -1) return 'text'
  const ext = clean.slice(dot + 1).toLowerCase()
  if (ext === 'md' || ext === 'markdown' || ext === 'mdx') return 'markdown'
  if (IMAGE_EXT.has(ext)) return 'image'
  return 'text'
}
