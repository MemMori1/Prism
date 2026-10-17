import { describe, expect, it } from 'vitest'
import { parseSnippet } from '../src/search'

describe('parseSnippet', () => {
  it('拆出高亮段', () => {
    expect(parseSnippet('前置\u0001命中\u0002后置')).toEqual([
      { text: '前置', hit: false },
      { text: '命中', hit: true },
      { text: '后置', hit: false },
    ])
  })

  it('多段高亮', () => {
    expect(parseSnippet('\u0001a\u0002中\u0001b\u0002')).toEqual([
      { text: 'a', hit: true },
      { text: '中', hit: false },
      { text: 'b', hit: true },
    ])
  })

  it('无标记时原样单段；空串无段', () => {
    expect(parseSnippet('普通文本')).toEqual([{ text: '普通文本', hit: false }])
    expect(parseSnippet('')).toEqual([])
  })

  it('容忍不闭合标记', () => {
    expect(parseSnippet('尾部\u0001开着')).toEqual([
      { text: '尾部', hit: false },
      { text: '开着', hit: true },
    ])
  })
})
