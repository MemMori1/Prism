import { beforeEach, describe, expect, it, vi } from 'vitest'
import {
  DEDUPE_WINDOW_MS,
  MAX_ITEMS,
  clearNotifications,
  markAllRead,
  notifications,
  pushNotification,
  resetNotificationsForTest,
  setSystemNotifier,
  unreadCount,
} from '../src/notify'

beforeEach(() => {
  resetNotificationsForTest()
  vi.restoreAllMocks()
})

describe('pushNotification', () => {
  it('新增条目：最新在前、默认未读、带会话归属', () => {
    expect(
      pushNotification({
        key: 's1:done',
        kind: 'done',
        title: '「任务」回合完成',
        body: 'OpenCode',
        sessionId: 's1',
        projectId: 'p1',
      }),
    ).toBe(true)
    expect(notifications.value).toHaveLength(1)
    expect(notifications.value[0]).toMatchObject({
      kind: 'done',
      title: '「任务」回合完成',
      sessionId: 's1',
      projectId: 'p1',
      read: false,
    })
    expect(unreadCount.value).toBe(1)
  })

  it('同键在去重窗口内合并（归档与工作台双实例回显）', () => {
    const now = vi.spyOn(Date, 'now').mockReturnValue(1_000_000)
    pushNotification({ key: 's1:done', kind: 'done', title: 'T', body: 'B' })
    now.mockReturnValue(1_000_000 + DEDUPE_WINDOW_MS - 1)
    expect(pushNotification({ key: 's1:done', kind: 'done', title: 'T', body: 'B' })).toBe(false)
    expect(notifications.value).toHaveLength(1)
  })

  it('同键超出窗口后重新记录（同一会话的下一回合）', () => {
    const now = vi.spyOn(Date, 'now').mockReturnValue(1_000_000)
    pushNotification({ key: 's1:done', kind: 'done', title: 'T', body: 'B' })
    now.mockReturnValue(1_000_000 + DEDUPE_WINDOW_MS)
    expect(pushNotification({ key: 's1:done', kind: 'done', title: 'T', body: 'B' })).toBe(true)
    expect(notifications.value).toHaveLength(2)
  })

  it('dedupe=key 永久去重（权限请求被引擎重放时不重复）', () => {
    const now = vi.spyOn(Date, 'now').mockReturnValue(1_000_000)
    const input = {
      key: 's1:perm:r1',
      kind: 'permission' as const,
      title: 'T',
      body: 'B',
      dedupe: 'key' as const,
    }
    expect(pushNotification(input)).toBe(true)
    now.mockReturnValue(1_000_000 + 60 * 60 * 1000)
    expect(pushNotification(input)).toBe(false)
    expect(notifications.value).toHaveLength(1)
  })

  it('超出上限丢弃最旧', () => {
    let tick = 1_000_000
    vi.spyOn(Date, 'now').mockImplementation(() => tick++)
    for (let i = 0; i < MAX_ITEMS + 5; i++) {
      pushNotification({ key: `k${i}`, kind: 'done', title: 'T', body: 'B' })
    }
    expect(notifications.value).toHaveLength(MAX_ITEMS)
    expect(notifications.value[0].key).toBe(`k${MAX_ITEMS + 4}`)
    expect(notifications.value.at(-1)?.key).toBe('k5')
  })

  it('系统通知：注入的发送器拿到标题与正文', () => {
    const sent: Array<[string, string]> = []
    setSystemNotifier((title, body) => sent.push([title, body]))
    pushNotification({ key: 'a', kind: 'done', title: 'T1', body: 'B1' })
    expect(sent).toEqual([['T1', 'B1']])
  })

  it('系统通知抛错不影响条目记录', () => {
    setSystemNotifier(() => {
      throw new Error('boom')
    })
    expect(pushNotification({ key: 'b', kind: 'failed', title: 'T2', body: 'B2' })).toBe(true)
    expect(notifications.value[0].key).toBe('b')
  })
})

describe('markAllRead / clearNotifications', () => {
  it('标记全部已读', () => {
    pushNotification({ key: 'a', kind: 'done', title: 'T', body: 'B' })
    pushNotification({ key: 'b', kind: 'failed', title: 'T', body: 'B' })
    expect(unreadCount.value).toBe(2)
    markAllRead()
    expect(unreadCount.value).toBe(0)
    expect(notifications.value.every(item => item.read)).toBe(true)
  })

  it('清空', () => {
    pushNotification({ key: 'a', kind: 'done', title: 'T', body: 'B' })
    clearNotifications()
    expect(notifications.value).toHaveLength(0)
    expect(unreadCount.value).toBe(0)
  })
})
