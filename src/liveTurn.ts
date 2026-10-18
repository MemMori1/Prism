/**
 * 运行轴的实时状态：把 SSE 事件流归约成"进行中的一条助手消息"。
 *
 * 设计：纯函数（便于单测），delta 级增量更新；DB 始终是最终事实来源，
 * turn 结束后由会话视图从 DB 刷新定稿。
 */

export interface LiveTextPart {
  kind: 'text'
  key: string
  /** 产出该部分的助手消息 id（用于与 DB 记录去重）。 */
  messageId: string
  text: string
}

export interface LiveReasoningPart {
  kind: 'reasoning'
  key: string
  messageId: string
  text: string
}

export interface LiveToolPart {
  kind: 'tool'
  key: string
  messageId: string
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
  messageId: string,
  delta: string,
): LiveTurn {
  const index = state.parts.findIndex(item => item.key === key)
  if (index === -1) {
    const part: LiveTextPart | LiveReasoningPart = { kind, key, messageId, text: delta }
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
  messageId: string,
  full: string,
): LiveTurn {
  const index = state.parts.findIndex(item => item.key === key)
  if (index === -1) {
    const part: LiveTextPart | LiveReasoningPart = { kind, key, messageId, text: full }
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
  patch: Partial<Omit<LiveToolPart, 'kind' | 'key' | 'messageId'>> & { messageId?: string },
): LiveTurn {
  const index = state.parts.findIndex(item => item.key === key)
  if (index === -1) {
    const created: LiveToolPart = {
      kind: 'tool',
      key,
      messageId: patch.messageId ?? 'unknown',
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
      return delta ? appendText(state, 'text', textKey('text'), assistantMessageID, delta) : state
    }

    case 'session.text.ended': {
      const full = text(data.text)
      return full !== null ? setText(state, 'text', textKey('text'), assistantMessageID, full) : state
    }

    case 'session.reasoning.delta': {
      const delta = text(data.delta)
      return delta
        ? appendText(state, 'reasoning', textKey('reasoning'), assistantMessageID, delta)
        : state
    }

    case 'session.reasoning.ended': {
      const full = text(data.text)
      return full !== null
        ? setText(state, 'reasoning', textKey('reasoning'), assistantMessageID, full)
        : state
    }

    case 'session.tool.input.started': {
      const callId = text(data.id)
      if (!callId) return state
      return upsertTool(state, `tool:${callId}`, {
        callId,
        messageId: assistantMessageID,
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
        messageId: assistantMessageID,
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

// ---- 收口：live 块与 DB 记录的去重 ----

/** 收口的时间容差：引擎时间戳与前端时钟可能有小偏差（毫秒）。 */
export const LIVE_TURN_TOLERANCE_MS = 3000

/** 结构上兼容时间线记录（本模块不依赖 types）。 */
export interface TimelineLikeRecord {
  id: string
  role: string
  completed: boolean
  timeCreated: number
}

/** live 块当前代表的范围：画了哪些消息、这一轮从哪一刻开始。 */
export interface LiveCoverage {
  /** live 块正在展示的助手消息 id。 */
  ids: Set<string>
  /** 本轮开跑时刻（execution.started，毫秒）。 */
  startedAt: number | null
  /** live 块是否还占着屏幕（跑着、还有内容或报了错）。 */
  open: boolean
}

/** 取 live 块的代表范围（收口时拿它跟 DB 记录对账）。 */
export function liveCoverage(turn: LiveTurn, startedAt: number | null): LiveCoverage {
  return {
    ids: new Set(
      turn.parts
        .map(part => part.messageId)
        .filter(messageId => messageId && messageId !== 'unknown'),
    ),
    startedAt,
    open: turn.running || turn.parts.length > 0 || !!turn.error,
  }
}

/**
 * 这条助手记录是否已由 live 块代表：同一条消息、落在本轮时间窗内、或还没完结。
 * 是的话收口前别并进时间线——否则同一轮会画两份（live 一份、记录一份）。
 */
export function coveredByLive(record: TimelineLikeRecord, coverage: LiveCoverage): boolean {
  if (!coverage.open) return false
  if (record.role !== 'assistant') return false
  if (!record.completed) return true
  if (coverage.ids.has(record.id)) return true
  return (
    coverage.startedAt !== null && record.timeCreated >= coverage.startedAt - LIVE_TURN_TOLERANCE_MS
  )
}
