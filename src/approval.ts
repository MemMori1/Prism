/**
 * 权限档位（原生引擎的「审批模式」）。
 *
 * 三档，和 Codex 那套对应：只读 / 每次询问 / 完全访问。
 * 档位是**每个会话**记的（localStorage），发送时随消息带给后端，由后端执行闸门——
 * 界面只是开关，真正拦人的地方在 Rust 那边。
 */

export type ApprovalMode = 'ask' | 'read-only' | 'full'

export interface ApprovalChoice {
  id: ApprovalMode
  /** 下拉里的一行标题。 */
  label: string
  /** 收起态显示的字（越短越好）。 */
  short: string
  /** 一句话说明，悬停和下拉里都用。 */
  hint: string
}

export const APPROVAL_CHOICES: ApprovalChoice[] = [
  {
    id: 'read-only',
    label: '只读',
    short: '只读',
    hint: '只能读文件、列目录、搜索；写文件 / 改文件 / 跑命令一律拒绝',
  },
  {
    id: 'ask',
    label: '每次询问',
    short: '询问',
    hint: '读随便；写文件、改文件、跑命令之前先问你（默认）',
  },
  {
    id: 'full',
    label: '完全访问',
    short: '全放行',
    hint: '不再询问，写文件和跑命令直接执行——只在你信得过的时候用',
  },
]

export const DEFAULT_APPROVAL: ApprovalMode = 'ask'

/** 认不出来一律回落到「每次询问」：宁可多问一句，也不能默认放行。 */
export function parseApproval(raw: unknown): ApprovalMode {
  return raw === 'read-only' || raw === 'full' || raw === 'ask' ? raw : DEFAULT_APPROVAL
}

export function approvalChoice(mode: ApprovalMode): ApprovalChoice {
  return APPROVAL_CHOICES.find(item => item.id === mode) ?? APPROVAL_CHOICES[1]
}

const KEY_PREFIX = 'prism-approval:'
const KEY_DEFAULT = 'prism-approval-default'

/** localStorage 在无窗环境（node 单测）里没有：拿不到就当没存过。 */
function storage(): Storage | null {
  try {
    if (typeof window === 'undefined') return null
    return window.localStorage ?? null
  } catch {
    return null
  }
}

/** 这个会话上次用的档位；没有就用全局默认，再没有就是「每次询问」。 */
export function loadApproval(sessionId: string | null | undefined): ApprovalMode {
  const store = storage()
  if (!store) return DEFAULT_APPROVAL
  if (sessionId) {
    const saved = store.getItem(`${KEY_PREFIX}${sessionId}`)
    if (saved) return parseApproval(saved)
  }
  return parseApproval(store.getItem(KEY_DEFAULT))
}

/** 记档位：会话级 + 全局默认（新会话沿用上次选的）。 */
export function saveApproval(sessionId: string | null | undefined, mode: ApprovalMode): void {
  const store = storage()
  if (!store) return
  if (sessionId) store.setItem(`${KEY_PREFIX}${sessionId}`, mode)
  store.setItem(KEY_DEFAULT, mode)
}

/**
 * 这个档位会不会真的拦人——只有「只读」和「询问」需要界面上提醒。
 */
export function approvalIsRestrictive(mode: ApprovalMode): boolean {
  return mode !== 'full'
}
