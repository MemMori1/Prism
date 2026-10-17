/**
 * 工作台纯逻辑：列宽约束与布局序列化（与组件解耦，方便单测）。
 */

export interface WorkbenchLayoutEntry {
  sessionId: string
  width: number
}

export const WORKBENCH_MIN_WIDTH = 360
export const WORKBENCH_MAX_WIDTH = 900
export const WORKBENCH_DEFAULT_WIDTH = 480

/** 列宽夹到可用区间；非法数字回落到默认宽。 */
export function clampColumnWidth(width: number): number {
  if (!Number.isFinite(width)) return WORKBENCH_DEFAULT_WIDTH
  return Math.min(WORKBENCH_MAX_WIDTH, Math.max(WORKBENCH_MIN_WIDTH, Math.round(width)))
}

/** 把持久化字符串解析成布局条目；容错：坏数据一律丢弃，不抛异常。 */
export function parseWorkbenchLayout(raw: string | null): WorkbenchLayoutEntry[] {
  if (!raw) return []
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch {
    return []
  }
  if (!Array.isArray(parsed)) return []
  const entries: WorkbenchLayoutEntry[] = []
  for (const item of parsed) {
    if (!item || typeof item !== 'object') continue
    const record = item as Record<string, unknown>
    if (typeof record.sessionId !== 'string' || !record.sessionId) continue
    const width =
      typeof record.width === 'number' ? clampColumnWidth(record.width) : WORKBENCH_DEFAULT_WIDTH
    entries.push({ sessionId: record.sessionId, width })
  }
  return entries
}

/** 序列化布局条目（只保留 id 与宽度）。 */
export function serializeWorkbenchLayout(entries: WorkbenchLayoutEntry[]): string {
  return JSON.stringify(
    entries.map(entry => ({ sessionId: entry.sessionId, width: clampColumnWidth(entry.width) })),
  )
}
