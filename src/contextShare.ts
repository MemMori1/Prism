/**
 * 上下文明细：把「这一轮用了多少 token」拆成看得懂的几块。
 *
 * 引擎只给总数，说不出各块多少；这里用**字符数**当权重把它分摊下去：
 * 系统提示 / 工具定义 / 工具结果 / 对话，四项加起来正好等于总数。
 * 所以面板上必须写明「分项按字符估算」——比例是估的，总数是真的。
 */

export interface ContextChars {
  /** 系统提示词（含工作目录与规则）。 */
  system: number
  /** 工具声明（发给模型的 function schema）。 */
  tools: number
  /** 工具结果（读到的文件内容、命令输出）。 */
  tool: number
  /** 用户 + 助手的文字。 */
  dialog: number
}

export interface ContextSlice {
  key: string
  label: string
  /** 这一项的字符数（分摊依据）。 */
  chars: number
  /** 分摊到的 token。 */
  tokens: number
  /** 占已用量的比例（0–1），用来画条。 */
  share: number
  /** 占窗口的百分比（可能为 null：窗口未知）。 */
  percent: number | null
}

/** 一项明细的显示顺序：从「模型固定开销」到「你自己的内容」。 */
const ORDER: Array<{ key: string; label: string }> = [
  { key: 'system', label: '系统提示词' },
  { key: 'tools', label: '工具定义' },
  { key: 'tool', label: '工具结果' },
  { key: 'dialog', label: '对话消息' },
]

function round1(value: number): number {
  return Math.round(value * 10) / 10
}

/**
 * 字符数 → 明细。
 *
 * `used` 是引擎报的**真实**总用量，四项按字符比例分它；字符全为 0 时返回空数组
 * （还没发过消息，面板不该编数字）。
 */
export function contextBreakdown(
  used: number,
  window: number | null,
  chars: ContextChars,
): ContextSlice[] {
  const total = chars.system + chars.tools + chars.tool + chars.dialog
  if (!total || !used) return []
  return ORDER.map(item => {
    const size = chars[item.key as keyof ContextChars] ?? 0
    const share = size / total
    const tokens = Math.round(used * share)
    return {
      key: item.key,
      label: item.label,
      chars: size,
      tokens,
      share,
      percent: window && window > 0 ? round1((tokens / window) * 100) : null,
    }
  })
}

/**
 * 没有引擎统计时的兜底：按时间线里的文字长度估。
 *
 * 工具卡只带一句 `summary`（没有完整输出），所以「工具结果」会被低估、
 * 「对话消息」相对偏高——面板上会标成估算。
 */
export function charsOfRecords(
  records: Array<{ role: string; parts: Array<{ kind: string; text?: string; summary?: string | null }> }>,
): ContextChars {
  const chars: ContextChars = { system: 0, tools: 0, tool: 0, dialog: 0 }
  for (const record of records) {
    for (const part of record.parts) {
      const text = part.text ?? part.summary ?? ''
      if (part.kind === 'tool') chars.tool += text.length
      else if (record.role === 'user' || record.role === 'assistant') chars.dialog += text.length
    }
  }
  return chars
}

/** 明细列表的 token 合计（用于和引擎总数对账）。 */
export function sumSlices(slices: ContextSlice[]): number {
  return slices.reduce((total, slice) => total + slice.tokens, 0)
}

/**
 * 百分比的统一写法：一位小数，整数就不带 `.0`（`8%` / `8.1%`）。
 *
 * 环上和面板里必须是同一套写法——一个写 8%、另一个写 8.1%，看着像两处数据打架。
 */
export function formatPercent(value: number): string {
  if (!Number.isFinite(value)) return '—'
  // `8` 会序列化成 "8"，`8.1` 就是 "8.1"——不用再判断整数。
  return `${Math.round(value * 10) / 10}%`
}
