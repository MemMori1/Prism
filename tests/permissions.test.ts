import { describe, expect, it } from 'vitest'
import type { PermissionRequest } from '../src/types'
import {
  mergePermissions,
  permissionFromEvent,
  permissionHeadline,
  permissionResources,
  removePermission,
  repliedRequestId,
} from '../src/permissions'

/** 真机抓到的 permission.asked data 形状（读 .env 触发）。 */
const askedData = {
  action: 'read',
  id: 'per_14720267a001XATm8hoUGHTavL',
  resources: ['.env'],
  save: ['*'],
  sessionID: 'ses_eb8dfe32fffeQv9WGD2nw6h9yZ',
  source: {
    id: 'call-901f9a53-7c7c-4040-97a8-77ddb15dfcfc',
    messageID: 'msg_147201d8d001cOnrnh5D2Lk9UV',
    type: 'tool',
  },
}

const request = (overrides: Partial<PermissionRequest> = {}): PermissionRequest => ({
  id: 'per_1',
  sessionId: 'ses_1',
  action: 'read',
  resources: ['.env'],
  message: null,
  sourceMessageId: null,
  sourceCallId: null,
  ...overrides,
})

describe('permissionFromEvent', () => {
  it('解析引擎真实形状', () => {
    const parsed = permissionFromEvent(askedData)
    expect(parsed).toEqual({
      id: 'per_14720267a001XATm8hoUGHTavL',
      sessionId: 'ses_eb8dfe32fffeQv9WGD2nw6h9yZ',
      action: 'read',
      resources: ['.env'],
      message: null,
      sourceMessageId: 'msg_147201d8d001cOnrnh5D2Lk9UV',
      sourceCallId: 'call-901f9a53-7c7c-4040-97a8-77ddb15dfcfc',
    })
  })

  it('缺 id 或非对象返回 null', () => {
    expect(permissionFromEvent(null)).toBeNull()
    expect(permissionFromEvent('x')).toBeNull()
    expect(permissionFromEvent({ action: 'read' })).toBeNull()
  })

  it('缺字段兜底不报错', () => {
    const parsed = permissionFromEvent({ id: 'per_9' })
    expect(parsed).toEqual({
      id: 'per_9',
      sessionId: '',
      action: 'unknown',
      resources: [],
      message: null,
      sourceMessageId: null,
      sourceCallId: null,
    })
  })
})

describe('permissionHeadline', () => {
  it('引擎消息优先', () => {
    expect(permissionHeadline(request({ message: '需要读取环境变量' }))).toBe('需要读取环境变量')
  })

  it('按动作映射中文', () => {
    expect(permissionHeadline(request())).toBe('读取文件需要授权')
    expect(permissionHeadline(request({ action: 'edit' }))).toBe('修改文件需要授权')
    expect(permissionHeadline(request({ action: 'external_directory' }))).toBe(
      '访问外部目录需要授权',
    )
    expect(permissionHeadline(request({ action: 'mystery' }))).toBe('执行 mystery需要授权')
  })
})

describe('permissionResources', () => {
  it('空列表给占位', () => {
    expect(permissionResources(request({ resources: [] }))).toEqual(['（未提供路径）'])
  })

  it('超长折叠', () => {
    expect(permissionResources(request({ resources: ['a', 'b', 'c', 'd', 'e'] }))).toEqual([
      'a',
      'b',
      'c',
      '…等 5 项',
    ])
  })
})

describe('mergePermissions / removePermission', () => {
  it('按 id 去重并保序', () => {
    const first = mergePermissions([], [request({ id: 'per_1' })])
    const second = mergePermissions(first, [request({ id: 'per_1' }), request({ id: 'per_2' })])
    expect(second.map(item => item.id)).toEqual(['per_1', 'per_2'])
  })

  it('无新增时保持原引用', () => {
    const current = [request({ id: 'per_1' })]
    expect(mergePermissions(current, [request({ id: 'per_1' })])).toBe(current)
  })

  it('移除命中项；无变化保持原引用', () => {
    const current = [request({ id: 'per_1' }), request({ id: 'per_2' })]
    expect(removePermission(current, 'per_1').map(item => item.id)).toEqual(['per_2'])
    expect(removePermission(current, 'per_x')).toBe(current)
  })
})

describe('repliedRequestId', () => {
  it('取 requestID 或 id', () => {
    expect(repliedRequestId({ requestID: 'per_1', reply: 'once' })).toBe('per_1')
    expect(repliedRequestId({ id: 'per_2' })).toBe('per_2')
    expect(repliedRequestId(null)).toBeNull()
  })
})
