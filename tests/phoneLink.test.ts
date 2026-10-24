import { describe, expect, it } from 'vitest'
import {
  pairingLeftSeconds,
  qrAvailable,
  statusLabel,
  statusTone,
  webhookPlaceholder,
} from '../src/phoneLink'
import type { PhoneLinkStatus } from '../src/types'

function make(overrides: Partial<PhoneLinkStatus> = {}): PhoneLinkStatus {
  return {
    enabled: true,
    engine: 'opencode',
    serverRunning: true,
    port: 15211,
    lanIp: '192.168.1.5',
    pairing: { expiresAt: 1_000_000 },
    qrSvg: '<svg/>',
    pairUrl: 'http://192.168.1.5:15211/p/abc',
    device: null,
    webhookConfigured: false,
    webhookHint: null,
    events: { onDone: true, onPermission: true, onError: true },
    chatRunning: false,
    sessionId: null,
    ...overrides,
  }
}

describe('statusLabel / statusTone', () => {
  it('未加载 → 占位', () => {
    expect(statusLabel(null)).toBe('…')
    expect(statusTone(null)).toBe('muted')
  })

  it('未启用 → 灰', () => {
    expect(statusLabel(make({ enabled: false }))).toBe('未启用')
    expect(statusTone(make({ enabled: false }))).toBe('muted')
  })

  it('启用但服务没起来 → 红', () => {
    expect(statusLabel(make({ serverRunning: false }))).toBe('启动失败（看日志）')
    expect(statusTone(make({ serverRunning: false }))).toBe('error')
  })

  it('已配对 → 绿，带设备名', () => {
    const status = make({ device: { name: '手机', pairedAt: 1, lastSeenAt: 2 } })
    expect(statusLabel(status)).toBe('已连接 · 手机')
    expect(statusTone(status)).toBe('ok')
  })

  it('没配对 → 黄，等扫码', () => {
    expect(statusLabel(make())).toBe('等待扫码')
    expect(statusTone(make())).toBe('warn')
  })
})

describe('webhookPlaceholder', () => {
  it('未配置给完整示例，已配置只回显尾 4 位', () => {
    expect(webhookPlaceholder(make())).toContain('qyapi.weixin.qq.com')
    expect(webhookPlaceholder(make({ webhookConfigured: true, webhookHint: '…7d6c' }))).toBe(
      '已保存 …7d6c（留空沿用）',
    )
  })
})

describe('pairingLeftSeconds / qrAvailable', () => {
  const now = 900_000

  it('按过期时刻算剩余秒数', () => {
    expect(pairingLeftSeconds(make(), now)).toBe(100)
    expect(pairingLeftSeconds(make({ pairing: { expiresAt: now - 1 } }), now)).toBe(0)
    expect(pairingLeftSeconds(make({ pairing: null }), now)).toBe(0)
  })

  it('二维码要四样齐全才显示', () => {
    expect(qrAvailable(make(), now)).toBe(true)
    expect(qrAvailable(make({ enabled: false }), now)).toBe(false)
    expect(qrAvailable(make({ serverRunning: false }), now)).toBe(false)
    expect(qrAvailable(make({ qrSvg: null }), now)).toBe(false)
    expect(qrAvailable(make({ pairing: { expiresAt: now - 1 } }), now)).toBe(false)
  })
})
