import { describe, expect, it } from 'vitest'
import { highlightCode, highlightJson } from '../src/highlight'

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

  it('其他语言只转义、不上色', () => {
    const html = highlightCode('const a = "<b>"', 'ts')
    expect(html).toBe('const a = "&lt;b&gt;"')
    expect(html).not.toContain('tok-')
  })
})
