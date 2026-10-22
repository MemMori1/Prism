import type { DagNodeRun, DagNodeStatus, DagRun, DagRunStatus } from './types'

/**
 * 运行态的展示层助手。
 *
 * 只做「快照 → 文案/数值」的纯计算，不碰网络也不碰时间源（当前时间由调用方
 * 传进来）。这样面板可以安心每秒重算一次进度条，也方便单测。
 */

export interface StatusMeta {
  label: string
  /** 语义色调：给样式表挑颜色用，不是具体颜色值。 */
  tone: 'idle' | 'active' | 'ok' | 'warn' | 'bad'
  /** 画布节点上的短记号（字符宽度要小，别把标题挤走）。 */
  glyph: string
}

const NODE_META: Record<DagNodeStatus, StatusMeta> = {
  pending: { label: '等待', tone: 'idle', glyph: '·' },
  waiting: { label: '待确认', tone: 'warn', glyph: '!' },
  running: { label: '运行中', tone: 'active', glyph: '▶' },
  succeeded: { label: '成功', tone: 'ok', glyph: '✓' },
  failed: { label: '失败', tone: 'bad', glyph: '×' },
  skipped: { label: '已跳过', tone: 'idle', glyph: '–' },
  cancelled: { label: '已取消', tone: 'idle', glyph: '–' },
}

const RUN_META: Record<DagRunStatus, StatusMeta> = {
  running: { label: '运行中', tone: 'active', glyph: '▶' },
  succeeded: { label: '已完成', tone: 'ok', glyph: '✓' },
  failed: { label: '有节点失败', tone: 'bad', glyph: '×' },
  cancelled: { label: '已停止', tone: 'idle', glyph: '–' },
}

export function nodeMeta(status: DagNodeStatus): StatusMeta {
  return NODE_META[status] ?? NODE_META.pending
}

export function runMeta(status: DagRunStatus): StatusMeta {
  return RUN_META[status] ?? RUN_META.running
}

/** 这条状态算不算"已经落定"（进度条的分母用）。 */
export function isSettled(status: DagNodeStatus): boolean {
  return status === 'succeeded' || status === 'failed' || status === 'skipped' || status === 'cancelled'
}

export interface RunProgress {
  settled: number
  total: number
  /** 0–100，四舍五入到整数：进度条不需要小数。 */
  percent: number
  ok: number
  failed: number
  skipped: number
  running: number
  waiting: number
  cancelled: number
}

export function runProgress(run: DagRun | null): RunProgress {
  const nodes = run?.nodes ?? []
  const count = (status: DagNodeStatus): number =>
    nodes.filter(node => node.status === status).length
  const settled = nodes.filter(node => isSettled(node.status)).length
  return {
    settled,
    total: nodes.length,
    percent: nodes.length ? Math.round((settled / nodes.length) * 100) : 0,
    ok: count('succeeded'),
    failed: count('failed'),
    skipped: count('skipped'),
    running: count('running'),
    waiting: count('waiting'),
    cancelled: count('cancelled'),
  }
}

/** 节点耗时：还在跑就算到 `now`，没开始返回 0（调用方据此不显示）。 */
export function nodeElapsed(node: DagNodeRun, now: number): number {
  if (!node.startedAt) return 0
  return Math.max(0, (node.finishedAt ?? now) - node.startedAt)
}

export function runElapsed(run: DagRun, now: number): number {
  return Math.max(0, (run.finishedAt ?? now) - run.startedAt)
}

/** 卡在人工闸门上、等人点的那几个节点（面板顶部要突出显示）。 */
export function waitingNodes(run: DagRun | null): DagNodeRun[] {
  return run?.nodes.filter(node => node.status === 'waiting') ?? []
}

export function nodeRunOf(run: DagRun | null, nodeId: string): DagNodeRun | null {
  return run?.nodes.find(node => node.id === nodeId) ?? null
}

/** 运行是否还在进行——按钮的"运行 / 停止"就看它。 */
export function isLive(run: DagRun | null): boolean {
  return run?.status === 'running'
}

/**
 * 能不能点「运行」。
 *
 * 三个否决项各自有话说，所以返回原因而不是布尔：按钮的 tooltip 直接显示它，
 * 用户不用猜为什么点不动。
 */
export function runBlocker(input: {
  hasPlan: boolean
  dirty: boolean
  errors: number
  live: boolean
  nodeCount: number
}): string | null {
  if (!input.hasPlan) return '先打开一张计划'
  if (input.dirty) return '先保存计划：跑的是磁盘上那份'
  if (input.errors > 0) return `有 ${input.errors} 个问题要先修掉`
  if (input.nodeCount === 0) return '图里还没有节点'
  if (input.live) return '这张计划正在跑'
  return null
}

/** 收尾一句话：状态 + 计数，给运行条右侧用。 */
export function runSummary(run: DagRun): string {
  const progress = runProgress(run)
  const parts: string[] = [`${progress.settled}/${progress.total}`]
  if (progress.failed) parts.push(`${progress.failed} 失败`)
  if (progress.skipped) parts.push(`${progress.skipped} 跳过`)
  if (progress.cancelled) parts.push(`${progress.cancelled} 取消`)
  return parts.join(' · ')
}
