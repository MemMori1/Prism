import { describe, expect, it } from 'vitest'
import { engineBadge, engineStatus } from '../src/engineStatus'
import type { EngineHealth } from '../src/types'

const base: EngineHealth = {
  engineId: 'opencode',
  displayName: 'OpenCode',
  databasePath: '/tmp/opencode.db',
  cliPath: '/usr/local/bin/opencode',
  ready: true,
}

describe('engineStatus', () => {
  it('CLI 与数据库都可用时为已就绪', () => {
    expect(engineStatus(base)).toEqual({ label: '已就绪', tone: 'ok' })
  })

  it('只有数据库时为只读', () => {
    expect(engineStatus({ ...base, cliPath: null, ready: false })).toEqual({
      label: '只读（未找到 CLI）',
      tone: 'warn',
    })
  })

  it('数据库也没有时为未安装', () => {
    expect(engineStatus({ ...base, databasePath: null, cliPath: null, ready: false })).toEqual({
      label: '未安装',
      tone: 'error',
    })
  })
})

describe('engineBadge', () => {
  it('使用固定的两字缩写', () => {
    expect(engineBadge('opencode')).toBe('OC')
    expect(engineBadge('codex')).toBe('CX')
    expect(engineBadge('claude')).toBe('CC')
  })

  it('未知引擎回退到前两个字母', () => {
    expect(engineBadge('gemini')).toBe('GE')
  })
})
