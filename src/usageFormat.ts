// 用量面板的纯展示助手（独立成文件便于单测）。
import type { TokenTotals } from './types'

const compact = new Intl.NumberFormat('en', { notation: 'compact', maximumFractionDigits: 1 })

/** token 数紧凑显示：1500 → 1.5K；2500000 → 2.5M。 */
export function formatTokens(value: number): string {
  const count = Math.max(0, Math.round(value))
  return compact.format(count)
}

/** 成本显示：null → 未收录；0 → 免费；极小值 → <$0.01。 */
export function formatCost(usd: number | null): string {
  if (usd === null) return '未收录'
  if (usd === 0) return '免费'
  if (usd < 0.01) return '<$0.01'
  return `$${usd.toFixed(2)}`
}

/** token 合计（各列口径互不重叠，可直接相加）。 */
export function totalTokens(tokens: TokenTotals): number {
  return tokens.input + tokens.output + tokens.reasoning + tokens.cacheRead + tokens.cacheWrite
}

/** 每日柱高度（0–100，相对峰值；零值 2% 占位，非零保底 3%）。 */
export function barHeights(values: number[]): number[] {
  const peak = Math.max(0, ...values)
  return values.map(value => {
    if (peak <= 0 || value <= 0) return 2
    return Math.max(3, (value / peak) * 100)
  })
}
