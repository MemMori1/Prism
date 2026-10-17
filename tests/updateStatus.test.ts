import { describe, expect, it } from 'vitest'
import { friendlyUpdateError } from '../src/updateStatus'

describe('friendlyUpdateError', () => {
  it('空文本 → 未知错误', () => {
    expect(friendlyUpdateError('   ')).toBe('未知错误')
  })

  it('404 / 无有效 release → 发布源暂无版本', () => {
    expect(
      friendlyUpdateError('Could not fetch a valid release JSON from the remote. Received status code: 404'),
    ).toContain('发布源暂无版本')
    expect(friendlyUpdateError('HTTP 404')).toContain('发布源暂无版本')
  })

  it('网络错误 → 网络不可达', () => {
    expect(friendlyUpdateError('error sending request for url (https://github.com/...)')).toContain(
      '网络不可达',
    )
    expect(friendlyUpdateError('operation timed out')).toContain('网络不可达')
  })

  it('签名错误 → 签名校验失败', () => {
    expect(friendlyUpdateError('signature verification failed')).toContain('签名校验失败')
  })

  it('其他错误原样返回，超长截断', () => {
    expect(friendlyUpdateError('boom')).toBe('boom')
    expect(friendlyUpdateError('x'.repeat(200)).length).toBe(121)
  })
})
