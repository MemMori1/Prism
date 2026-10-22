import { describe, expect, it } from 'vitest'
import type { DagNodeRun, DagRun } from '../src/types'
import {
  isLive,
  nodeElapsed,
  nodeMeta,
  nodeRunOf,
  runBlocker,
  runElapsed,
  runProgress,
  runSummary,
  waitingNodes,
} from '../src/dagRun'

function node(id: string, status: DagNodeRun['status'], extra: Partial<DagNodeRun> = {}): DagNodeRun {
  return {
    id,
    title: id,
    agent: 'general',
    status,
    attempt: status === 'pending' ? 0 : 1,
    startedAt: null,
    finishedAt: null,
    output: '',
    error: null,
    tokensIn: 0,
    tokensOut: 0,
    steps: [],
    ...extra,
  }
}

function run(nodes: DagNodeRun[], extra: Partial<DagRun> = {}): DagRun {
  return {
    id: 'run-1',
    planId: 'dag-1',
    planName: '测试图',
    project: '/tmp/p',
    status: 'running',
    startedAt: 1_000,
    finishedAt: null,
    message: null,
    nodes,
    ...extra,
  }
}

describe('DAG 运行态展示', () => {
  it('进度只数落定的节点', () => {
    const progress = runProgress(
      run([
        node('a', 'succeeded'),
        node('b', 'failed'),
        node('c', 'skipped'),
        node('d', 'running'),
        node('e', 'pending'),
      ]),
    )
    expect(progress.total).toBe(5)
    expect(progress.settled).toBe(3)
    expect(progress.percent).toBe(60)
    expect(progress.ok).toBe(1)
    expect(progress.failed).toBe(1)
    expect(progress.skipped).toBe(1)
  })

  it('空运行不除零', () => {
    const progress = runProgress(run([]))
    expect(progress).toMatchObject({ total: 0, settled: 0, percent: 0 })
    expect(runProgress(null).percent).toBe(0)
  })

  it('运行中的节点耗时算到当下，没开始的是 0', () => {
    const live = node('a', 'running', { startedAt: 2_000 })
    expect(nodeElapsed(live, 5_000)).toBe(3_000)
    const done = node('b', 'succeeded', { startedAt: 2_000, finishedAt: 3_500 })
    expect(nodeElapsed(done, 9_999)).toBe(1_500)
    expect(nodeElapsed(node('c', 'pending'), 9_999)).toBe(0)
  })

  it('整场耗时在结束后冻结', () => {
    expect(runElapsed(run([]), 4_000)).toBe(3_000)
    expect(runElapsed(run([], { finishedAt: 2_500 }), 9_999)).toBe(1_500)
  })

  it('只有卡在闸门上的节点会被列出来', () => {
    const list = run([node('a', 'waiting'), node('b', 'running'), node('c', 'waiting')])
    expect(waitingNodes(list).map(item => item.id)).toEqual(['a', 'c'])
    expect(waitingNodes(null)).toEqual([])
  })

  it('运行中的计划拦不住第二次启动', () => {
    expect(isLive(run([]))).toBe(true)
    expect(isLive(run([], { status: 'succeeded' }))).toBe(false)
  })

  it('点不动「运行」时说得出原因，且脏图优先于别的原因', () => {
    const base = { hasPlan: true, dirty: false, errors: 0, live: false, nodeCount: 3 }
    expect(runBlocker(base)).toBeNull()
    expect(runBlocker({ ...base, dirty: true, errors: 2 })).toContain('先保存')
    expect(runBlocker({ ...base, errors: 2 })).toContain('2 个问题')
    expect(runBlocker({ ...base, nodeCount: 0 })).toContain('没有节点')
    expect(runBlocker({ ...base, hasPlan: false })).toContain('先打开')
    expect(runBlocker({ ...base, live: true })).toContain('正在跑')
  })

  it('每个状态都有文案与短记号', () => {
    for (const status of ['pending', 'waiting', 'running', 'succeeded', 'failed', 'skipped', 'cancelled'] as const) {
      const meta = nodeMeta(status)
      expect(meta.label).not.toBe('')
      expect(meta.glyph.length).toBe(1)
    }
  })

  it('收尾摘要带上计数', () => {
    const summary = runSummary(
      run([node('a', 'succeeded'), node('b', 'failed'), node('c', 'skipped')]),
    )
    expect(summary).toContain('3/3')
    expect(summary).toContain('1 失败')
    expect(summary).toContain('1 跳过')
  })

  it('按节点 id 找运行记录，找不到给 null', () => {
    const snapshot = run([node('a', 'succeeded')])
    expect(nodeRunOf(snapshot, 'a')?.status).toBe('succeeded')
    expect(nodeRunOf(snapshot, 'zzz')).toBeNull()
    expect(nodeRunOf(null, 'a')).toBeNull()
  })
})
