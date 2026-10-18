import type { EngineHealth, ProjectItem } from './types'

/** 引擎图标样式（无第三方图标依赖，用内联 SVG / 文本绘制）。 */
export type EngineIcon = 'claude' | 'codex' | 'opencode' | 'prism'

export interface EngineChoice {
  id: string
  label: string
  description: string
  icon: EngineIcon
  /** 本机引擎健康检查通过，且 Prism 运行轴支持新建会话。 */
  available: boolean
  /** 尚未拿到健康数据。 */
  checking: boolean
  /** 原生引擎：还没配 API（点开卡片去填）。 */
  needsSetup?: boolean
}

/** 运行轴当前能从 Prism 直接建会话的引擎。 */
export const CREATABLE_ENGINE_IDS = ['opencode', 'codex', 'native'] as const

const PRESENTATION: ReadonlyArray<Omit<EngineChoice, 'available' | 'checking'>> = [
  {
    id: 'native',
    label: 'Prism 原生',
    description: '内置 agent：用自己的 API Key 直连模型，读写文件与跑命令',
    icon: 'prism',
  },
  {
    id: 'claude',
    label: 'Claude Code',
    description: '使用 Claude Code 开始一个空白会话',
    icon: 'claude',
  },
  {
    id: 'codex',
    label: 'Codex',
    description: '使用 Codex 开始一个空白会话',
    icon: 'codex',
  },
  {
    id: 'opencode',
    label: 'OpenCode',
    description: '使用 OpenCode 开始一个空白会话',
    icon: 'opencode',
  },
]

function creatable(id: string): boolean {
  return (CREATABLE_ENGINE_IDS as readonly string[]).includes(id)
}

/** 把 engine_health 映射成 Agent 卡片；顺序与 Monet 新任务页一致。 */
export function engineChoices(health: EngineHealth[]): EngineChoice[] {
  const byId = new Map(health.map(item => [item.engineId, item]))
  return PRESENTATION.map(item => {
    const entry = byId.get(item.id)
    // 原生引擎不依赖本机 CLI：永远可点，没配 API 时点开是配置向导。
    if (item.id === 'native') {
      return {
        ...item,
        available: true,
        checking: false,
        needsSetup: !!entry && !entry.ready,
      }
    }
    return {
      ...item,
      available: !!entry?.ready && creatable(item.id),
      checking: !entry,
    }
  })
}

/** 取最近活跃的项目（按 timeUpdated）。 */
export function newestProject(projects: ProjectItem[]): ProjectItem | null {
  let newest: ProjectItem | null = null
  for (const project of projects) {
    if (!newest || project.timeUpdated > newest.timeUpdated) newest = project
  }
  return newest
}
