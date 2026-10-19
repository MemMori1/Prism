/**
 * 文件类型 → 图标形状。
 *
 * 只在这里做一次判定，组件只负责把对应的路径画出来（图标是纯 SVG，
 * 不引入图标库：文件树里十几种类型，一个字体图标包太重了）。
 */

export type FileIconKind =
  | 'code'
  | 'style'
  | 'data'
  | 'markdown'
  | 'image'
  | 'shell'
  | 'text'
  | 'file'

const CODE = new Set([
  'ts', 'tsx', 'js', 'jsx', 'mjs', 'cjs', 'vue', 'svelte',
  'rs', 'py', 'go', 'java', 'kt', 'kts', 'swift', 'c', 'h', 'cc', 'cpp', 'hpp',
  'cs', 'rb', 'php', 'lua', 'dart', 'scala', 'gradle',
])

const STYLE = new Set(['css', 'scss', 'sass', 'less', 'styl'])

const DATA = new Set([
  'json', 'jsonc', 'json5', 'yaml', 'yml', 'toml', 'ini', 'env', 'lock', 'xml', 'plist',
])

const MARKDOWN = new Set(['md', 'markdown', 'mdx', 'rst'])

const IMAGE = new Set(['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp', 'avif', 'svg', 'ico', 'tiff'])

const SHELL = new Set(['sh', 'bash', 'zsh', 'fish', 'ps1', 'bat', 'cmd'])

const TEXT = new Set(['txt', 'log', 'csv', 'tsv'])

/** 没有扩展名但一眼能认出来的文件名。 */
const BY_NAME: Record<string, FileIconKind> = {
  dockerfile: 'shell',
  makefile: 'shell',
  procfile: 'shell',
  license: 'text',
  readme: 'markdown',
  '.gitignore': 'data',
  '.gitattributes': 'data',
  '.editorconfig': 'data',
  '.npmrc': 'data',
  '.env': 'data',
}

/** 文件名（或路径）→ 图标类型。 */
export function fileIconKind(name: string | null | undefined): FileIconKind {
  const clean = (name ?? '').split(/[?#]/)[0].trim()
  if (!clean) return 'file'
  const lowered = clean.toLowerCase()
  const base = lowered.split('/').pop() ?? lowered
  const byName = BY_NAME[base]
  if (byName) return byName
  const dot = base.lastIndexOf('.')
  if (dot <= 0 || dot === base.length - 1) return 'file'
  const ext = base.slice(dot + 1)
  if (CODE.has(ext)) return 'code'
  if (STYLE.has(ext)) return 'style'
  if (DATA.has(ext)) return 'data'
  if (MARKDOWN.has(ext)) return 'markdown'
  if (IMAGE.has(ext)) return 'image'
  if (SHELL.has(ext)) return 'shell'
  if (TEXT.has(ext)) return 'text'
  return 'file'
}

/**
 * 每种图标对应的 SVG 路径（24×24 画布、描边画法）。
 * 形状本身就是区分依据：`<>`＝代码、`#`＝样式、`{}`＝结构化数据、
 * `M↓`＝markdown、方框＝图片、`>_`＝终端、横线＝纯文本、纸页＝其他。
 */
export const FILE_ICON_PATHS: Record<FileIconKind, string[]> = {
  code: ['m8.5 8-3 4 3 4', 'm15.5 8 3 4-3 4'],
  style: ['M9.5 4.5 8 15', 'M15 4.5 13.5 15', 'M5 9.5h13', 'M4.5 13h13'],
  data: [
    'M10 4.5c-1.3 0-1.9.7-1.9 1.9v1.9c0 1-.6 1.7-1.6 1.7 1 0 1.6.7 1.6 1.7v1.9c0 1.2.6 1.9 1.9 1.9',
    'M14 4.5c1.3 0 1.9.7 1.9 1.9v1.9c0 1 .6 1.7 1.6 1.7-1 0-1.6.7-1.6 1.7v1.9c0 1.2-.6 1.9-1.9 1.9',
  ],
  markdown: ['M4 16.5V7.5l4 5 4-5v9', 'M17 7.5v6.5', 'm14.5 11.5 2.5 2.5 2.5-2.5'],
  image: [
    'M4.5 6.5a2 2 0 0 1 2-2h11a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2h-11a2 2 0 0 1-2-2Z',
    'M9 10.5a1.4 1.4 0 1 1-2.8 0 1.4 1.4 0 0 1 2.8 0Z',
    'm5 17 4.5-4.5 3 3 2.5-2.5 4 4',
  ],
  shell: ['M4 6.5h16v11H4Z', 'm7.5 10.5 2 2-2 2', 'M12.5 14.5h4'],
  text: ['M6 8h12', 'M6 12h12', 'M6 16h7'],
  file: ['M7 4.5h6.5L18 9v10.5a1 1 0 0 1-1 1H7a1 1 0 0 1-1-1v-14a1 1 0 0 1 1-1Z', 'M13.5 4.5V9H18'],
}
