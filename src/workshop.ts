import type { AssetItem, AssetKind, McpServer } from './types'

/** 工坊展示辅助：标签、分组与归属文案（纯函数，可单测）。 */

export const ASSET_ENGINE_LABELS: Record<string, string> = {
  native: 'Prism 原生',
  opencode: 'OpenCode',
  codex: 'Codex',
  dsh: 'DeepSeek Harness',
  claude: 'Claude Code',
}

export const ASSET_KIND_LABELS: Record<AssetKind, string> = {
  skill: '技能',
  command: '命令',
  agent: '子代理',
  memory: '记忆',
  plugin: '插件',
}

export function engineLabel(engine: string): string {
  return ASSET_ENGINE_LABELS[engine] ?? engine
}

export function kindLabel(kind: string): string {
  return ASSET_KIND_LABELS[kind as AssetKind] ?? kind
}

export interface AssetGroup {
  key: string
  engine: string
  kind: AssetKind
  label: string
  items: AssetItem[]
}

const ENGINE_ORDER = ['opencode', 'codex', 'dsh', 'claude']
const KIND_ORDER: AssetKind[] = ['skill', 'command', 'agent', 'memory', 'plugin']

function orderOf(list: readonly string[], value: string): number {
  const index = list.indexOf(value)
  return index < 0 ? list.length : index
}

/** 按 引擎×种类 分组，并保持稳定顺序（未知引擎/种类排最后）。 */
export function groupAssets(items: AssetItem[]): AssetGroup[] {
  const groups = new Map<string, AssetGroup>()
  for (const item of items) {
    const key = `${item.engine}:${item.kind}`
    const group = groups.get(key)
    if (group) {
      group.items.push(item)
    } else {
      groups.set(key, {
        key,
        engine: item.engine,
        kind: item.kind,
        label: kindLabel(item.kind),
        items: [item],
      })
    }
  }
  return [...groups.values()].sort((a, b) => {
    const engineDelta = orderOf(ENGINE_ORDER, a.engine) - orderOf(ENGINE_ORDER, b.engine)
    if (engineDelta !== 0) {
      return engineDelta
    }
    return orderOf(KIND_ORDER, a.kind) - orderOf(KIND_ORDER, b.kind)
  })
}

/** 归属文案：用户 / 内置 / 项目 · 目录名。 */
export function scopeLabel(item: { scope: string; project: string | null }): string {
  if (item.scope === 'user') {
    return '用户'
  }
  if (item.scope === 'builtin') {
    return '内置'
  }
  const name = item.project?.split('/').filter(Boolean).pop()
  return name ? `项目 · ${name}` : '项目'
}

export interface McpGroup {
  engine: string
  label: string
  items: McpServer[]
}

/** MCP 服务按引擎分组（引擎序固定）。 */
export function groupMcp(servers: McpServer[]): McpGroup[] {
  const groups = new Map<string, McpGroup>()
  for (const server of servers) {
    const existing = groups.get(server.engine)
    if (existing) {
      existing.items.push(server)
    } else {
      groups.set(server.engine, {
        engine: server.engine,
        label: engineLabel(server.engine),
        items: [server],
      })
    }
  }
  return [...groups.values()].sort(
    (a, b) => orderOf(ENGINE_ORDER, a.engine) - orderOf(ENGINE_ORDER, b.engine),
  )
}

/** stdio 服务的命令行展示（可执行文件 + 参数）。 */
export function commandLine(server: McpServer): string {
  return [server.command, ...server.args]
    .filter((part): part is string => Boolean(part))
    .join(' ')
}
