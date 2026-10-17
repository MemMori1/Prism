import { describe, expect, it } from 'vitest'
import { commandLine, groupAssets, groupMcp, kindLabel, scopeLabel } from '../src/workshop'
import type { AssetItem, McpServer } from '../src/types'

function item(overrides: Partial<AssetItem>): AssetItem {
  return {
    engine: 'opencode',
    kind: 'skill',
    name: 'x',
    description: null,
    path: '/tmp/x.md',
    scope: 'user',
    project: null,
    updatedAt: 0,
    ...overrides,
  }
}

function server(overrides: Partial<McpServer>): McpServer {
  return {
    engine: 'opencode',
    name: 'imessage',
    transport: 'stdio',
    command: '/usr/bin/python3',
    args: ['/x/mcp.py'],
    url: null,
    disabled: false,
    envKeys: [],
    source: '/tmp/opencode.jsonc',
    scope: 'user',
    project: null,
    commandExists: true,
    ...overrides,
  }
}

describe('groupAssets', () => {
  it('按 引擎×种类 分组并保持稳定顺序', () => {
    const groups = groupAssets([
      item({ engine: 'claude', kind: 'memory' }),
      item({ engine: 'codex', kind: 'skill' }),
      item({ engine: 'opencode', kind: 'command' }),
      item({ engine: 'codex', kind: 'skill', name: 'y' }),
    ])
    expect(groups.map((group) => group.key)).toEqual([
      'opencode:command',
      'codex:skill',
      'claude:memory',
    ])
    expect(groups[1].items.map((entry) => entry.name)).toEqual(['x', 'y'])
    expect(groups[1].label).toBe('技能')
  })

  it('未知引擎与种类排在最后', () => {
    const groups = groupAssets([
      item({ engine: 'zzz', kind: 'mystery' as AssetItem['kind'] }),
      item({ engine: 'codex', kind: 'skill' }),
    ])
    expect(groups.map((group) => group.key)).toEqual(['codex:skill', 'zzz:mystery'])
  })
})

describe('scopeLabel', () => {
  it('用户 / 内置 / 项目', () => {
    expect(scopeLabel(item({ scope: 'user' }))).toBe('用户')
    expect(scopeLabel(item({ scope: 'builtin' }))).toBe('内置')
    expect(scopeLabel(item({ scope: 'project', project: '/a/b/项目名' }))).toBe('项目 · 项目名')
    expect(scopeLabel(item({ scope: 'project', project: null }))).toBe('项目')
  })
})

describe('kindLabel', () => {
  it('已知种类出中文，未知原样', () => {
    expect(kindLabel('agent')).toBe('子代理')
    expect(kindLabel('unknown')).toBe('unknown')
  })
})

describe('groupMcp', () => {
  it('按引擎分组并保持引擎序', () => {
    const groups = groupMcp([
      server({ engine: 'codex', name: 'stitch' }),
      server({ engine: 'opencode', name: 'imessage' }),
      server({ engine: 'codex', name: 'node_repl' }),
      server({ engine: 'claude', name: 'figma', transport: 'http', url: 'https://x.test', command: null, args: [] }),
    ])
    expect(groups.map((group) => group.engine)).toEqual(['opencode', 'codex', 'claude'])
    expect(groups[1].items.map((entry) => entry.name)).toEqual(['stitch', 'node_repl'])
    expect(groups[0].label).toBe('OpenCode')
  })
})

describe('commandLine', () => {
  it('拼可执行文件与参数；缺命令返回空串', () => {
    expect(commandLine(server({}))).toBe('/usr/bin/python3 /x/mcp.py')
    expect(commandLine(server({ command: null, args: [] }))).toBe('')
  })
})
