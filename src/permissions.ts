/**
 * 权限审批的前端纯逻辑：引擎事件/列表端点 → 视图状态。
 *
 * 引擎契约（真机验证）：`permission.asked` 的 data 形如
 * `{ id, sessionID, action, resources[], save[], message?, source: {id, messageID, type} }`；
 * 回复决定值只有 `once | always | reject`。
 */

import type { PermissionRequest } from './types'

export type PermissionDecision = 'once' | 'always' | 'reject'

const ACTION_LABELS: Record<string, string> = {
  read: '读取文件',
  edit: '修改文件',
  external_directory: '访问外部目录',
}

function asText(value: unknown): string | null {
  return typeof value === 'string' && value.trim() ? value : null
}

/** 从引擎事件 data（或列表端点条目）解析权限请求；缺 id 则返回 null。 */
export function permissionFromEvent(data: unknown): PermissionRequest | null {
  if (!data || typeof data !== 'object') return null
  const record = data as Record<string, unknown>
  const id = asText(record.id)
  if (!id) return null
  const resources = Array.isArray(record.resources)
    ? record.resources.filter((item): item is string => typeof item === 'string')
    : []
  const source =
    record.source && typeof record.source === 'object'
      ? (record.source as Record<string, unknown>)
      : null
  return {
    id,
    sessionId: asText(record.sessionID) ?? '',
    action: asText(record.action) ?? 'unknown',
    resources,
    message: asText(record.message),
    sourceMessageId: source ? asText(source.messageID) : null,
    sourceCallId: source ? asText(source.id) : null,
  }
}

/** 卡片标题：引擎给的消息优先，其次按动作映射。 */
export function permissionHeadline(request: PermissionRequest): string {
  if (request.message) return request.message
  const label = ACTION_LABELS[request.action] ?? `执行 ${request.action}`
  return `${label}需要授权`
}

/** 资源行（过长列表折叠）。 */
export function permissionResources(request: PermissionRequest, limit = 3): string[] {
  const list = request.resources.length ? request.resources : ['（未提供路径）']
  if (list.length <= limit) return list
  return [...list.slice(0, limit), `…等 ${list.length} 项`]
}

/** 按 id 合并去重（保持现有顺序，新请求追加）。 */
export function mergePermissions(
  current: PermissionRequest[],
  incoming: PermissionRequest[],
): PermissionRequest[] {
  const seen = new Set(current.map(item => item.id))
  const additions = incoming.filter(item => !seen.has(item.id))
  return additions.length ? [...current, ...additions] : current
}

/** 移除已处理的请求（无变化时返回原引用）。 */
export function removePermission(
  current: PermissionRequest[],
  id: string,
): PermissionRequest[] {
  const next = current.filter(item => item.id !== id)
  return next.length === current.length ? current : next
}

/** 从 permission.replied / permission.rejected 事件里取请求 id。 */
export function repliedRequestId(data: unknown): string | null {
  if (!data || typeof data !== 'object') return null
  const record = data as Record<string, unknown>
  return asText(record.requestID) ?? asText(record.id)
}
