/**
 * 解析 unified diff 补丁（OpenCode edit 工具 metadata.files[].patch）
 * 为可逐行渲染的结构。
 */

export type DiffRowType = 'hunk' | 'add' | 'del' | 'context'

export interface DiffRow {
  type: DiffRowType
  text: string
}

/**
 * 解析规则：
 * - 第一个 `@@` 之前是头部（Index/diff/---/+++ 等），全部跳过；
 * - `@@` 行标记为 hunk（用于视觉分隔）；
 * - `+` / `-` / 空格前缀分别是新增 / 删除 / 上下文；
 * - `\ No newline at end of file` 跳过；空行按上下文处理。
 */
export function parsePatch(patch: string): DiffRow[] {
  const rows: DiffRow[] = []
  let inHunk = false

  for (const raw of patch.split('\n')) {
    if (!inHunk) {
      if (raw.startsWith('@@')) {
        inHunk = true
        rows.push({ type: 'hunk', text: raw })
      }
      continue
    }
    if (raw.startsWith('@@')) {
      rows.push({ type: 'hunk', text: raw })
      continue
    }
    if (raw.startsWith('\\')) continue
    if (raw.startsWith('+')) {
      rows.push({ type: 'add', text: raw.slice(1) })
      continue
    }
    if (raw.startsWith('-')) {
      rows.push({ type: 'del', text: raw.slice(1) })
      continue
    }
    if (raw.startsWith(' ')) {
      rows.push({ type: 'context', text: raw.slice(1) })
      continue
    }
    if (raw === '') {
      rows.push({ type: 'context', text: '' })
      continue
    }
    rows.push({ type: 'context', text: raw })
  }

  return rows
}

export function patchStats(rows: DiffRow[]): { added: number; removed: number } {
  let added = 0
  let removed = 0
  for (const row of rows) {
    if (row.type === 'add') added += 1
    else if (row.type === 'del') removed += 1
  }
  return { added, removed }
}
