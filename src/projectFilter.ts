import { engineLabel } from './workshop'

/** 项目栏的「项目类型」筛选：下拉选项、过滤与徽标缩写（纯函数，可单测）。 */

/** 下拉里代表「不限类型」。 */
export const ALL_ENGINES = 'all'

export interface EngineOption {
  value: string
  label: string
}

/** 固定顺序：原生排第一，与新建会话的入口卡片一致。 */
const ORDER = ['native', 'opencode', 'codex', 'claude']

/** 下拉选项只列当前真实存在的类型，避免选了个空列表。 */
export function projectEngineOptions(projects: ReadonlyArray<{ engine: string }>): EngineOption[] {
  const present = new Set(projects.map(project => project.engine))
  const options: EngineOption[] = [{ value: ALL_ENGINES, label: '全部类型' }]
  for (const id of ORDER) {
    if (present.has(id)) options.push({ value: id, label: engineLabel(id) })
  }
  // 没登记过的新引擎也照实列出来，不然接进来会「看不见」。
  for (const id of [...present].sort()) {
    if (!ORDER.includes(id)) options.push({ value: id, label: engineLabel(id) })
  }
  return options
}

export function filterByEngine<T extends { engine: string }>(projects: readonly T[], engine: string): T[] {
  if (!engine || engine === ALL_ENGINES) return [...projects]
  return projects.filter(project => project.engine === engine)
}

/** 徽标上的两字缩写（OC / CX / CC / PR）。 */
const SHORT_LABELS: Record<string, string> = {
  native: 'PR',
  opencode: 'OC',
  codex: 'CX',
  claude: 'CC',
}

export function engineBadge(engine: string): string {
  return SHORT_LABELS[engine] ?? engine.slice(0, 2).toUpperCase()
}
