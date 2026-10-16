import { invoke } from '@tauri-apps/api/core'
import type { EngineHealth, ProjectItem, SessionSummary, TimelineRecord } from './types'

/** Tauri IPC 封装：所有后端调用从这里进。 */
export const api = {
  engineHealth: () => invoke<EngineHealth>('engine_health'),
  listProjects: () => invoke<ProjectItem[]>('list_projects'),
  listSessions: (projectId: string) =>
    invoke<SessionSummary[]>('list_sessions', { projectId }),
  readTimeline: (sessionId: string) =>
    invoke<TimelineRecord[]>('read_timeline', { sessionId }),
}
