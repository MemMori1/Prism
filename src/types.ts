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
  id: string
  worktree: string
  name: string
  sessionCount: number
  timeUpdated: number
}

export interface SessionSummary {
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
  | { kind: 'tool'; name: string; status: string | null; summary: string | null }

export type RecordRole = 'user' | 'assistant' | 'system'

export interface TimelineRecord {
  id: string
  role: RecordRole
  timeCreated: number
  model: string | null
  completed: boolean
  parts: TimelinePart[]
}
