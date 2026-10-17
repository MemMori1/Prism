/**
 * 搜索结果片段的拆段渲染。
 *
 * 后端（Rust）在命中片段里用 \u0001 / \u0002 控制字符标记高亮段；
 * 前端拆成 [{text, hit}] 用文本插值渲染，天然避免 HTML 注入。
 */

export interface SnippetSegment {
  text: string
  hit: boolean
}

export const SNIPPET_MARK_OPEN = '\u0001'
export const SNIPPET_MARK_CLOSE = '\u0002'

/** 把带标记的片段拆成若干段（容忍不闭合 / 多余标记）。 */
export function parseSnippet(snippet: string): SnippetSegment[] {
  const segments: SnippetSegment[] = []
  let buffer = ''
  let hit = false

  const flush = () => {
    if (buffer) {
      segments.push({ text: buffer, hit })
      buffer = ''
    }
  }

  for (const ch of snippet) {
    if (ch === SNIPPET_MARK_OPEN) {
      flush()
      hit = true
    } else if (ch === SNIPPET_MARK_CLOSE) {
      flush()
      hit = false
    } else {
      buffer += ch
    }
  }
  flush()
  return segments
}
