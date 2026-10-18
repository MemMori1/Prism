import { describe, expect, it } from 'vitest'
import {
  ALL_ENGINES,
  engineBadge,
  filterByEngine,
  projectEngineOptions,
} from '../src/projectFilter'

const projects = [
  { id: 'a', engine: 'opencode' },
  { id: 'b', engine: 'codex' },
  { id: 'c', engine: 'native' },
  { id: 'd', engine: 'opencode' },
]

describe('projectEngineOptions', () => {
  it('总是先给「全部类型」', () => {
    expect(projectEngineOptions([])).toEqual([{ value: ALL_ENGINES, label: '全部类型' }])
  })

  it('只列出现过的类型，顺序固定（原生在前）', () => {
    expect(projectEngineOptions(projects).map(option => option.value)).toEqual([
      ALL_ENGINES,
      'native',
      'opencode',
      'codex',
    ])
  })

  it('没登记的新引擎排最后，用原 id 兜底', () => {
    const options = projectEngineOptions([{ engine: 'gemini' }, { engine: 'codex' }])
    expect(options.map(option => option.value)).toEqual([ALL_ENGINES, 'codex', 'gemini'])
    expect(options.at(-1)?.label).toBe('gemini')
  })
})

describe('filterByEngine', () => {
  it('全部时原样返回', () => {
    expect(filterByEngine(projects, ALL_ENGINES)).toHaveLength(4)
    expect(filterByEngine(projects, '')).toHaveLength(4)
  })

  it('按类型只留下对应项目', () => {
    expect(filterByEngine(projects, 'opencode').map(item => item.id)).toEqual(['a', 'd'])
    expect(filterByEngine(projects, 'native').map(item => item.id)).toEqual(['c'])
  })

  it('没有这类项目时给空数组', () => {
    expect(filterByEngine(projects, 'claude')).toEqual([])
  })

  it('不改动原数组', () => {
    const source = [...projects]
    filterByEngine(projects, 'codex')
    expect(projects).toEqual(source)
  })
})

describe('engineBadge', () => {
  it('已知引擎用两字缩写', () => {
    expect(engineBadge('native')).toBe('PR')
    expect(engineBadge('opencode')).toBe('OC')
    expect(engineBadge('codex')).toBe('CX')
    expect(engineBadge('claude')).toBe('CC')
  })

  it('未知引擎取前两个字母大写', () => {
    expect(engineBadge('gemini')).toBe('GE')
    expect(engineBadge('x')).toBe('X')
  })
})
