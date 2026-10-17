import { describe, expect, it } from 'vitest'
import { mergeTrayCounts } from '../src/trayStatus'

describe('mergeTrayCounts', () => {
  const empty = { running: 0, waiting: 0, runningIds: [], waitingIds: [] }

  it('两处都空闲 → 全零', () => {
    expect(mergeTrayCounts(empty, { running: false, waiting: 0 }, 's1')).toEqual({
      running: 0,
      waiting: 0,
    })
  })

  it('归档与工作台是不同会话 → 相加', () => {
    const wb = { running: 1, waiting: 0, runningIds: ['a'], waitingIds: [] }
    expect(mergeTrayCounts(wb, { running: true, waiting: 2 }, 'b')).toEqual({
      running: 2,
      waiting: 2,
    })
  })

  it('同一会话同时在两处 → 去重不虚高', () => {
    const wb = { running: 1, waiting: 2, runningIds: ['a'], waitingIds: ['a'] }
    expect(mergeTrayCounts(wb, { running: true, waiting: 2 }, 'a')).toEqual({
      running: 1,
      waiting: 2,
    })
  })

  it('未选中会话 → 忽略归档侧', () => {
    const wb = { running: 1, waiting: 1, runningIds: ['a'], waitingIds: ['a'] }
    expect(mergeTrayCounts(wb, { running: true, waiting: 3 }, null)).toEqual({
      running: 1,
      waiting: 1,
    })
  })
})
