/**
 * 应用内通知中心（M11 自动化与通知）：回合完成 / 失败 / 待审批沉淀为持久条目。
 *
 * 设计：条目是纯内存事件流，不做引擎状态副本——点击跳转仍以项目 / 会话
 * 选中态为准。归档视图与工作台列可能对同一条引擎事件各上报一次，按
 * "键 + 去重窗口"合并；权限请求按请求 id 永久去重（引擎重连可能重放）。
 *
 * 系统通知走注入的发送器（非前台判定与权限处理在 systemNotify.ts），
 * 发送失败静默降级——应用内条目不受影响。
 */
import { computed, readonly, ref } from 'vue'

export type NotifyKind = 'done' | 'failed' | 'permission'

export interface NotifyItem {
  id: number
  /** 去重键（会话 + 事件签名）。 */
  key: string
  kind: NotifyKind
  title: string
  body: string
  sessionId: string | null
  projectId: string | null
  time: number
  read: boolean
}

/** 条目上限（超出丢弃最旧）。 */
export const MAX_ITEMS = 100
/** 同键去重窗口：合并双视图实例对同一条引擎事件的上报。 */
export const DEDUPE_WINDOW_MS = 10_000

const items = ref<NotifyItem[]>([])
let nextId = 1

export const notifications = readonly(items)
export const unreadCount = computed(() => items.value.filter(item => !item.read).length)

export interface NotifyInput {
  key: string
  kind: NotifyKind
  title: string
  body: string
  sessionId?: string | null
  projectId?: string | null
  /**
   * 去重方式：window = 同键在窗口内只留一条（默认）；
   * key = 同键永久去重（权限请求按 id 唯一）。
   */
  dedupe?: 'window' | 'key'
}

type SystemNotifier = (title: string, body: string) => void

/** 系统通知发送器（main.ts 注入真实实现；默认空实现 = 静默降级）。 */
let systemNotifier: SystemNotifier = () => {}

export function setSystemNotifier(notifier: SystemNotifier): void {
  systemNotifier = notifier
}

/** 记录一条通知。返回是否新增（被去重合并时为 false）。 */
export function pushNotification(input: NotifyInput): boolean {
  const now = Date.now()
  const mode = input.dedupe ?? 'window'
  const duplicate = items.value.some(item => {
    if (item.key !== input.key) return false
    if (mode === 'key') return true
    return now - item.time < DEDUPE_WINDOW_MS
  })
  if (duplicate) return false

  items.value = [
    {
      id: nextId++,
      key: input.key,
      kind: input.kind,
      title: input.title,
      body: input.body,
      sessionId: input.sessionId ?? null,
      projectId: input.projectId ?? null,
      time: now,
      read: false,
    },
    ...items.value,
  ].slice(0, MAX_ITEMS)

  try {
    systemNotifier(input.title, input.body)
  } catch {
    // 系统通知失败不影响主流程。
  }
  return true
}

export function markAllRead(): void {
  items.value = items.value.map(item => (item.read ? item : { ...item, read: true }))
}

export function clearNotifications(): void {
  items.value = []
}

/** 测试辅助：重置 store（含 id 计数与发送器）。 */
export function resetNotificationsForTest(): void {
  items.value = []
  nextId = 1
  systemNotifier = () => {}
}
