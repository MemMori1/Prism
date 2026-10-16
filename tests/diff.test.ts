import { describe, expect, it } from 'vitest'
import { parsePatch, patchStats } from '../src/diff'

const PATCH = [
  'Index: src/app.ts',
  '===================================================================',
  '--- src/app.ts',
  '+++ src/app.ts',
  '@@ -1,4 +1,5 @@',
  ' const a = 1',
  '-const b = 2',
  '+const b = 3',
  '+const c = 4',
  ' export {}',
].join('\n')

describe('parsePatch', () => {
  it('解析 hunk / 上下文 / 新增 / 删除', () => {
    const rows = parsePatch(PATCH)
    expect(rows.map(row => row.type)).toEqual([
      'hunk',
      'context',
      'del',
      'add',
      'add',
      'context',
    ])
    expect(rows[1].text).toBe('const a = 1')
    expect(rows[2].text).toBe('const b = 2')
    expect(rows[3].text).toBe('const b = 3')
  })

  it('跳过 @@ 之前的全部头部行', () => {
    const rows = parsePatch('Index: x\n--- a\n+++ b\n@@ -1 +1 @@\n-a\n+b')
    expect(rows).toHaveLength(3)
    expect(rows[0].type).toBe('hunk')
  })

  it('hunk 内空行按上下文处理', () => {
    const rows = parsePatch('@@ -1,3 +1,3 @@\n line1\n\n line3')
    expect(rows.map(row => row.type)).toEqual(['hunk', 'context', 'context', 'context'])
    expect(rows[2].text).toBe('')
  })

  it('跳过 "No newline at end of file" 行', () => {
    const rows = parsePatch('@@ -1 +1 @@\n-a\n+b\n\\ No newline at end of file')
    expect(rows).toHaveLength(3)
  })

  it('多个 hunk 都保留', () => {
    const rows = parsePatch('@@ -1 +1 @@\n-a\n+b\n@@ -10 +10 @@\n-c\n+d')
    expect(rows.filter(row => row.type === 'hunk')).toHaveLength(2)
  })

  it('空补丁返回空数组', () => {
    expect(parsePatch('')).toEqual([])
  })
})

describe('patchStats', () => {
  it('统计新增与删除行数', () => {
    const stats = patchStats(parsePatch(PATCH))
    expect(stats).toEqual({ added: 2, removed: 1 })
  })

  it('无增删时为零', () => {
    expect(patchStats(parsePatch('@@ -1 +1 @@\n same'))).toEqual({ added: 0, removed: 0 })
  })
})
