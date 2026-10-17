import { invoke } from '@tauri-apps/api/core'
import type {
  AssetFile,
  AssetItem,
  CreatedSession,
  EngineHealth,
  McpProbeResult,
  McpServer,
  PermissionRequest,
  ProjectItem,
  QuotaSummary,
  RoutineDefinition,
  RoutineLogEntry,
  RuntimeStatus,
  SearchHit,
  SearchStats,
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
  readTimeline: (sessionId: string, beforeSeq?: number) =>
    invoke<TimelinePage>('read_timeline', { sessionId, beforeSeq: beforeSeq ?? null }),
  readToolDetail: (sessionId: string, messageId: string, callId: string) =>
    invoke<ToolDetail>('read_tool_detail', { sessionId, messageId, callId }),
  runtimeStatus: () => invoke<RuntimeStatus>('runtime_status'),
  sessionCreate: (directory: string, engine?: string) =>
    invoke<CreatedSession>('session_create', { directory, engine: engine ?? null }),
  sessionSend: (sessionId: string, text: string) =>
    invoke<string>('session_send', { sessionId, text }),
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
}
