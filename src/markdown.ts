import katex from 'katex'
import { marked, type TokenizerAndRendererExtension, type Tokens } from 'marked'
import { basename } from './format'

/**
 * 把引擎返回的 markdown 文本渲染成 HTML。
 *
 * 安全策略：
 * - 原始 HTML（含内联标签 / 块级标签）走 renderer.html 钩子，一律转义为纯文本；
 * - 链接/图片的 href/src 只允许 http(s)、mailto、锚点与相对路径；
 * - LaTeX 经 KaTeX 渲染（trust 关闭：\href 等扩展一律不生效，禁脚本注入）；
 * - ```html / ```svg 代码块渲染为沙箱预览（iframe sandbox 禁脚本 / img data URI，
 *   脚本不会执行、不能导航顶层页面）；
 * - 指向本地文件的 HTML / SVG / 图片链接转成交付物卡片（数据由后端按需读取，
 *   点击预览时再取内容）。
 * 渲染结果带缓存（同一条消息反复渲染时命中）。
 */

marked.setOptions({ gfm: true, breaks: true })

function escapeHtml(source: string): string {
  return source.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
}

function escapeAttr(source: string): string {
  return escapeHtml(source).replace(/"/g, '&quot;').replace(/'/g, '&#39;')
}

// ---- LaTeX（KaTeX）----

/** 单条公式的源码上限（字符），超过按原文展示，避免解析风暴。 */
const MATH_LIMIT = 4000

function renderMath(source: string, display: boolean): string {
  const tex = source.trim()
  if (!tex || tex.length > MATH_LIMIT) {
    return `<code class="md-math-fallback">${escapeHtml(tex)}</code>`
  }
  try {
    return katex.renderToString(tex, {
      displayMode: display,
      throwOnError: false,
      strict: 'ignore',
      trust: false,
      maxExpand: 1000,
    })
  } catch {
    return `<code class="md-math-fallback">${escapeHtml(tex)}</code>`
  }
}

interface MathToken extends Tokens.Generic {
  text: string
  display: boolean
}

function isMathToken(token: Tokens.Generic): token is MathToken {
  return typeof (token as MathToken).text === 'string'
}

/** 块级公式：$$…$$ / \[…\]。 */
const blockMath: TokenizerAndRendererExtension = {
  name: 'blockMath',
  level: 'block',
  tokenizer(src) {
    let match = /^\$\$([\s\S]+?)\$\$[ \t]*(?:\n|$)/.exec(src)
    if (match) {
      return { type: 'blockMath', raw: match[0], text: match[1].trim(), display: true }
    }
    match = /^\\\[([\s\S]+?)\\\][ \t]*(?:\n|$)/.exec(src)
    if (match) {
      return { type: 'blockMath', raw: match[0], text: match[1].trim(), display: true }
    }
  },
  renderer(token) {
    if (!isMathToken(token)) return ''
    return `<div class="md-math-block">${renderMath(token.text, true)}</div>\n`
  },
}

/** 行内公式：$…$ / \(…\)。内容首尾不得为空白；开头的 $ 后不得紧跟数字（避免误吃 $5）。 */
const inlineMath: TokenizerAndRendererExtension = {
  name: 'inlineMath',
  level: 'inline',
  start(src) {
    const dollar = src.indexOf('$')
    const paren = src.indexOf('\\(')
    if (dollar === -1) return paren === -1 ? undefined : paren
    if (paren === -1) return dollar
    return Math.min(dollar, paren)
  },
  tokenizer(src) {
    let match = /^\$\$([^\n]+?)\$\$/.exec(src)
    if (match) {
      return { type: 'inlineMath', raw: match[0], text: match[1].trim(), display: true }
    }
    match = /^\$(?![\s\d])((?:[^\n$]*[^\s$])?)\$/.exec(src)
    if (match) {
      return { type: 'inlineMath', raw: match[0], text: match[1].trim(), display: false }
    }
    match = /^\\\(([^\n]+?)\\\)/.exec(src)
    if (match) {
      return { type: 'inlineMath', raw: match[0], text: match[1].trim(), display: false }
    }
  },
  renderer(token) {
    if (!isMathToken(token)) return ''
    return `<span class="md-math-inline">${renderMath(token.text, false)}</span>`
  },
}

// ---- 代码块：HTML / SVG 沙箱预览 ----

/** 单块预览的源码上限（字符），超过按普通代码块展示。 */
const EMBED_LIMIT = 200 * 1024

function codeBlockHtml(source: string, lang: string): string {
  const langAttr = lang ? ` class="language-${escapeAttr(lang)}"` : ''
  return `<pre><code${langAttr}>${escapeHtml(source)}</code></pre>\n`
}

function renderHtmlEmbed(source: string): string {
  if (source.length > EMBED_LIMIT) return codeBlockHtml(source, 'html')
  return (
    `<div class="md-embed">` +
    `<div class="md-embed-head">HTML 预览 · 禁脚本沙箱</div>` +
    `<iframe class="md-embed-frame" sandbox="" srcdoc="${escapeAttr(source)}" referrerpolicy="no-referrer"></iframe>` +
    `<details class="md-embed-src"><summary>源码</summary>${codeBlockHtml(source, 'html')}</details>` +
    `</div>\n`
  )
}

function renderSvgEmbed(source: string): string {
  if (source.length > EMBED_LIMIT) return codeBlockHtml(source, 'svg')
  return (
    `<div class="md-embed">` +
    `<div class="md-embed-head">SVG 预览 · 禁脚本沙箱</div>` +
    `<iframe class="md-embed-svg" sandbox="" srcdoc="${escapeAttr(source)}" referrerpolicy="no-referrer"></iframe>` +
    `<details class="md-embed-src"><summary>源码</summary>${codeBlockHtml(source, 'svg')}</details>` +
    `</div>\n`
  )
}

marked.use({
  renderer: {
    html(token: Tokens.HTML | Tokens.Tag): string {
      return escapeHtml(token.text)
    },
    code(token: Tokens.Code): string {
      const lang = (token.lang ?? '').trim().split(/\s+/)[0]?.toLowerCase() ?? ''
      if (lang === 'html' || lang === 'htm') return renderHtmlEmbed(token.text)
      if (lang === 'svg') return renderSvgEmbed(token.text)
      return codeBlockHtml(token.text, lang)
    },
  },
  extensions: [blockMath, inlineMath],
})

const cache = new Map<string, string>()
const CACHE_LIMIT = 600

function sanitizeLinks(html: string): string {
  return html.replace(
    /\s(href|src)="([^"]*)"/gi,
    (match, attribute: string, url: string) => {
      const value = url.trim().toLowerCase()
      const safe =
        value.startsWith('http:') ||
        value.startsWith('https:') ||
        value.startsWith('mailto:') ||
        value.startsWith('#') ||
        value.startsWith('/') ||
        value.startsWith('./') ||
        value.startsWith('../')
      return safe ? match : ` ${attribute}="#"`
    },
  )
}

// ---- 交付物卡片（本地 HTML / SVG / 图片链接）----

const ARTIFACT_EXT: Record<string, 'image' | 'html' | 'svg'> = {
  png: 'image',
  jpg: 'image',
  jpeg: 'image',
  gif: 'image',
  webp: 'image',
  bmp: 'image',
  avif: 'image',
  html: 'html',
  htm: 'html',
  svg: 'svg',
}

const ARTIFACT_ICON: Record<string, string> = {
  image: '🖼',
  html: '🌐',
  svg: '◇',
}

/** 本地路径 → 可预览类型；http(s) / data / 锚点一律不认。 */
function artifactKindOf(url: string): 'image' | 'html' | 'svg' | null {
  const value = url.trim()
  const lower = value.toLowerCase()
  if (lower.startsWith('http:') || lower.startsWith('https:') || lower.startsWith('data:')) {
    return null
  }
  if (!(value.startsWith('/') || value.startsWith('./') || value.startsWith('../'))) return null
  const clean = value.split(/[?#]/)[0]
  const dot = clean.lastIndexOf('.')
  if (dot === -1) return null
  return ARTIFACT_EXT[clean.slice(dot + 1).toLowerCase()] ?? null
}

function artifactCard(url: string, kind: string): string {
  const clean = url.split(/[?#]/)[0]
  const name = basename(clean)
  const icon = ARTIFACT_ICON[kind] ?? '📄'
  return (
    `<a class="md-artifact" data-artifact-kind="${kind}" data-artifact-path="${escapeAttr(url)}"` +
    ` href="${escapeAttr(url)}" title="${escapeAttr(clean)}" rel="noreferrer">` +
    `<span class="md-artifact-icon" aria-hidden="true">${icon}</span>` +
    `<span class="md-artifact-name">${escapeHtml(name)}</span>` +
    `<span class="md-artifact-path">${escapeHtml(clean)}</span>` +
    `</a>`
  )
}

/** 把本地交付物链接/图片替换成卡片（先链接后独立图片，避免嵌套替换残留）。 */
function artifactify(html: string): string {
  let out = html.replace(
    /<a\s[^>]*href="([^"]*)"[^>]*>[\s\S]*?<\/a>/gi,
    (tag: string, href: string) => {
      const kind = artifactKindOf(href)
      return kind ? artifactCard(href, kind) : tag
    },
  )
  out = out.replace(/<img\s[^>]*>/gi, (tag: string) => {
    const src = /\ssrc="([^"]*)"/i.exec(tag)?.[1]
    if (!src) return tag
    const kind = artifactKindOf(src)
    return kind ? artifactCard(src, kind) : tag
  })
  return out
}

export function renderMarkdown(source: string): string {
  const cached = cache.get(source)
  if (cached !== undefined) return cached

  const html = artifactify(sanitizeLinks(marked.parse(source) as string))

  if (cache.size >= CACHE_LIMIT) cache.clear()
  cache.set(source, html)
  return html
}
