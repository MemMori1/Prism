/**
 * 代码块高亮：只做 JSON（助手最常贴的就是它），产出**已转义**的 HTML。
 *
 * 为什么自己写而不是引 prismjs / highlight.js：只看 JSON 一种语法，
 * 20 行的词法足够，省掉一整个高亮库的体积与主题表；其余语言按原文转义展示，
 * 和以前一样安全（不解析、不执行）。
 *
 * 口径：字符串 / 数字 / true|false|null 上色，键（后面紧跟冒号的字符串）单独一色，
 * 标点保持正文色——留白比七彩更耐看。
 */

function escapeHtml(source: string): string {
  return source.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
}

/**
 * 这个字符串后面是不是跟着冒号（即 JSON 的键）。
 *
 * 判断放在匹配结果里做（`("…")(\s*:)?`），不靠"向前找"：
 * 值里同样有冒号（`"https://a"`），只看字符串本身分不出来。
 */
const JSON_TOKEN =
  /("(?:[^"\\]|\\.)*")(\s*:)?|(-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?)|\b(true|false|null)\b/g

export function highlightJson(source: string): string {
  const out: string[] = []
  let last = 0
  JSON_TOKEN.lastIndex = 0
  let match: RegExpExecArray | null
  while ((match = JSON_TOKEN.exec(source)) !== null) {
    out.push(escapeHtml(source.slice(last, match.index)))
    if (match[1] !== undefined) {
      const isKey = match[2] !== undefined
      out.push(`<span class="tok-${isKey ? 'key' : 'string'}">${escapeHtml(match[1])}</span>`)
      if (match[2]) out.push(escapeHtml(match[2]))
    } else if (match[3] !== undefined) {
      out.push(`<span class="tok-number">${escapeHtml(match[3])}</span>`)
    } else {
      out.push(`<span class="tok-literal">${escapeHtml(match[4])}</span>`)
    }
    last = match.index + match[0].length
  }
  out.push(escapeHtml(source.slice(last)))
  return out.join('')
}

/** 按语言分发：目前只有 JSON 上色，其余原样转义。 */
export function highlightCode(source: string, lang: string): string {
  return lang === 'json' ? highlightJson(source) : escapeHtml(source)
}
