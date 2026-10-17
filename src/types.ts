/** 与 Rust 侧 serde（camelCase）严格对应的前端类型。 */

export interface EngineHealth {
  engineId: string
  displayName: string
  databasePath: string | null
  cliPath: string | null
  /** 数据库与 CLI 都可用 */
  ready: boolean
}

export interface ProjectItem {
  /** 所属引擎 id（opencode / codex …）。 */
  engine: string
  id: string
  worktree: string
  name: string
  sessionCount: number
  timeUpdated: number
}

export interface SessionSummary {
  /** 所属引擎 id（opencode / codex …）。 */
  engine: string
  id: string
  projectId: string
  title: string | null
  directory: string
  model: string | null
  cost: number
  tokensInput: number
  tokensOutput: number
  tokensReasoning: number
  timeCreated: number
  timeUpdated: number
}

export type TimelinePart =
  | { kind: 'text'; text: string }
  | { kind: 'reasoning'; text: string }
  | {
      kind: 'tool'
      name: string
      status: string | null
      summary: string | null
      callId: string
    }

export type RecordRole = 'user' | 'assistant' | 'system'

export interface TimelineRecord {
  id: string
  role: RecordRole
  timeCreated: number
  model: string | null
  completed: boolean
  parts: TimelinePart[]
}

export interface TimelinePage {
  records: TimelineRecord[]
  nextCursor: number | null
  hasMore: boolean
}

export interface ToolDetail {
  input: Record<string, unknown> | null
  output: string | null
  error: string | null
  metadata: Record<string, unknown> | null
  truncated: boolean
}

export interface RuntimeStatus {
  running: boolean
  baseUrl: string | null
  pid: number | null
}

export interface CreatedSession {
  sessionId: string
  directory: string | null
}

/** 挂起的权限请求（引擎 permission.asked 的同形数据）。 */
export interface PermissionRequest {
  id: string
  sessionId: string
  action: string
  resources: string[]
  message: string | null
  sourceMessageId: string | null
  sourceCallId: string | null
}

/** 引擎 SSE 事件（原样透传的信封，取用 type / data）。 */
export interface EngineEvent {
  type: string
  data: Record<string, unknown> | null
}

/** 搜索命中（Prism 自有索引）。 */
export interface SearchHit {
  messageId: string | null
  sessionId: string
  sessionTitle: string | null
  projectId: string
  role: string | null
  seq: number | null
  timeCreated: number
  /** 命中片段；高亮段用 \u0001 / \u0002 包裹。 */
  snippet: string
}

/** 搜索索引状态。 */
export interface SearchStats {
  indexedSessions: number
  indexedMessages: number
  updatedAt: number
}
