/**
 * 代码高亮：JSON 走专用词法，常见语言走一个通用扫描器，产出**已转义**的 HTML。
 *
 * 为什么不引 prismjs / highlight.js：只需要「字符串 / 注释 / 数字 / 关键字」四类颜色，
 * 一百行的扫描器就够，省掉一整个高亮库的体积与主题表。拿不准的地方一律当普通文本，
 * 输出全部经过转义，不会因为高亮把 XSS 引进来。
 *
 * 口径：留白比七彩耐看——标点、变量名保持正文色，只给四类上色。
 */

function escapeHtml(source: string): string {
  return source.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
}

// ---- JSON（键要单独一色，所以单独一套）----

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

// ---- 通用扫描器 ----

export type LangFamily =
  | 'json'
  | 'js'
  | 'rust'
  | 'python'
  | 'go'
  | 'shell'
  | 'css'
  | 'text'

interface LangSpec {
  keywords: Set<string>
  /** 行注释前缀（按顺序匹配，长的在前）。 */
  line: string[]
  /** 块注释：只有成对的才支持。 */
  block?: [string, string]
  /** 字符串引号（单双引号、反引号）。 */
  quotes: string[]
}

function kw(words: string): Set<string> {
  return new Set(words.split(/\s+/).filter(Boolean))
}

const JS_KEYWORDS = kw(`
  const let var function return if else for while do switch case break continue default
  class extends super new this import export from as async await yield try catch finally throw
  typeof instanceof in of delete void interface type enum implements public private protected
  readonly static get set declare namespace abstract
  true false null undefined
`)

const SPECS: Record<string, LangSpec> = {
  js: {
    keywords: JS_KEYWORDS,
    line: ['//'],
    block: ['/*', '*/'],
    quotes: ["'", '"', '`'],
  },
  rust: {
    keywords: kw(`
      fn let mut pub use mod struct enum impl trait for while loop if else match return self Self
      crate super where async await move ref dyn const static type in as unsafe extern true false
      Some None Ok Err
    `),
    line: ['//'],
    block: ['/*', '*/'],
    quotes: ['"'],
  },
  python: {
    keywords: kw(`
      def class import from as return if elif else for while in not and or try except finally with
      lambda None True False raise yield async await global nonlocal pass break continue self del
      assert
    `),
    line: ['#'],
    quotes: ["'", '"'],
  },
  go: {
    keywords: kw(`
      func package import var const type struct interface map chan go defer return if else for range
      switch case default select break continue nil true false fallthrough goto
    `),
    line: ['//'],
    block: ['/*', '*/'],
    quotes: ['"', '`'],
  },
  shell: {
    keywords: kw(`
      if then else elif fi for in do done case esac function return export local while until
      echo cd set source
    `),
    line: ['#'],
    quotes: ["'", '"'],
  },
  css: {
    keywords: kw(`important media import keyframes from`),
    line: [],
    block: ['/*', '*/'],
    quotes: ["'", '"'],
  },
}

/** 扩展名 → 语言家族（决定了用哪套词法 / 要不要走预览）。 */
export function languageOfPath(path: string | null | undefined): LangFamily {
  if (!path) return 'text'
  const clean = path.split(/[?#]/)[0]
  const dot = clean.lastIndexOf('.')
  if (dot === -1) return 'text'
  const ext = clean.slice(dot + 1).toLowerCase()
  switch (ext) {
    case 'json':
    case 'jsonc':
      return 'json'
    case 'js':
    case 'jsx':
    case 'mjs':
    case 'cjs':
    case 'ts':
    case 'tsx':
    case 'vue':
    case 'svelte':
      return 'js'
    case 'rs':
      return 'rust'
    case 'py':
      return 'python'
    case 'go':
      return 'go'
    case 'sh':
    case 'bash':
    case 'zsh':
    case 'fish':
      return 'shell'
    case 'css':
    case 'scss':
    case 'less':
      return 'css'
    default:
      return 'text'
  }
}

/** 这一行的扫描结果：HTML + 行尾是不是还在块注释里。 */
function scanLine(line: string, spec: LangSpec, inBlock: boolean): { html: string; open: boolean } {
  const out: string[] = []
  let index = 0
  let block = inBlock

  const pushText = (text: string) => {
    if (text) out.push(escapeHtml(text))
  }

  while (index < line.length) {
    // 块注释里：一直吃到结束标记（可能这行吃不完）。
    if (block) {
      const close = spec.block?.[1] ?? '*/'
      const end = line.indexOf(close, index)
      if (end === -1) {
        out.push(`<span class="tok-comment">${escapeHtml(line.slice(index))}</span>`)
        return { html: out.join(''), open: true }
      }
      out.push(`<span class="tok-comment">${escapeHtml(line.slice(index, end + close.length))}</span>`)
      index = end + close.length
      block = false
      continue
    }

    const rest = line.slice(index)

    // 行注释：吃到行尾。
    const lineHit = spec.line.find(mark => rest.startsWith(mark))
    if (lineHit) {
      out.push(`<span class="tok-comment">${escapeHtml(rest)}</span>`)
      break
    }

    // 块注释开始。
    if (spec.block && rest.startsWith(spec.block[0])) {
      block = true
      continue
    }

    const char = rest[0]

    // 字符串：一路吃到同款引号（转义跳过）。
    if (spec.quotes.includes(char)) {
      let cursor = index + 1
      while (cursor < line.length) {
        if (line[cursor] === '\\') {
          cursor += 2
          continue
        }
        if (line[cursor] === char) {
          cursor += 1
          break
        }
        cursor += 1
      }
      out.push(`<span class="tok-string">${escapeHtml(line.slice(index, cursor))}</span>`)
      index = cursor
      continue
    }

    // 数字。
    if (/[0-9]/.test(char) && !/[\w$]/.test(line[index - 1] ?? '')) {
      const match = /^(?:0[xX][0-9a-fA-F]+|\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)/.exec(rest)
      if (match) {
        out.push(`<span class="tok-number">${escapeHtml(match[0])}</span>`)
        index += match[0].length
        continue
      }
    }

    // 标识符 / 关键字。
    if (/[A-Za-z_$]/.test(char)) {
      const match = /^[A-Za-z_$][\w$]*/.exec(rest)
      const word = match?.[0] ?? char
      if (spec.keywords.has(word)) {
        out.push(`<span class="tok-keyword">${escapeHtml(word)}</span>`)
      } else {
        pushText(word)
      }
      index += word.length
      continue
    }

    pushText(char)
    index += 1
  }

  return { html: out.join(''), open: block }
}

/**
 * 逐行高亮：返回每行一段 HTML，行数与原文本一致（供带行号的预览用）。
 *
 * 逐行做是因为界面按行渲染、要能对上行号；块注释的状态跨行传递，
 * 所以这里显式记住「上一行结束时还在注释里」。
 */
export function highlightLines(source: string, lang: LangFamily): string[] {
  const lines = source.split('\n')
  if (lang === 'json') return lines.map(line => highlightJson(line))
  const spec = SPECS[lang]
  if (!spec) return lines.map(escapeHtml)

  let block = false
  return lines.map(line => {
    const scanned = scanLine(line, spec, block)
    block = scanned.open
    return scanned.html
  })
}

/** markdown 代码块的语言标记（```ts / ```bash…）→ 语言家族。 */
export function familyOfLangName(lang: string): LangFamily {
  switch (lang.trim().toLowerCase()) {
    case 'json':
    case 'jsonc':
      return 'json'
    case 'js':
    case 'javascript':
    case 'jsx':
    case 'ts':
    case 'typescript':
    case 'tsx':
    case 'vue':
      return 'js'
    case 'rust':
    case 'rs':
      return 'rust'
    case 'python':
    case 'py':
      return 'python'
    case 'go':
      return 'go'
    case 'sh':
    case 'bash':
    case 'shell':
    case 'zsh':
    case 'console':
      return 'shell'
    case 'css':
    case 'scss':
    case 'less':
      return 'css'
    default:
      return 'text'
  }
}

/** 整段高亮（markdown 代码块用）。 */
export function highlightCode(source: string, lang: string): string {
  return highlightLines(source, familyOfLangName(lang)).join('\n')
}
