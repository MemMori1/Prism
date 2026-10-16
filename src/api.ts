import { invoke } from '@tauri-apps/api/core'
import type { EngineHealth, ProjectItem, SessionSummary, TimelinePage, ToolDetail } from './types'

/** Tauri IPC 封装：所有后端调用从这里进。 */
export const api = {
  engineHealth: () => invoke<EngineHealth>('engine_health'),
  listProjects: () => invoke<ProjectItem[]>('list_projects'),
  listSessions: (projectId: string) =>
    invoke<SessionSummary[]>('list_sessions', { projectId }),
  readTimeline: (sessionId: string, beforeSeq?: number) =>
    invoke<TimelinePage>('read_timeline', { sessionId, beforeSeq: beforeSeq ?? null }),
  readToolDetail: (sessionId: string, messageId: string, callId: string) =>
    invoke<ToolDetail>('read_tool_detail', { sessionId, messageId, callId }),
}
