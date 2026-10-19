import { beforeEach, describe, expect, it } from 'vitest'
import {
  APPROVAL_CHOICES,
  DEFAULT_APPROVAL,
  approvalChoice,
  approvalIsRestrictive,
  loadApproval,
  parseApproval,
  saveApproval,
} from '../src/approval'

describe('parseApproval', () => {
  it('认三档', () => {
    expect(parseApproval('read-only')).toBe('read-only')
    expect(parseApproval('ask')).toBe('ask')
    expect(parseApproval('full')).toBe('full')
  })

  it('认不出来一律回落到「每次询问」', () => {
    expect(parseApproval('')).toBe(DEFAULT_APPROVAL)
    expect(parseApproval(null)).toBe(DEFAULT_APPROVAL)
    expect(parseApproval('yolo')).toBe(DEFAULT_APPROVAL)
    expect(parseApproval(42)).toBe(DEFAULT_APPROVAL)
  })
})

describe('approvalChoice', () => {
  it('每档都有标签和说明', () => {
    for (const mode of ['read-only', 'ask', 'full'] as const) {
      const choice = approvalChoice(mode)
      expect(choice.id).toBe(mode)
      expect(choice.label.length).toBeGreaterThan(0)
      expect(choice.hint.length).toBeGreaterThan(0)
    }
    expect(APPROVAL_CHOICES).toHaveLength(3)
  })
})

describe('approvalIsRestrictive', () => {
  it('只有完全访问不拦人', () => {
    expect(approvalIsRestrictive('read-only')).toBe(true)
    expect(approvalIsRestrictive('ask')).toBe(true)
    expect(approvalIsRestrictive('full')).toBe(false)
  })
})

/** 测试环境是 node：给个内存版 localStorage，行为与浏览器一致。 */
function memoryStorage(): Storage {
  const map = new Map<string, string>()
  return {
    get length() {
      return map.size
    },
    clear: () => map.clear(),
    getItem: (key: string) => map.get(key) ?? null,
    key: (index: number) => [...map.keys()][index] ?? null,
    removeItem: (key: string) => void map.delete(key),
    setItem: (key: string, value: string) => void map.set(key, String(value)),
  }
}

describe('loadApproval / saveApproval', () => {
  beforeEach(() => {
    ;(globalThis as unknown as { window: { localStorage: Storage } }).window = {
      localStorage: memoryStorage(),
    }
  })

  it('没存过＝默认（每次询问）', () => {
    expect(loadApproval('native:1')).toBe('ask')
  })

  it('记住这个会话的档位', () => {
    saveApproval('native:1', 'full')
    expect(loadApproval('native:1')).toBe('full')
    // 别的会话不受影响，但会落到全局默认
    expect(loadApproval('native:2')).toBe('full')
  })

  it('新会话沿用上次选的档位', () => {
    saveApproval('native:1', 'read-only')
    expect(loadApproval(null)).toBe('read-only')
  })

  it('存档被写坏也不崩，回落到默认', () => {
    window.localStorage.setItem('prism-approval:native:1', '???')
    expect(loadApproval('native:1')).toBe('ask')
  })
})
