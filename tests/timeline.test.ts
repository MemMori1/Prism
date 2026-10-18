import { describe, expect, it } from 'vitest'
import { TURN_DURATION_MAX_MS, recordText, turnDurations } from '../src/timeline'
import type { TimelineRecord } from '../src/types'

function record(id: string, timeCreated: number, role = 'assistant'): TimelineRecord {
  return { id, role: role as TimelineRecord['role'], timeCreated, model: null, completed: true, parts: [] }
}

describe('turnDurations（每轮用时＝与上一条记录的时间差）', () => {
  it('单轮对话：助手减用户就是这一轮的用时', () => {
    const durations = turnDurations([
      record('u1', 1_000, 'user'),
      record('a1', 4_500),
    ])
    expect(durations.get('a1')).toBe(3_500)
    // 第一条没有参照，不给。
    expect(durations.has('u1')).toBe(false)
  })

  it('多轮带工具：每段各算各的间隔', () => {
    const durations = turnDurations([
      record('u1', 1_000, 'user'),
      record('a1', 2_000),
      record('a2', 3_500),
    ])
    expect(durations.get('a1')).toBe(1_000)
    expect(durations.get('a2')).toBe(1_500)
  })

  it('缺时间戳 / 时间倒流：宁可不显示，也不给假数', () => {
    const durations = turnDurations([
      record('u1', 0, 'user'),
      record('a1', 5_000),
      record('a2', 4_000),
    ])
    expect(durations.has('a1')).toBe(false)
    expect(durations.has('a2')).toBe(false)
  })

  it('超过上限（中间挂机）不算这一轮的用时', () => {
    const durations = turnDurations([
      record('u1', 1_000, 'user'),
      record('a1', 1_000 + TURN_DURATION_MAX_MS + 1),
      record('a2', 1_000 + TURN_DURATION_MAX_MS + 1 + 2_000),
    ])
    expect(durations.has('a1')).toBe(false)
    expect(durations.get('a2')).toBe(2_000)
  })
})

describe('recordText（复制按钮的内容）', () => {
  it('只要正文，不带思考过程与工具调用', () => {
    const message: TimelineRecord = {
      id: 'a1',
      role: 'assistant',
      timeCreated: 1,
      model: null,
      completed: true,
      parts: [
        { kind: 'reasoning', text: '先想一下' },
        { kind: 'text', text: '第一段' },
        { kind: 'tool', name: 'shell', status: 'completed', summary: null, callId: 'c1' },
        { kind: 'text', text: '  第二段  ' },
      ],
    }
    expect(recordText(message)).toBe('第一段\n\n第二段')
  })

  it('没有正文（纯工具调用）给空串，界面据此不画复制按钮', () => {
    const message: TimelineRecord = {
      id: 'a1',
      role: 'assistant',
      timeCreated: 1,
      model: null,
      completed: true,
      parts: [{ kind: 'tool', name: 'shell', status: 'completed', summary: null, callId: 'c1' }],
    }
    expect(recordText(message)).toBe('')
  })
})
