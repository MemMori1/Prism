import type { EngineHealth } from './types'

export type EngineTone = 'muted' | 'ok' | 'warn' | 'error'

export interface EngineStatus {
  label: string
  tone: EngineTone
}

/** 简短引擎标识，用于左侧栏 / 首页等窄空间。 */
const ENGINE_BADGES: Record<string, string> = {
  opencode: 'OC',
  codex: 'CX',
  dsh: 'DS',
  claude: 'CC',
}

export function engineBadge(engine: string): string {
  return ENGINE_BADGES[engine] ?? engine.slice(0, 2).toUpperCase()
}

/** 引擎就绪状态：CLI 可用即 ready；只有历史数据库时提示只读。 */
export function engineStatus(item: EngineHealth): EngineStatus {
  if (item.ready) return { label: '已就绪', tone: 'ok' }
  if (item.databasePath) return { label: '只读（未找到 CLI）', tone: 'warn' }
  return { label: '未安装', tone: 'error' }
}
