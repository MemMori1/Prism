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
  it('按 Claude、Codex、OpenCode 排列并保留卡片文案', () => {
    const choices = engineChoices([health('opencode', true), health('codex', true), health('claude', true)])
    expect(choices.map(item => item.id)).toEqual(['claude', 'codex', 'opencode'])
    expect(choices.map(item => item.label)).toEqual(['Claude Code', 'Codex', 'OpenCode'])
    expect(choices[2].description).toBe('使用 OpenCode 开始一个空白会话')
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
