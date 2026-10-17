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

  it('行内公式渲染为 KaTeX', () => {
    const html = renderMarkdown('当 $E=mc^2$ 成立')
    expect(html).toContain('katex')
  })

  it('块级公式渲染为 display 模式', () => {
    const html = renderMarkdown('$$\n\\frac{a}{b}\n$$')
    expect(html).toContain('katex-display')
  })

  it('美元金额不误判为公式', () => {
    const html = renderMarkdown('价格是 $5 和 $10')
    expect(html).not.toContain('katex')
  })

  it('反斜杠括号公式可渲染', () => {
    const html = renderMarkdown('设 \\(x^2\\) 为平方')
    expect(html).toContain('katex')
  })

  it('非法公式不抛错（throwOnError 关闭）', () => {
    expect(() => renderMarkdown('$\\frac{1}{$')).not.toThrow()
  })

  it('html 代码块渲染为禁脚本沙箱预览', () => {
    const html = renderMarkdown('```html\n<h1>hi</h1>\n```')
    expect(html).toContain('md-embed-frame')
    expect(html).toContain('sandbox=')
    expect(html).not.toContain('<h1>')
    expect(html).toContain('&lt;h1&gt;')
  })

  it('svg 代码块渲染为预览', () => {
    const html = renderMarkdown('```svg\n<svg xmlns="http://www.w3.org/2000/svg"><circle r="8"/></svg>\n```')
    expect(html).toContain('md-embed-svg')
    expect(html).not.toContain('<circle r="8"/>')
  })

  it('本地 HTML 链接变成交付物卡片', () => {
    const html = renderMarkdown('[对比卡](/tmp/card.html)')
    expect(html).toContain('md-artifact')
    expect(html).toContain('data-artifact-kind="html"')
    expect(html).toContain('/tmp/card.html')
  })

  it('本地图片变成交付物卡片', () => {
    const html = renderMarkdown('![图](/tmp/shot.png)')
    expect(html).toContain('data-artifact-kind="image"')
    expect(html).toContain('md-artifact-name')
  })

  it('普通本地文本链接不变卡片', () => {
    const html = renderMarkdown('[说明](/tmp/readme.md)')
    expect(html).not.toContain('md-artifact')
  })

  it('远程图片不转卡片且保留', () => {
    const html = renderMarkdown('![x](https://a.com/x.png)')
    expect(html).not.toContain('md-artifact')
    expect(html).toContain('https://a.com/x.png')
  })
})
