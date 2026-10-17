// 首页仪表盘的纯展示助手（独立成文件便于单测）。
import type { UsageDaily, UsageEngine } from './types'
import { totalTokens } from './usageFormat'

export interface HeatCell {
  /** YYYY-MM-DD */
  date: string
  tokens: number
  /** 0–4 强度档（0 = 无用量）。 */
  level: number
}

/** 解析 YYYY-MM-DD 为本地日期（直接 new Date(key) 会按 UTC 解析，跨时区会错位）。 */
export function parseDayKey(key: string): Date {
  const [year, month, day] = key.split('-').map(Number)
  return new Date(year, (month ?? 1) - 1, day ?? 1)
}

function levelFor(value: number, peak: number): number {
  if (value <= 0 || peak <= 0) return 0
  const ratio = value / peak
  if (ratio <= 0.25) return 1
  if (ratio <= 0.5) return 2
  if (ratio <= 0.75) return 3
  return 4
}

/** 最近 N 天的热力图数据（升序，最后一个通常是今天）。 */
export function buildHeatmap(daily: UsageDaily[], days = 28): HeatCell[] {
  const slice = daily.slice(-Math.max(1, days))
  const values = slice.map(entry => totalTokens(entry.tokens))
  const peak = Math.max(0, ...values)
  return slice.map((entry, index) => ({
    date: entry.date,
    tokens: values[index],
    level: levelFor(values[index], peak),
  }))
}

/** 热力图网格：每列一周、每行周一…周日；放不满的位置为 null。 */
export function heatmapColumns(cells: HeatCell[]): Array<Array<HeatCell | null>> {
  if (!cells.length) return []
  const first = parseDayKey(cells[0].date)
  // getDay(): 0=周日 → 归一到 0=周一。
  const pad = (first.getDay() + 6) % 7
  const columnCount = Math.ceil((pad + cells.length) / 7)
  const columns: Array<Array<HeatCell | null>> = Array.from({ length: columnCount }, () =>
    Array.from({ length: 7 }, () => null as HeatCell | null),
  )
  cells.forEach((cell, index) => {
    const slot = pad + index
    columns[Math.floor(slot / 7)][slot % 7] = cell
  })
  return columns
}

export interface PulseStats {
  today: number
  yesterday: number
  /** 今日相对昨日的增减（%）；昨日为 0 或没有昨日数据时为 null。 */
  deltaPercent: number | null
  /** 最近 7 天合计。 */
  week: number
  /** 窗口内用量最高的一天（全 0 时为 null）。 */
  best: HeatCell | null
}

export function pulseStats(daily: UsageDaily[]): PulseStats {
  const values = daily.map(entry => totalTokens(entry.tokens))
  const today = values.length ? values[values.length - 1] : 0
  const yesterday = values.length > 1 ? values[values.length - 2] : 0
  const week = values.slice(-7).reduce((sum, value) => sum + value, 0)
  let best: HeatCell | null = null
  daily.forEach((entry, index) => {
    if (values[index] <= 0) return
    if (!best || values[index] > best.tokens) {
      best = { date: entry.date, tokens: values[index], level: 0 }
    }
  })
  return {
    today,
    yesterday,
    deltaPercent: yesterday > 0 ? ((today - yesterday) / yesterday) * 100 : null,
    week,
    best,
  }
}

export interface ModelShare {
  model: string
  engine: string
  engineLabel: string
  tokens: number
  /** 占全部模型 token 的比例（0–1）。 */
  share: number
  costUsd: number | null
}

/** 跨引擎的模型分布（按 token 倒序取前 limit 个；比例的分母是全量）。 */
export function topModels(engines: UsageEngine[], limit = 5): ModelShare[] {
  const all = engines.flatMap(engine =>
    engine.models.map(item => ({
      model: item.model,
      engine: engine.engine,
      engineLabel: engine.label,
      tokens: totalTokens(item.tokens),
      costUsd: item.costUsd,
    })),
  )
  const total = all.reduce((sum, item) => sum + item.tokens, 0)
  return all
    .sort((a, b) => b.tokens - a.tokens)
    .slice(0, Math.max(1, limit))
    .map(item => ({ ...item, share: total > 0 ? item.tokens / total : 0 }))
}
