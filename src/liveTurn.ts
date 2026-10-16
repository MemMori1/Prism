/**
 * 运行轴的实时状态：把 SSE 事件流归约成"进行中的一条助手消息"。
 *
 * 设计：纯函数（便于单测），delta 级增量更新；DB 始终是最终事实来源，
 * turn 结束后由会话视图从 DB 刷新定稿。
 */

export interface LiveTextPart {
  kind: 'text'
  key: string
  text: string
}

export interface LiveReasoningPart {
  kind: 'reasoning'
  key: string
  text: string
}

export interface LiveToolPart {
  kind: 'tool'
  key: string
  callId: string
  name: string
  status: 'running' | 'completed' | 'error'
  summary: string | null
  input: unknown | null
  output: string | null
  error: string | null
  metadata: unknown | null
}

export type LivePart = LiveTextPart | LiveReasoningPart | LiveToolPart

export interface LiveTurn {
  running: boolean
  interrupted: boolean
  error: string | null
  parts: LivePart[]
}

export interface EngineEventLike {
  type: string
  data: Record<string, unknown> | null
}

export function createLiveTurn(): LiveTurn {
  return { running: false, interrupted: false, error: null, parts: [] }
}

function text(value: unknown): string | null {
  return typeof value === 'string' ? value : null
}

function errorText(value: unknown): string | null {
  if (typeof value === 'string' && value.trim()) return value
  if (value && typeof value === 'object') {
    const message = (value as Record<string, unknown>).message
    if (typeof message === 'string' && message.trim()) return message
  }
  return null
}

/** 与后端 summarize_tool_input 对齐的输入摘要。 */
export function summarizeToolInput(input: unknown): string | null {
  if (!input || typeof input !== 'object') return null
  const record = input as Record<string, unknown>
  const keys = ['command', 'filePath', 'path', 'pattern', 'query', 'url', 'description', 'task']
  for (const key of keys) {
    const value = record[key]
    if (typeof value === 'string' && value.trim()) {
      const first = value.trim().split('\n')[0]
      return first.length > 96 ? `${first.slice(0, 96)}…` : first
    }
  }
  return null
}

function joinContentTexts(content: unknown): string | null {
  if (!Array.isArray(content)) return null
  const texts = content
    .map(part =>
      part && typeof part === 'object' ? (part as Record<string, unknown>).text : null,
    )
    .filter((value): value is string => typeof value === 'string')
  return texts.length ? texts.join('\n') : null
}

function appendText(
  state: LiveTurn,
  kind: 'text' | 'reasoning',
  key: string,
  delta: string,
): LiveTurn {
  const index = state.parts.findIndex(item => item.key === key)
  if (index === -1) {
    const part: LiveTextPart | LiveReasoningPart = { kind, key, text: delta }
    return { ...state, parts: [...state.parts, part] }
  }
  const target = state.parts[index]
  if (target.kind !== kind) return state
  const parts = [...state.parts]
  parts[index] = { ...target, text: target.text + delta }
  return { ...state, parts }
}

function setText(
  state: LiveTurn,
  kind: 'text' | 'reasoning',
  key: string,
  full: string,
): LiveTurn {
  const index = state.parts.findIndex(item => item.key === key)
  if (index === -1) {
    const part: LiveTextPart | LiveReasoningPart = { kind, key, text: full }
    return { ...state, parts: [...state.parts, part] }
  }
  const target = state.parts[index]
  if (target.kind !== kind) return state
  const parts = [...state.parts]
  parts[index] = { ...target, text: full }
  return { ...state, parts }
}

function upsertTool(
  state: LiveTurn,
  key: string,
  patch: Partial<Omit<LiveToolPart, 'kind' | 'key'>>,
): LiveTurn {
  const index = state.parts.findIndex(item => item.key === key)
  if (index === -1) {
    const created: LiveToolPart = {
      kind: 'tool',
      key,
      callId: patch.callId ?? key,
      name: patch.name ?? 'tool',
      status: patch.status ?? 'running',
      summary: patch.summary ?? null,
      input: patch.input ?? null,
      output: patch.output ?? null,
      error: patch.error ?? null,
      metadata: patch.metadata ?? null,
    }
    return { ...state, parts: [...state.parts, created] }
  }
  const target = state.parts[index]
  if (target.kind !== 'tool') return state
  const parts = [...state.parts]
  parts[index] = { ...target, ...patch }
  return { ...state, parts }
}

/** 事件 → 新状态（无变化时原样返回，便于跳过无谓渲染）。 */
export function reduceLiveTurn(state: LiveTurn, event: EngineEventLike): LiveTurn {
  const data = event.data ?? {}
  const assistantMessageID = text(data.assistantMessageID) ?? 'unknown'
  const ordinal = typeof data.ordinal === 'number' ? data.ordinal : 0
  const textKey = (kind: 'text' | 'reasoning') => `${kind}:${assistantMessageID}:${ordinal}`

  switch (event.type) {
    case 'session.execution.started':
      return { running: true, interrupted: false, error: null, parts: [] }

    case 'session.execution.succeeded':
      return state.running ? { ...state, running: false } : state

    case 'session.execution.interrupted':
      return { ...state, running: false, interrupted: true }

    case 'session.execution.failed':
      return { ...state, running: false, error: errorText(data.error) ?? '执行失败' }

    case 'session.step.failed':
      return { ...state, error: errorText(data.error) ?? '步骤失败' }

    case 'session.text.delta': {
      const delta = text(data.delta)
      return delta ? appendText(state, 'text', textKey('text'), delta) : state
    }

    case 'session.text.ended': {
      const full = text(data.text)
      return full !== null ? setText(state, 'text', textKey('text'), full) : state
    }

    case 'session.reasoning.delta': {
      const delta = text(data.delta)
      return delta ? appendText(state, 'reasoning', textKey('reasoning'), delta) : state
    }

    case 'session.reasoning.ended': {
      const full = text(data.text)
      return full !== null ? setText(state, 'reasoning', textKey('reasoning'), full) : state
    }

    case 'session.tool.input.started': {
      const callId = text(data.id)
      if (!callId) return state
      return upsertTool(state, `tool:${callId}`, {
        callId,
        name: text(data.name) ?? 'tool',
        status: 'running',
      })
    }

    case 'session.tool.called': {
      const callId = text(data.id)
      if (!callId) return state
      const input = data.input ?? null
      return upsertTool(state, `tool:${callId}`, {
        callId,
        status: 'running',
        input,
        summary: summarizeToolInput(input),
      })
    }

    case 'session.tool.success': {
      const callId = text(data.id)
      if (!callId) return state
      return upsertTool(state, `tool:${callId}`, {
        callId,
        status: 'completed',
        output: joinContentTexts(data.content),
        metadata: data.metadata ?? null,
      })
    }

    case 'session.tool.progress': {
      const callId = text(data.id)
      if (!callId) return state
      return upsertTool(state, `tool:${callId}`, {
        callId,
        metadata: data.metadata ?? null,
      })
    }

    default:
      return state
  }
}
