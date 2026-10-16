import { describe, expect, it } from 'vitest'
import { renderMarkdown } from '../src/markdown'

describe('renderMarkdown', () => {
  it('渲染标题与列表', () => {
    const html = renderMarkdown('# 标题\n\n- 一\n- 二')
    expect(html).toContain('<h1')
    expect(html).toContain('<li>')
  })

  it('转义 HTML，不允许注入', () => {
    const html = renderMarkdown('<script>alert(1)</script>')
    expect(html).not.toContain('<script')
    expect(html).toContain('&lt;script')
  })

  it('代码块内容按原文转义展示', () => {
    const html = renderMarkdown('```\n<div>hello</div>\n```')
    expect(html).toContain('<pre>')
    expect(html).toContain('&lt;div&gt;hello&lt;/div&gt;')
  })

  it('javascript: 链接被中和', () => {
    const html = renderMarkdown('[点我](javascript:alert(1))')
    expect(html).not.toContain('javascript:')
    expect(html).toContain('href="#"')
  })

  it('data: 图片源被中和', () => {
    const html = renderMarkdown('![x](data:image/svg+xml;base64,AAA)')
    expect(html).not.toContain('data:')
  })

  it('正常的 http 链接保留', () => {
    const html = renderMarkdown('[官网](https://example.com/a?b=1)')
    expect(html).toContain('https://example.com/a?b=1')
  })

  it('blockquote 正常渲染（> 未被转义）', () => {
    const html = renderMarkdown('> 引用内容')
    expect(html).toContain('<blockquote')
  })

  it('单换行渲染为换行（breaks）', () => {
    const html = renderMarkdown('第一行\n第二行')
    expect(html).toContain('<br')
  })

  it('重复渲染命中缓存且结果一致', () => {
    const source = '# 缓存\n\n内容'
    expect(renderMarkdown(source)).toBe(renderMarkdown(source))
  })
})
