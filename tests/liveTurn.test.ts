import { describe, expect, it } from 'vitest'
import {
  createLiveTurn,
  reduceLiveTurn,
  summarizeToolInput,
  type LivePart,
  type LiveToolPart,
} from '../src/liveTurn'

const data = (extra: Record<string, unknown> = {}) => ({
  sessionID: 'ses_1',
  assistantMessageID: 'msg_1',
  ordinal: 0,
  ...extra,
})

function run(events: Array<{ type: string; data: Record<string, unknown> }>) {
  let state = createLiveTurn()
  for (const event of events) {
    state = reduceLiveTurn(state, event)
  }
  return state
}

describe('summarizeToolInput', () => {
  it('优先取 command', () => {
    expect(summarizeToolInput({ command: 'ls -la', path: '/tmp' })).toBe('ls -la')
  })

  it('支持 path / pattern / query 等常见字段', () => {
    expect(summarizeToolInput({ filePath: 'src/a.ts' })).toBe('src/a.ts')
    expect(summarizeToolInput({ pattern: 'TODO' })).toBe('TODO')
    expect(summarizeToolInput({ query: 'docs' })).toBe('docs')
  })

  it('只取第一行并截断过长内容', () => {
    expect(summarizeToolInput({ command: 'echo a\necho b' })).toBe('echo a')
    const long = summarizeToolInput({ command: 'x'.repeat(200) })
    expect(long?.endsWith('…')).toBe(true)
    expect(long?.length).toBe(97)
  })

  it('非对象返回 null', () => {
    expect(summarizeToolInput(null)).toBeNull()
    expect(summarizeToolInput('command')).toBeNull()
    expect(summarizeToolInput({})).toBeNull()
  })
})

describe('reduceLiveTurn', () => {
  it('完整一轮：思考 → 正文 → 工具 → 收口', () => {
    const state = run([
      { type: 'session.execution.started', data: data() },
      { type: 'session.step.started', data: data() },
      { type: 'session.reasoning.started', data: data() },
      { type: 'session.reasoning.delta', data: data({ delta: '先看' }) },
      { type: 'session.reasoning.delta', data: data({ delta: '文件。' }) },
      { type: 'session.reasoning.ended', data: data({ text: '先看文件。' }) },
      { type: 'session.text.started', data: data() },
      { type: 'session.text.delta', data: data({ delta: '好的' }) },
      { type: 'session.text.delta', data: data({ delta: '，我看下' }) },
      { type: 'session.text.ended', data: data({ text: '好的，我看下' }) },
      { type: 'session.tool.input.started', data: data({ id: 'call_1', name: 'read' }) },
      {
        type: 'session.tool.called',
        data: data({ id: 'call_1', input: { path: 'src/a.ts' } }),
      },
      {
        type: 'session.tool.success',
        data: data({
          id: 'call_1',
          content: [{ type: 'text', text: '内容' }],
          metadata: { truncated: false },
        }),
      },
      { type: 'session.execution.succeeded', data: data() },
    ])

    expect(state.running).toBe(false)
    expect(state.interrupted).toBe(false)
    expect(state.parts.map(part => part.kind)).toEqual(['reasoning', 'text', 'tool'])

    const reasoning = state.parts[0]
    if (reasoning.kind !== 'reasoning') throw new Error('应为 reasoning')
    expect(reasoning.text).toBe('先看文件。')

    const text = state.parts[1]
    if (text.kind !== 'text') throw new Error('应为 text')
    expect(text.text).toBe('好的，我看下')

    const tool = state.parts[2] as LiveToolPart
    expect(tool.name).toBe('read')
    expect(tool.status).toBe('completed')
    expect(tool.summary).toBe('src/a.ts')
    expect(tool.output).toBe('内容')
  })

  it('没有 started 事件时 delta 也能自建 part', () => {
    const state = run([
      { type: 'session.execution.started', data: data() },
      { type: 'session.text.delta', data: data({ delta: '流式' }) },
    ])
    const part: LivePart = state.parts[0]
    if (part.kind !== 'text') throw new Error('应为 text')
    expect(part.text).toBe('流式')
  })

  it('execution.started 清空上一轮 parts', () => {
    const state = run([
      { type: 'session.text.delta', data: data({ delta: '旧' }) },
      { type: 'session.execution.started', data: data() },
    ])
    expect(state.running).toBe(true)
    expect(state.parts).toEqual([])
  })

  it('中断：running 结束并标记 interrupted', () => {
    const state = run([
      { type: 'session.execution.started', data: data() },
      { type: 'session.text.delta', data: data({ delta: '半截' }) },
      { type: 'session.execution.interrupted', data: data({ reason: 'user' }) },
    ])
    expect(state.running).toBe(false)
    expect(state.interrupted).toBe(true)
  })

  it('失败：取出错误信息', () => {
    const state = run([
      { type: 'session.execution.started', data: data() },
      {
        type: 'session.execution.failed',
        data: data({ error: { type: 'x', message: '模型超时' } }),
      },
    ])
    expect(state.running).toBe(false)
    expect(state.error).toBe('模型超时')
  })

  it('无关事件原样返回（引用不变）', () => {
    const state = createLiveTurn()
    expect(reduceLiveTurn(state, { type: 'session.usage.updated', data: data() })).toBe(state)
    expect(reduceLiveTurn(state, { type: 'server.connected', data: null })).toBe(state)
  })

  it('多个工具按出现顺序排列，互不干扰', () => {
    const state = run([
      { type: 'session.execution.started', data: data() },
      { type: 'session.tool.input.started', data: data({ id: 'call_a', name: 'shell' }) },
      { type: 'session.tool.input.started', data: data({ id: 'call_b', name: 'grep' }) },
      {
        type: 'session.tool.called',
        data: data({ id: 'call_b', input: { pattern: 'x' } }),
      },
      { type: 'session.tool.success', data: data({ id: 'call_a', content: [] }) },
    ])
    const tools = state.parts.filter((part): part is LiveToolPart => part.kind === 'tool')
    expect(tools.map(tool => tool.callId)).toEqual(['call_a', 'call_b'])
    expect(tools[0].status).toBe('completed')
    expect(tools[1].status).toBe('running')
    expect(tools[1].summary).toBe('x')
  })
})
