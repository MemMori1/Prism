import { marked, type Tokens } from 'marked'

/**
 * 把引擎返回的 markdown 文本渲染成 HTML。
 *
 * 安全策略：
 * - 原始 HTML（含内联标签 / 块级标签）走 renderer.html 钩子，一律转义为纯文本；
 * - 链接/图片的 href/src 只允许 http(s)、mailto、锚点与相对路径。
 * 渲染结果带缓存（同一条消息反复渲染时命中）。
 */

marked.setOptions({ gfm: true, breaks: true })

function escapeHtml(source: string): string {
  return source.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
}

marked.use({
  renderer: {
    html(token: Tokens.HTML | Tokens.Tag): string {
      return escapeHtml(token.text)
    },
  },
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

export function renderMarkdown(source: string): string {
  const cached = cache.get(source)
  if (cached !== undefined) return cached

  const html = sanitizeLinks(marked.parse(source) as string)

  if (cache.size >= CACHE_LIMIT) cache.clear()
  cache.set(source, html)
  return html
}
