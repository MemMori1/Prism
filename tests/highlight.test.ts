import { describe, expect, it } from 'vitest'
import { highlightCode, highlightJson, highlightLines, languageOfPath } from '../src/highlight'

describe('highlightJson', () => {
  it('键、字符串、数字、字面量分别上色', () => {
    const html = highlightJson('{"name":"prism","count":3,"ok":true,"none":null}')
    expect(html).toContain('<span class="tok-key">"name"</span>')
    expect(html).toContain('<span class="tok-string">"prism"</span>')
    expect(html).toContain('<span class="tok-number">3</span>')
    expect(html).toContain('<span class="tok-literal">true</span>')
    expect(html).toContain('<span class="tok-literal">null</span>')
  })

  it('键判定看后面是不是冒号：值里带冒号的 URL 不算键', () => {
    const html = highlightJson('{"home":"https://example.com/a:b"}')
    expect(html).toContain('<span class="tok-key">"home"</span>')
    expect(html).toContain('<span class="tok-string">"https://example.com/a:b"</span>')
  })

  it('负数、小数、指数都算数字', () => {
    const html = highlightJson('[-1, 0.5, 1200, 1.2e-3]')
    expect(html).toContain('<span class="tok-number">-1</span>')
    expect(html).toContain('<span class="tok-number">0.5</span>')
    expect(html).toContain('<span class="tok-number">1200</span>')
    expect(html).toContain('<span class="tok-number">1.2e-3</span>')
  })

  it('字符串里的 true / 数字不被拆开', () => {
    const html = highlightJson('{"a":"true 42"}')
    expect(html).toContain('<span class="tok-string">"true 42"</span>')
    expect(html).not.toContain('tok-literal')
  })

  it('HTML 一律转义，不产生可执行标签', () => {
    const html = highlightJson('{"x":"<script>alert(1)</script>"}')
    expect(html).not.toContain('<script')
    expect(html).toContain('&lt;script&gt;')
  })

  it('缩进与换行原样保留', () => {
    const html = highlightJson('{\n  "a": 1\n}')
    expect(html).toContain('\n  ')
  })

  it('空字符串与纯标点不炸', () => {
    expect(highlightJson('')).toBe('')
    expect(highlightJson('{}[],:')).toBe('{}[],:')
  })
})

describe('highlightCode', () => {
  it('json 走高亮（大小写由调用方归一）', () => {
    expect(highlightCode('{"a":1}', 'json')).toContain('tok-key')
  })

  it('其他语言按关键字上色，且照旧转义', () => {
    const html = highlightCode('const a = "<b>"', 'ts')
    expect(html).toContain('tok-keyword')
    expect(html).toContain('&lt;b&gt;')
    expect(html).not.toContain('<b>')
  })

  it('不认识的语言只转义', () => {
    expect(highlightCode('const a = "<b>"', 'brainfuck')).toBe('const a = "&lt;b&gt;"')
  })
})

describe('highlightLines', () => {
  it('ts：关键字 / 字符串 / 注释 / 数字', () => {
    const [line] = highlightLines('const a = "hi" // 注释 42', 'js')
    expect(line).toContain('tok-keyword">const')
    expect(line).toContain('tok-string">"hi"')
    expect(line).toContain('tok-comment">// 注释 42')
  })

  it('块注释跨行传递', () => {
    const lines = highlightLines('/* 开头\n  中间\n*/\nconst y = 1', 'js')
    expect(lines[0]).toContain('tok-comment')
    expect(lines[1]).toContain('tok-comment')
    expect(lines[2]).toContain('tok-comment')
    expect(lines[3]).toContain('tok-keyword')
  })

  it('字符串里的关键字不上色', () => {
    const [line] = highlightLines('"const"', 'js')
    expect(line).toContain('tok-string')
    expect(line).not.toContain('tok-keyword')
  })

  it('python 的 # 注释与 go 的 func', () => {
    expect(highlightLines('# 说明', 'python')[0]).toContain('tok-comment')
    expect(highlightLines('func main() {}', 'go')[0]).toContain('tok-keyword')
  })

  it('text 一律转义、不上色', () => {
    expect(highlightLines('<div>', 'text')[0]).toBe('&lt;div&gt;')
  })

  it('行数与原文一致（含末尾空行）', () => {
    expect(highlightLines('a\nb\n', 'text')).toHaveLength(3)
  })
})

describe('languageOfPath', () => {
  it('按扩展名归类', () => {
    expect(languageOfPath('src/a.ts')).toBe('js')
    expect(languageOfPath('src/a.vue')).toBe('js')
    expect(languageOfPath('src/a.rs')).toBe('rust')
    expect(languageOfPath('a.py')).toBe('python')
    expect(languageOfPath('a.sh')).toBe('shell')
    expect(languageOfPath('a.css')).toBe('css')
    expect(languageOfPath('a.json')).toBe('json')
    expect(languageOfPath('README.md')).toBe('text')
    expect(languageOfPath(null)).toBe('text')
  })
})
