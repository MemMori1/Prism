import { describe, expect, it } from 'vitest'
import { engineChoices, newestProject } from '../src/sessionStart'
import type { EngineHealth, ProjectItem } from '../src/types'

function health(engineId: string, ready: boolean): EngineHealth {
  return {
    engineId,
    displayName: engineId,
    databasePath: null,
    cliPath: ready ? `/usr/local/bin/${engineId}` : null,
    ready,
  }
}

function project(id: string, timeUpdated: number): ProjectItem {
  return {
    engine: 'opencode',
    id,
    worktree: `/tmp/${id}`,
    name: id,
    sessionCount: 0,
    timeUpdated,
  }
}

describe('engineChoices', () => {
  it('原生引擎排在最前，其余按 Claude、Codex、DeepSeek Harness、OpenCode 排列', () => {
    const choices = engineChoices([
      health('opencode', true),
      health('codex', true),
      health('dsh', true),
      health('claude', true),
    ])
    expect(choices.map(item => item.id)).toEqual(['native', 'claude', 'codex', 'dsh', 'opencode'])
    expect(choices.map(item => item.label)).toEqual([
      'Prism 原生',
      'Claude Code',
      'Codex',
      'DeepSeek Harness',
      'OpenCode',
    ])
    expect(choices[4].description).toBe('使用 OpenCode 开始一个空白会话')
  })

  it('DeepSeek Harness 装了 CLI 就能从 Prism 直接建会话', () => {
    const ready = engineChoices([health('dsh', true)]).find(item => item.id === 'dsh')
    expect(ready).toMatchObject({ available: true, checking: false })
    const missing = engineChoices([]).find(item => item.id === 'dsh')
    expect(missing).toMatchObject({ available: false, checking: true })
  })

  it('原生引擎不依赖本机 CLI：没配置也可点，只标记待配置', () => {
    const choices = engineChoices([
      { ...health('native', false), cliPath: null },
      health('codex', true),
    ])
    const native = choices.find(item => item.id === 'native')
    expect(native).toMatchObject({ available: true, checking: false, needsSetup: true })
  })

  it('原生引擎配好之后不再提示待配置', () => {
    const choices = engineChoices([health('native', true)])
    expect(choices.find(item => item.id === 'native')).toMatchObject({
      available: true,
      needsSetup: false,
    })
  })

  it('Claude 即使本机就绪也因运行轴未接入而不可用', () => {
    const choices = engineChoices([health('claude', true), health('codex', true), health('opencode', true)])
    expect(choices.find(item => item.id === 'claude')?.available).toBe(false)
    expect(choices.find(item => item.id === 'codex')?.available).toBe(true)
    expect(choices.find(item => item.id === 'opencode')?.available).toBe(true)
  })

  it('健康检查未就绪或缺失时不可用，缺失时标记为检测中', () => {
    const choices = engineChoices([health('opencode', false)])
    expect(choices.find(item => item.id === 'opencode')).toMatchObject({
      available: false,
      checking: false,
    })
    expect(choices.find(item => item.id === 'codex')).toMatchObject({
      available: false,
      checking: true,
    })
    expect(choices.find(item => item.id === 'claude')).toMatchObject({
      available: false,
      checking: true,
    })
  })
})

describe('newestProject', () => {
  it('选最近活跃的项目', () => {
    const projects = [project('old', 10), project('new', 30), project('mid', 20)]
    expect(newestProject(projects)?.id).toBe('new')
  })

  it('没有项目时返回 null', () => {
    expect(newestProject([])).toBeNull()
  })
})
