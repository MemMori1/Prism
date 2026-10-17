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

/** 工坊资产种类。 */
export type AssetKind = 'skill' | 'command' | 'agent' | 'memory' | 'plugin'

/** 工坊资产（技能 / 命令 / 子代理 / 记忆 / 插件，只读浏览）。 */
export interface AssetItem {
  engine: string
  kind: AssetKind
  name: string
  description: string | null
  /** 入口文件路径（技能给 SKILL.md；插件给 plugin.json）。 */
  path: string
  /** 归属：用户级 / 引擎内置 / 项目级。 */
  scope: 'user' | 'builtin' | 'project'
  project: string | null
  updatedAt: number
}

/** 资产入口文件内容（工坊右侧预览）。 */
export interface AssetFile {
  path: string
  content: string
  truncated: boolean
}

/** MCP 服务（三引擎配置真相位置解析，只读）。 */
export interface McpServer {
  engine: string
  name: string
  transport: 'stdio' | 'http'
  /** stdio：可执行文件 */
  command: string | null
  args: string[]
  /** http：远端地址 */
  url: string | null
  disabled: boolean
  /** 环境变量键名（不暴露值） */
  envKeys: string[]
  /** 定义所在配置文件 */
  source: string
  scope: 'user' | 'project'
  project: string | null
  /** stdio：命令是否可解析（绝对路径存在 / PATH 可找到） */
  commandExists: boolean | null
}

/** MCP 探活结果（仅 http GET；任何 HTTP 响应都算可达）。 */
export interface McpProbeResult {
  ok: boolean
  status: number | null
  latencyMs: number
  error: string | null
}

// ---- 定时任务（M11b） ----

/** 任务定义（列表接口附派生字段 nextRun / running）。 */
export interface RoutineDefinition {
  id: string
  name: string
  /** 五字段 cron（分 时 日 月 周）。 */
  schedule: string
  prompt: string
  engine: string
  enabled: boolean
  createdAt: number
  /** 最近一次触发时刻（无论成败）。 */
  lastTrigger: number | null
  /** succeeded / failed / interrupted / timeout / error */
  lastStatus: string | null
  lastSessionId: string | null
  nextRun: number | null
  running: boolean
}

export interface RoutineLogEntry {
  startedAt: number
  finishedAt: number | null
  status: string
  sessionId: string | null
  engine: string
  directory: string
  error: string | null
}

/** 例行任务事件（Rust 调度器 → 前端通知 / 刷新）。 */
export interface RoutineEventPayload {
  kind: 'done' | 'failed' | 'permission'
  routineId: string
  name: string
  engine: string
  directory: string
  sessionId: string | null
  startedAt: number
  status: string | null
  error: string | null
  requestId: string | null
}
