import { invoke } from '@tauri-apps/api/core'
import type {
  AnnotationStore,
  ArtifactData,
  AssetFile,
  AssetItem,
  CreatedSession,
  EngineCatalog,
  EngineHealth,
  FileHit,
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
  ProjectItem,
  QuotaSummary,
  RoutineDefinition,
  RoutineLogEntry,
  RuntimeStatus,
  SearchHit,
  SearchStats,
  SendOptions,
  SessionLedger,
  SessionSummary,
  TimelinePage,
  ToolDetail,
  UsageSummary,
} from './types'

/** Tauri IPC 封装：所有后端调用从这里进。 */
export const api = {
  engineHealth: () => invoke<EngineHealth[]>('engine_health'),
  cliVersion: () => invoke<string | null>('cli_version'),
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
  sessionLedger: (sessionId: string) =>
    invoke<SessionLedger>('session_ledger', { sessionId }),
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
  nativeModelsFetch: (input: {
    providerId?: string | null
    baseUrl?: string | null
    apiKey?: string | null
  }) => invoke<NativeModel[]>('native_models_fetch', input),
  nativeProbe: (providerId?: string | null, model?: string | null) =>
    invoke<string>('native_probe', { providerId, model }),
}
