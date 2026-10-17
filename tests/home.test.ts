import { describe, expect, it } from 'vitest'
import { buildHeatmap, heatmapColumns, parseDayKey, pulseStats, topModels } from '../src/home'
import type { TokenTotals, UsageDaily, UsageEngine } from '../src/types'

function tokens(input: number, output = 0): TokenTotals {
  return { input, cacheRead: 0, cacheWrite: 0, output, reasoning: 0 }
}

function day(date: string, input: number): UsageDaily {
  return { date, tokens: tokens(input) }
}

describe('parseDayKey', () => {
  it('按本地日期解析（不因 UTC 偏移错位）', () => {
    const date = parseDayKey('2026-10-17')
    expect(date.getFullYear()).toBe(2026)
    expect(date.getMonth()).toBe(9)
    expect(date.getDate()).toBe(17)
  })
})

describe('buildHeatmap', () => {
  it('取最近 N 天并按峰值分 0–4 档', () => {
    const daily = [
      day('2026-10-01', 0),
      day('2026-10-02', 100),
      day('2026-10-03', 200),
      day('2026-10-04', 300),
      day('2026-10-05', 400),
    ]
    const cells = buildHeatmap(daily, 4)
    expect(cells.map(cell => cell.date)).toEqual([
      '2026-10-02',
      '2026-10-03',
      '2026-10-04',
      '2026-10-05',
    ])
    expect(cells.map(cell => cell.level)).toEqual([1, 2, 3, 4])
    expect(cells[0].tokens).toBe(100)
  })
})

describe('heatmapColumns', () => {
  it('列按周对齐、行对齐周一…周日', () => {
    // 2026-10-12 是周一。
    const cells = buildHeatmap(
      [
        day('2026-10-11', 10), // 周日
        day('2026-10-12', 10),
        day('2026-10-13', 10),
      ],
      3,
    )
    const columns = heatmapColumns(cells)
    expect(columns.length).toBe(2, '周日落在第一列，其余进第二列')
    expect(columns[0][6]?.date).toBe('2026-10-11', '周日 = 第 7 行')
    expect(columns[1][0]?.date).toBe('2026-10-12')
    expect(columns[1][1]?.date).toBe('2026-10-13')
    expect(columns[1][2]).toBeNull()
  })

  it('空数据返回空网格', () => {
    expect(heatmapColumns([])).toEqual([])
  })
})

describe('pulseStats', () => {
  it('今日 / 昨日 / 周合计 / 峰值日', () => {
    const daily = [
      day('2026-10-14', 100),
      day('2026-10-15', 300),
      day('2026-10-16', 200),
      day('2026-10-17', 400),
    ]
    const stats = pulseStats(daily)
    expect(stats.today).toBe(400)
    expect(stats.yesterday).toBe(200)
    expect(stats.deltaPercent).toBeCloseTo(100)
    expect(stats.week).toBe(1000)
    expect(stats.best?.date).toBe('2026-10-17')
  })

  it('昨日为 0 时增减为 null；全 0 无峰值日', () => {
    const stats = pulseStats([day('2026-10-16', 0), day('2026-10-17', 500)])
    expect(stats.deltaPercent).toBeNull()
    expect(pulseStats([day('2026-10-17', 0)]).best).toBeNull()
  })
})

describe('topModels', () => {
  it('按 token 倒序、比例分母为全量', () => {
    const engines: UsageEngine[] = [
      {
        engine: 'opencode',
        label: 'OpenCode',
        tokens: tokens(100),
        sessions: 1,
        costUsd: 0,
        unpricedModels: [],
        models: [
          { model: 'big', tokens: tokens(300), sessions: 1, costUsd: 1 },
          { model: 'small', tokens: tokens(100), sessions: 1, costUsd: 0 },
        ],
      },
      {
        engine: 'codex',
        label: 'Codex',
        tokens: tokens(100),
        sessions: 1,
        costUsd: 0,
        unpricedModels: [],
        models: [{ model: 'mid', tokens: tokens(200), sessions: 1, costUsd: null }],
      },
    ]
    const models = topModels(engines, 2)
    expect(models.map(item => item.model)).toEqual(['big', 'mid'])
    expect(models[0].share).toBeCloseTo(300 / 600)
    expect(models[1].engineLabel).toBe('Codex')
    expect(models[1].costUsd).toBeNull()
  })
})
