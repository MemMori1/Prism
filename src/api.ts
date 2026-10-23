import { invoke } from '@tauri-apps/api/core'
import type {
  AgentProfile,
  AnnotationStore,
  ArtifactData,
  AssetFile,
  AssetItem,
  BackgroundTask,
  BrowserSnapshot,
  ChangeReport,
  ContextStats,
  CreatedSession,
  DagIssue,
  DagArtifactList,
  DagArtifactText,
  DagPlan,
  DagRun,
  DagSummary,
  DshPermissionView,
  EngineCatalog,
  EngineHealth,
  FileDiff,
  FileHit,
  FileText,
  GitBranches,
  GitInfo,
  GitSnapshot,
  McpProbeResult,
  McpServer,
  NativeConfigView,
  NativeLimitsInput,
  NativeModel,
  NativeProviderInput,
  PermissionRequest,
  PluginInfo,
  ProjectItem,
  QuotaSummary,
  RoutineDefinition,
  RoutineLogEntry,
  RuntimeStatus,
  SearchHit,
  SearchStats,
  SendOptions,
  SessionSummary,
  TaskSpawnInput,
  TimelinePage,
  ToolDetail,
  ToolView,
  UsageSummary,
} from './types'

/** Tauri IPC 封装：所有后端调用从这里进。 */
export const api = {
  engineHealth: () => invoke<EngineHealth[]>('engine_health'),
  cliVersion: () => invoke<string | null>('cli_version'),
  /** dsh 权限档位（进程级，改完会重启 dsh 进程）。 */
  dshPermission: () => invoke<DshPermissionView>('dsh_permission'),
  dshSetPermission: (mode: string | null) =>
    invoke<DshPermissionView>('dsh_set_permission', { mode }),
  listProjects: () => invoke<ProjectItem[]>('list_projects'),
  listSessions: (projectId: string) =>
    invoke<SessionSummary[]>('list_sessions', { projectId }),
  sessionSummary: (sessionId: string) =>
    invoke<SessionSummary | null>('session_summary', { sessionId }),
  readTimeline: (sessionId: string, beforeSeq?: number, limit?: number) =>
    invoke<TimelinePage>('read_timeline', {
      sessionId,
      beforeSeq: beforeSeq ?? null,
      limit: limit ?? null,
    }),
  readToolDetail: (sessionId: string, messageId: string, callId: string) =>
    invoke<ToolDetail>('read_tool_detail', { sessionId, messageId, callId }),
  runtimeStatus: () => invoke<RuntimeStatus>('runtime_status'),
  sessionCreate: (directory: string, engine?: string) =>
    invoke<CreatedSession>('session_create', { directory, engine: engine ?? null }),
  sessionSend: (sessionId: string, text: string, options?: SendOptions) =>
    invoke<string>('session_send', { sessionId, text, options: options ?? null }),
  sessionInterrupt: (sessionId: string) => invoke<void>('session_interrupt', { sessionId }),
  sessionPermissions: (sessionId: string) =>
    invoke<PermissionRequest[]>('session_permissions', { sessionId }),
  sessionPermissionReply: (sessionId: string, requestId: string, decision: string) =>
    invoke<void>('session_permission_reply', { sessionId, requestId, decision }),
  searchStats: () => invoke<SearchStats>('search_stats'),
  searchUpdateIndex: () => invoke<SearchStats>('search_update_index'),
  searchQuery: (
    term: string,
    options?: {
      projectId?: string | null
      since?: number | null
      onlyTitle?: boolean
      limit?: number
    },
  ) =>
    invoke<SearchHit[]>('search_query', {
      term,
      projectId: options?.projectId ?? null,
      since: options?.since ?? null,
      onlyTitle: options?.onlyTitle ?? false,
      limit: options?.limit ?? null,
    }),
  workshopAssets: () => invoke<AssetItem[]>('workshop_assets'),
  workshopRead: (path: string) => invoke<AssetFile>('workshop_read', { path }),
  openPath: (path: string) => invoke<void>('open_path', { path }),
  readArtifact: (path: string) => invoke<ArtifactData>('read_artifact', { path }),
  /** 会话目录的未提交改动（文件变更追踪）。 */
  changesList: (directory: string) =>
    invoke<ChangeReport>('changes_list', { directory }),
  /** 单个文件的统一 diff。 */
  changesDiff: (directory: string, path: string) =>
    invoke<FileDiff>('changes_diff', { directory, path }),
  /** 文件预览（文本；二进制只回标记）。 */
  fileText: (path: string) => invoke<FileText>('file_text', { path }),
  /** 在系统文件管理器里定位这个文件。 */
  revealPath: (path: string) => invoke<void>('reveal_path', { path }),
  /** 上下文明细（系统提示 / 工具 / 对话各多少字符；原生引擎才有）。 */
  contextStats: (sessionId: string) => invoke<ContextStats | null>('context_stats', { sessionId }),
  /** 这个目录现在还在不在（历史会话可能指向已删 / 已移动的目录）。 */
  dirExists: (path: string) => invoke<boolean>('dir_exists', { path }),
  /** 保存文本文件（只允许会话目录 / 仓库根之内的已存在文件）。 */
  writeText: (path: string, text: string, directory: string) =>
    invoke<number>('write_text', { path, text, directory }),
  annotationsGet: () => invoke<AnnotationStore>('annotations_get'),
  annotationUpdate: (
    sessionId: string,
    patch: { title?: string; starred?: boolean; tags?: string[] },
  ) =>
    invoke<AnnotationStore>('annotation_update', {
      sessionId,
      title: patch.title ?? null,
      starred: patch.starred ?? null,
      tags: patch.tags ?? null,
    }),
  tagCreate: (name: string, color: string) =>
    invoke<AnnotationStore>('tag_create', { name, color }),
  /** 面板浏览器：打开地址 / 同步矩形 / 显隐 / 导航 / 读状态。 */
  browserOpen: (url: string) => invoke<BrowserSnapshot>('browser_open', { url }),
  browserBounds: (bounds: { x: number; y: number; width: number; height: number }) =>
    invoke<void>('browser_bounds', { bounds }),
  browserVisible: (visible: boolean) => invoke<void>('browser_visible', { visible }),
  browserHistory: (direction: 'back' | 'forward' | 'reload') =>
    invoke<void>('browser_history', { direction }),
  browserState: () => invoke<BrowserSnapshot>('browser_state'),
  /** 用系统默认浏览器打开一个 http(s) 地址（内嵌浏览器不可用时的退路）。 */
  openUrl: (url: string) => invoke<void>('open_url', { url }),
  /** 批量关闭 / 恢复会话（只写增补表，引擎数据不动）。 */
  annotationsSetHidden: (sessionIds: string[], hidden: boolean) =>
    invoke<AnnotationStore>('annotations_set_hidden', { sessionIds, hidden }),
  tagUpdate: (id: string, patch: { name?: string; color?: string }) =>
    invoke<AnnotationStore>('tag_update', {
      id,
      name: patch.name ?? null,
      color: patch.color ?? null,
    }),
  tagDelete: (id: string) => invoke<AnnotationStore>('tag_delete', { id }),
  tagMerge: (sourceId: string, targetId: string) =>
    invoke<AnnotationStore>('tag_merge', { sourceId, targetId }),
  gitInfoBatch: (paths: string[]) =>
    invoke<Record<string, GitInfo>>('git_info_batch', { paths }),
  workshopMcp: () => invoke<McpServer[]>('workshop_mcp'),
  mcpProbe: (url: string) => invoke<McpProbeResult>('mcp_probe', { url }),
  routinesList: () => invoke<RoutineDefinition[]>('routines_list'),
  routineCreate: (name: string, schedule: string, prompt: string, engine: string) =>
    invoke<RoutineDefinition>('routine_create', { name, schedule, prompt, engine }),
  routineUpdate: (
    id: string,
    patch: {
      name?: string
      schedule?: string
      prompt?: string
      engine?: string
      enabled?: boolean
    },
  ) =>
    invoke<RoutineDefinition>('routine_update', {
      id,
      name: patch.name ?? null,
      schedule: patch.schedule ?? null,
      prompt: patch.prompt ?? null,
      engine: patch.engine ?? null,
      enabled: patch.enabled ?? null,
    }),
  routineDelete: (id: string) => invoke<void>('routine_delete', { id }),
  routineRun: (id: string) => invoke<void>('routine_run', { id }),
  routineLogs: (id: string, limit?: number) =>
    invoke<RoutineLogEntry[]>('routine_logs', { id, limit: limit ?? null }),
  usageSummary: (days: number) => invoke<UsageSummary>('usage_summary', { days }),
  quotaSummary: (force: boolean) => invoke<QuotaSummary>('quota_summary', { force }),
  trayStatus: (running: number, waiting: number) =>
    invoke<void>('tray_status', { running, waiting }),
  engineCatalog: (engine: string, directory: string) =>
    invoke<EngineCatalog>('engine_catalog', { engine, directory }),
  sessionShell: (sessionId: string, command: string) =>
    invoke<void>('session_shell', { sessionId, command }),
  sessionCommand: (sessionId: string, name: string, text: string) =>
    invoke<string>('session_command', { sessionId, name, text }),
  projectFiles: (directory: string, query: string, limit?: number) =>
    invoke<FileHit[]>('project_files', { directory, query, limit: limit ?? null }),
  gitSnapshot: (directory: string) => invoke<GitSnapshot>('git_snapshot', { directory }),
  gitBranches: (directory: string) => invoke<GitBranches>('git_branches', { directory }),
  gitSwitchBranch: (directory: string, branch: string, create = false) =>
    invoke<GitSnapshot>('git_switch_branch', { directory, branch, create }),
  /** 原生引擎（Prism 内置 agent）的模型服务。 */
  nativeConfig: () => invoke<NativeConfigView>('native_config'),
  nativeProviderSave: (provider: NativeProviderInput) =>
    invoke<NativeConfigView>('native_provider_save', { provider }),
  nativeProviderDelete: (id: string) =>
    invoke<NativeConfigView>('native_provider_delete', { id }),
  nativeModelSelect: (provider: string, model: string) =>
    invoke<NativeConfigView>('native_model_select', { provider, model }),
  nativeLimitsSet: (limits: NativeLimitsInput) =>
    invoke<NativeConfigView>('native_config_set', { limits }),
  /** 开关一个工具：关掉的不进 schema，也不占 token。内置与外部工具同一条命令。 */
  nativeToolToggle: (name: string, enabled: boolean) =>
    invoke<NativeConfigView>('native_tool_toggle', { name, enabled }),
  /** 工具清单：内置 + 插件运行时注册的外部工具。 */
  toolList: () => invoke<ToolView[]>('tool_list'),
  nativeModelsFetch: (input: {
    providerId?: string | null
    baseUrl?: string | null
    apiKey?: string | null
  }) => invoke<NativeModel[]>('native_models_fetch', input),
  nativeProbe: (providerId?: string | null, model?: string | null) =>
    invoke<string>('native_probe', { providerId, model }),

  /** 后台任务：起进程 / 列任务 / 停止 / 移出列表。 */
  taskSpawn: (input: TaskSpawnInput) =>
    invoke<BackgroundTask>('task_spawn', {
      cmd: input.cmd,
      cwd: input.cwd,
      sessionId: input.sessionId ?? null,
      title: input.title ?? null,
      background: input.background ?? true,
    }),
  taskList: () => invoke<BackgroundTask[]>('task_list'),
  taskStop: (id: string) => invoke<void>('task_stop', { id }),
  taskRemove: (id: string) => invoke<void>('task_remove', { id }),

  // ---- Agent 注册表 ----
  /** 内置 + 自定义（同 id 时自定义覆盖内置）。 */
  agentsList: () => invoke<AgentProfile[]>('agents_list'),
  agentSave: (profile: AgentProfile) => invoke<AgentProfile>('agent_save', { profile }),
  agentDelete: (id: string) => invoke<void>('agent_delete', { id }),

  // ---- DAG 编排 ----
  dagList: () => invoke<DagSummary[]>('dag_list'),
  dagGet: (id: string) => invoke<DagPlan>('dag_get', { id }),
  /** 空白草稿（带新 id）：手动搭图用。 */
  dagNew: (project: string) => invoke<DagPlan>('dag_new', { project }),
  dagSave: (plan: DagPlan) => invoke<DagPlan>('dag_save', { plan }),
  dagDelete: (id: string) => invoke<void>('dag_delete', { id }),
  dagValidate: (plan: DagPlan) => invoke<DagIssue[]>('dag_validate', { plan }),
  /** 让 Planner 拆图。不落盘：确认后再 `dagSave`。 */
  dagPlan: (goal: string, project: string, maxNodes?: number) =>
    invoke<DagPlan>('dag_plan', { goal, project, maxNodes: maxNodes ?? null }),

  // ---- 运行 ----
  /**
   * 起一次运行。**立刻返回**：返回的那一刻图刚开跑，后续进度靠 `dag.run` 事件推。
   * 校验不过会抛人话（"这张图还不能跑：…"），不会留一个空运行。
   */
  dagRun: (id: string) => invoke<DagRun>('dag_run', { id }),
  /** 停止：正在跑的节点在下次工具/模型调用前收手。 */
  dagRunCancel: (runId: string) => invoke<DagRun>('dag_run_cancel', { runId }),
  /** 人工闸门：`allowed=false` 表示别跑了（节点取消、下游跳过）。 */
  dagRunApprove: (runId: string, nodeId: string, allowed: boolean) =>
    invoke<DagRun>('dag_run_approve', { runId, nodeId, allowed }),
  /** 某张计划最近一次运行（进程重启后从磁盘捡回来）。 */
  dagRunLatest: (planId: string) => invoke<DagRun | null>('dag_run_latest', { planId }),
  dagRunGet: (runId: string) => invoke<DagRun | null>('dag_run_get', { runId }),
  /** 已装的节点执行器 id：Agent 编辑器据此标出"这个引擎现在跑不了"。 */
  dagRunners: () => invoke<string[]>('dag_runners'),
  /** 某个 DAG 的产物清单 + 已用空间。 */
  dagArtifacts: (dagId: string) => invoke<DagArtifactList>('dag_artifacts', { dagId }),
  /** 按引用取产物正文（预览用，超过 limit 会截断）。 */
  dagArtifactGet: (dagId: string, id: string, limit?: number) =>
    invoke<DagArtifactText>('dag_artifact_get', { dagId, id, limit: limit ?? null }),

  // ---- 插件清单 ----
  pluginsList: () => invoke<PluginInfo[]>('plugins_list'),
  /** 开关插件：**下次启动生效**。 */
  pluginSetEnabled: (id: string, enabled: boolean) =>
    invoke<void>('plugin_set_enabled', { id, enabled }),
}
