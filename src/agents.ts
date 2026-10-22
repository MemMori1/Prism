/**
 * Agent 面板与 DAG 面板共用的展示辅助（纯函数，可单测）。
 *
 * 这里只放"怎么显示"，不放"怎么存"——存储与校验在 Rust 侧（`agents.rs`）。
 */

import type { AgentPermission, AgentProfile, AgentWorkspace } from './types'

export interface PermissionOption {
  id: AgentPermission
  label: string
  description: string
  /** 会跳过审批的档位，UI 要额外提示。 */
  risky: boolean
}

/** 与 `agents::Permission` 一一对应；顺序即 UI 展示顺序。 */
export const PERMISSION_OPTIONS: readonly PermissionOption[] = [
  {
    id: 'read-only',
    label: '只读',
    description: '只能读文件与列目录，写操作与命令直接拒绝',
    risky: false,
  },
  {
    id: 'workspace-write',
    label: '工作区可写',
    description: '只能改工作目录，写文件与跑命令仍要审批',
    risky: false,
  },
  {
    id: 'danger-full-access',
    label: '全盘可写',
    description: '全盘可写、工具调用不再询问（只给信任的仓库用）',
    risky: true,
  },
]

export const WORKSPACE_OPTIONS: readonly { id: AgentWorkspace; label: string; description: string }[] =
  [
    { id: 'shared', label: '共用目录', description: '直接在项目目录跑：只读调研、单写者串行链' },
    { id: 'worktree', label: '独立工作树', description: '每个节点一个 git worktree + 分支，可 diff' },
    { id: 'scratch', label: '临时目录', description: '空目录，产物只能通过报告带出' },
  ]

export function permissionOption(id: AgentPermission): PermissionOption {
  return PERMISSION_OPTIONS.find(option => option.id === id) ?? PERMISSION_OPTIONS[1]
}

export function permissionLabel(id: AgentPermission): string {
  return permissionOption(id).label
}

export function workspaceLabel(id: AgentWorkspace): string {
  return WORKSPACE_OPTIONS.find(option => option.id === id)?.label ?? id
}

/** 执行器徽标：`native` 显示 `原生`，其余取引擎 id 的短标记。 */
export function executorBadge(executor: string): string {
  const badges: Record<string, string> = {
    native: '原生',
    dsh: 'DSH',
    opencode: 'OC',
    codex: 'CDX',
    claude: 'CC',
  }
  return badges[executor] ?? executor.toUpperCase().slice(0, 4)
}

export function executorLabel(executor: string): string {
  const labels: Record<string, string> = {
    native: 'Prism 原生',
    dsh: 'DeepSeek Harness',
    opencode: 'OpenCode',
    codex: 'Codex',
    claude: 'Claude Code',
  }
  return labels[executor] ?? executor
}

/** 外部引擎自己管工具，Prism 只能通过会话参数影响——UI 要说清这一点。 */
export function supportsPrivateTools(executor: string): boolean {
  return executor === 'native'
}

/**
 * 模型实际会看到的工具清单（预览用）。
 *
 * 解析顺序与 Rust 侧一致：公用（null = 全部可用）→ 加私有 → 去 deny → 去重。
 * 权限档过滤在前端做不了（工具权限表在后端），所以预览是"上限"而不是"最终"。
 */
export function effectiveTools(profile: AgentProfile, available: readonly string[]): string[] {
  const deny = new Set(profile.tools.deny)
  const base = profile.tools.shared ?? [...available]
  const out: string[] = []
  for (const name of [...base, ...profile.tools.private]) {
    if (deny.has(name) || out.includes(name)) continue
    out.push(name)
  }
  return out
}

/** 新建 Agent 的草稿：权限取最保守的一档，避免"新建即全盘可写"。 */
export function emptyAgentDraft(id = '', executor = 'native'): AgentProfile {
  return {
    id,
    name: '',
    description: '',
    origin: 'user',
    overridesBuiltin: false,
    executor,
    model: null,
    systemPrompt: '',
    permission: 'workspace-write',
    workspace: 'shared',
    tools: { shared: null, private: [], deny: [] },
    mcp: [],
    tags: [],
    linkDirs: [],
    gateDefault: false,
  }
}

/** 卡片角标：这个 Agent 覆盖了内置同名 preset。 */
export function originLabel(profile: AgentProfile): string {
  if (profile.origin === 'builtin') return '内置'
  return profile.overridesBuiltin ? '覆盖内置' : '自定义'
}

export interface AgentFilter {
  origin: 'all' | 'builtin' | 'user'
  term: string
  tag: string | null
}

export function filterAgents(list: readonly AgentProfile[], filter: AgentFilter): AgentProfile[] {
  const term = filter.term.trim().toLowerCase()
  return list.filter(profile => {
    if (filter.origin !== 'all' && profile.origin !== filter.origin) return false
    if (filter.tag && !profile.tags.includes(filter.tag)) return false
    if (!term) return true
    return (
      profile.name.toLowerCase().includes(term) ||
      profile.id.toLowerCase().includes(term) ||
      profile.description.toLowerCase().includes(term) ||
      profile.tags.some(tag => tag.toLowerCase().includes(term))
    )
  })
}

/** 面板左栏的分组：先内置（保持 preset 顺序），再自定义。 */
export function agentSections(list: readonly AgentProfile[]): {
  builtin: AgentProfile[]
  user: AgentProfile[]
} {
  return {
    builtin: list.filter(profile => profile.origin === 'builtin'),
    user: list.filter(profile => profile.origin === 'user'),
  }
}

/** 所有出现过的标签（稳定顺序，供筛选条用）。 */
export function agentTags(list: readonly AgentProfile[]): string[] {
  const out: string[] = []
  for (const profile of list) {
    for (const tag of profile.tags) {
      if (!out.includes(tag)) out.push(tag)
    }
  }
  return out
}

/** 拆逗号分隔的输入（标签 / 私有工具），去空去重。 */
export function splitList(raw: string): string[] {
  const out: string[] = []
  for (const piece of raw.split(/[,，\s]+/)) {
    const value = piece.trim()
    if (value && !out.includes(value)) out.push(value)
  }
  return out
}
