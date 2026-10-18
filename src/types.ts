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
  /** 模型提供方（OpenCode 的 providerID；其他引擎为 null/缺省）。 */
  modelProvider?: string | null
  /** 该轮推理档位（OpenCode variant / Codex effort）。 */
  variant?: string | null
  completed: boolean
  parts: TimelinePart[]
  /** 该轮 token 用量（引擎数据可得时）。 */
  tokens?: TurnTokens | null
}

/** 单轮 token 用量（各引擎归一到同一口径：缓存读不与未缓存输入重叠）。 */
export interface TurnTokens {
  input: number
  cacheRead: number
  cacheWrite: number
  output: number
  reasoning: number
  /** 该轮成本（引擎提供时）。 */
  cost?: number
  /** 模型上下文窗口（已知时）。 */
  contextWindow?: number
}

/** 本地交付物内容（HTML / SVG / 图片）。 */
/** 会话目录里的一条未提交改动（git 工作树口径）。 */
export interface ChangeFile {
  /** 绝对路径。 */
  path: string
  /** 相对仓库根。 */
  rel: string
  status: 'added' | 'modified' | 'deleted' | 'renamed'
  /** 新文件 / 未跟踪：diff 由内容拼。 */
  untracked: boolean
  additions: number
  deletions: number
  binary: boolean
}

export interface ChangeReport {
  isRepo: boolean
  root: string | null
  branch: string | null
  files: ChangeFile[]
  additions: number
  deletions: number
  truncated: boolean
}

export interface FileDiff {
  rel: string
  patch: string
  truncated: boolean
  binary: boolean
}

/** 文件预览内容。 */
export interface FileText {
  path: string
  text: string
  size: number
  truncated: boolean
  binary: boolean
}

export interface ArtifactData {
  kind: string
  name: string
  size: number
  mime: string
  text?: string
  base64?: string
}

/** 单个会话的增补信息（星标 / 标签 / 自定义标题）。 */
export interface SessionAnnotation {
  title?: string
  starred: boolean
  tags: string[]
  updatedAt?: number
}

/** 标签定义。 */
export interface TagDef {
  id: string
  name: string
  color: string
}

/** 档案增补全量存储（后端返回后前端整体替换）。 */
export interface AnnotationStore {
  version: number
  tags: TagDef[]
  sessions: Record<string, SessionAnnotation>
}

/** 目录的 git 信息（分支 / 主仓库根 / worktree 标记）。 */
export interface GitInfo {
  repoRoot: string | null
  branch: string | null
  isWorktree: boolean
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

/** 跨引擎用量：token 分项（各列口径互不重叠，可直接相加）。 */
export interface TokenTotals {
  input: number
  output: number
  reasoning: number
  cacheRead: number
  cacheWrite: number
}

export interface UsageModel {
  model: string
  tokens: TokenTotals
  sessions: number
  /** 估算成本（美元）；价格表未收录为 null。 */
  costUsd: number | null
}

export interface UsageEngine {
  engine: string
  label: string
  tokens: TokenTotals
  sessions: number
  costUsd: number
  /** 价格表未收录的模型名（成本显示「未收录」）。 */
  unpricedModels: string[]
  models: UsageModel[]
}

export interface UsageDaily {
  /** 本地日期 YYYY-MM-DD。 */
  date: string
  tokens: TokenTotals
}

export interface UsageSummary {
  /** 统计窗口天数；0 = 全部。 */
  days: number
  sessions: number
  totals: TokenTotals
  costUsd: number
  engines: UsageEngine[]
  daily: UsageDaily[]
  /** 窗口内按本地小时的会话数（24 桶：工作节奏）。 */
  hours: number[]
  notes: string[]
}

/** 配额：单个订阅窗口。 */
export interface QuotaWindow {
  usedPercent: number
  windowMinutes: number | null
  /** Unix 毫秒（后端已归一）。 */
  resetsAtMs: number | null
}

export interface QuotaCredits {
  hasCredits: boolean
  unlimited: boolean
  balance: string | null
}

/** 配额：单引擎状态（ok | no_subscription | cli_missing | error | pending_cli）。 */
export interface EngineQuota {
  state: string
  accountType: string | null
  planType: string | null
  primary: QuotaWindow | null
  secondary: QuotaWindow | null
  credits: QuotaCredits | null
  message: string | null
}

export interface QuotaSummary {
  /** 本次取数时间（Unix 毫秒）。 */
  fetchedAtMs: number
  codex: EngineQuota
  claude: EngineQuota
}

// ---------------------------------------------------------------- 组合器（M20）

/** 组合器：引擎可选的模型（含推理档位）。 */
export interface EngineModel {
  id: string
  label: string
  provider: string | null
  /** 推理档位 id（OpenCode variant / Codex effort）。 */
  variants: string[]
  defaultVariant: string | null
  isDefault: boolean
}

/** 组合器：引擎斜杠命令。 */
export interface EngineCommand {
  name: string
  description: string
}

/** 引擎能力目录（模型 + 命令；不可用时带原因）。 */
export interface EngineCatalog {
  engine: string
  available: boolean
  reason: string | null
  models: EngineModel[]
  commands: EngineCommand[]
}

/** 项目文件索引条目（@ 补全）。 */
export interface FileHit {
  path: string
  name: string
}

/** Git 上下文快照（分支 / 变更 / 最近提交）。 */
export interface GitSnapshot {
  branch: string | null
  dirty: string[]
  commits: string[]
}

/** 本地分支（切换弹层用）。 */
export interface GitBranch {
  name: string
  /** tip 的提交摘要。 */
  subject: string
  /** tip 的提交时间（Unix 秒）。 */
  time: number
}

/** 分支概览。 */
export interface GitBranches {
  current: string | null
  /** 未提交改动条数（切换前提示）。 */
  dirty: number
  /** 按最近提交时间倒序。 */
  branches: GitBranch[]
}

/** 发送附件（uri 为绝对路径）。 */
export interface SendFileRef {
  uri: string
  name: string
}

/** 组合器发送可选项（模型 / 推理档位 / 附件）。 */
export interface SendOptions {
  files?: SendFileRef[]
  model?: string | null
  provider?: string | null
  variant?: string | null
}

/** 一个模型服务里的模型（`active` = 当前选中的那个）。 */
export interface NativeModel {
  id: string
  label: string
  active: boolean
}

/** 模型服务（端点）：密钥不回传，只给末四位。 */
export interface NativeProvider {
  id: string
  label: string
  protocol: string
  baseUrl: string
  hasKey: boolean
  keyHint: string
  models: NativeModel[]
  ready: boolean
  /** 当前选中的模型是否落在这个服务上。 */
  active: boolean
}

/** 当前选中的模型。 */
export interface NativeSelection {
  provider: string
  model: string
}

/** 原生引擎的模型管理视图。 */
export interface NativeConfigView {
  providers: NativeProvider[]
  active: NativeSelection | null
  maxTurns: number
  ready: boolean
  modelCount: number
}

/** 新增 / 编辑一个模型服务；`apiKey` 留空 = 沿用已存的密钥。 */
export interface NativeProviderInput {
  id?: string | null
  label: string
  baseUrl: string
  apiKey?: string
  clearKey?: boolean
  models: Array<{ id: string; label?: string | null }>
}

/** 只改运行参数。 */
export interface NativeLimitsInput {
  maxTurns?: number | null
}
