import { describe, expect, it } from 'vitest'
import {
  TASK_TAIL_LIMIT,
  appendTaskLine,
  dockTasks,
  filterTasks,
  formatElapsed,
  groupTasks,
  mergeTask,
  taskCounts,
  taskElapsed,
  taskExitLabel,
} from '../src/taskModel'
import type { BackgroundTask } from '../src/types'

function task(patch: Partial<BackgroundTask> = {}): BackgroundTask {
  return {
    id: 't1',
    cmd: 'pnpm build',
    cwd: '/tmp/prism',
    state: 'running',
    startedAt: 1_000,
    endedAt: null,
    exitCode: null,
    sessionId: 's1',
    title: null,
    background: true,
    origin: 'user',
    tail: [],
    ...patch,
  }
}

describe('taskCounts', () => {
  it('分开数运行中 / 已结束 / 失败', () => {
    const counts = taskCounts([
      task({ id: 'a' }),
      task({ id: 'b', state: 'running' }),
      task({ id: 'c', state: 'done', endedAt: 2_000, exitCode: 0 }),
      task({ id: 'd', state: 'failed', endedAt: 2_000, exitCode: 1 }),
      task({ id: 'e', state: 'stopped', endedAt: 2_000 }),
    ])
    expect(counts).toEqual({ running: 2, ended: 3, failed: 1 })
  })

  it('空列表是全零', () => {
    expect(taskCounts([])).toEqual({ running: 0, ended: 0, failed: 0 })
  })
})

describe('dockTasks / filterTasks', () => {
  it('前台终端任务不进任务坞', () => {
    const list = [task({ id: 'bg' }), task({ id: 'fg', background: false })]
    expect(dockTasks(list).map(item => item.id)).toEqual(['bg'])
  })

  it('Agent 起的命令只在跑着的时候进坞', () => {
    const list = [
      task({ id: 'user' }),
      task({ id: 'agent-live', origin: 'agent' }),
      task({ id: 'agent-done', origin: 'agent', state: 'done', endedAt: 2_000 }),
    ]
    expect(dockTasks(list).map(item => item.id)).toEqual(['user', 'agent-live'])
  })

  it('运行中 / 已结束筛选', () => {
    const list = [
      task({ id: 'a' }),
      task({ id: 'b', state: 'done', endedAt: 2_000 }),
      task({ id: 'c', state: 'failed', endedAt: 2_000 }),
    ]
    expect(filterTasks(list, 'running').map(item => item.id)).toEqual(['a'])
    expect(filterTasks(list, 'ended').map(item => item.id)).toEqual(['b', 'c'])
    expect(filterTasks(list, 'all')).toHaveLength(3)
  })
})

describe('mergeTask', () => {
  it('新任务按开始时间倒序插入', () => {
    const older = task({ id: 'old', startedAt: 1_000 })
    const newer = task({ id: 'new', startedAt: 5_000 })
    const list = mergeTask([older], newer)
    expect(list.map(item => item.id)).toEqual(['new', 'old'])
  })

  it('状态事件比输出晚到时不吞掉尾巴', () => {
    const live = task({ tail: ['$ pnpm build', 'a', 'b', 'c'] })
    const snapshot = task({ state: 'done', endedAt: 2_000, exitCode: 0, tail: ['$ pnpm build', 'a'] })
    const [merged] = mergeTask([live], snapshot)
    expect(merged.state).toBe('done')
    expect(merged.tail).toEqual(['$ pnpm build', 'a', 'b', 'c'])
  })

  it('快照带更长的 tail 时用快照（刷新 / 重启后的补全）', () => {
    const live = task({ tail: ['a'] })
    const snapshot = task({ tail: ['a', 'b', 'c'] })
    const [merged] = mergeTask([live], snapshot)
    expect(merged.tail).toEqual(['a', 'b', 'c'])
  })
})

describe('appendTaskLine', () => {
  it('追加到对应任务', () => {
    const list = [task({ id: 'a', tail: ['x'] }), task({ id: 'b', tail: [] })]
    const next = appendTaskLine(list, { id: 'a', line: 'y', stream: 'stdout' })
    expect(next[0].tail).toEqual(['x', 'y'])
    expect(next[1].tail).toEqual([])
  })

  it('超过上限只留最后几行', () => {
    const tail = Array.from({ length: TASK_TAIL_LIMIT }, (_, index) => `line-${index}`)
    const [next] = appendTaskLine([task({ tail })], {
      id: 't1',
      line: 'last',
      stream: 'stdout',
    })
    expect(next.tail).toHaveLength(TASK_TAIL_LIMIT)
    expect(next.tail[0]).toBe('line-1')
    expect(next.tail.at(-1)).toBe('last')
  })

  it('未知任务（迟到的行）原样返回同一引用', () => {
    const list = [task()]
    expect(appendTaskLine(list, { id: 'nope', line: 'x', stream: 'stderr' })).toBe(list)
  })
})

describe('时间', () => {
  it('跑着的按 now 算，结束的按 endedAt 算', () => {
    const running = task({ startedAt: 1_000 })
    expect(taskElapsed(running, 4_500)).toBe(3_500)
    const done = task({ startedAt: 1_000, endedAt: 3_000, state: 'done' })
    expect(taskElapsed(done, 999_999)).toBe(2_000)
  })

  it('秒表格式', () => {
    expect(formatElapsed(0)).toBe('00:00')
    expect(formatElapsed(42_000)).toBe('00:42')
    expect(formatElapsed(12 * 60_000 + 3_000)).toBe('12:03')
    expect(formatElapsed(3_600_000 + 2 * 60_000 + 9_000)).toBe('1:02:09')
  })
})

describe('groupTasks', () => {
  it('运行中在前，已结束在后', () => {
    const list = [
      task({ id: 'done', startedAt: 9_000, state: 'done', endedAt: 10_000 }),
      task({ id: 'run-old', startedAt: 1_000 }),
      task({ id: 'run-new', startedAt: 8_000 }),
    ]
    const groups = groupTasks(list)
    expect(groups.running.map(item => item.id)).toEqual(['run-new', 'run-old'])
    expect(groups.ended.map(item => item.id)).toEqual(['done'])
  })
})

describe('taskExitLabel', () => {
  it('运行中不写、停止写原因、失败写退出码', () => {
    expect(taskExitLabel(task())).toBe('')
    expect(taskExitLabel(task({ state: 'stopped', endedAt: 2_000 }))).toBe('已停止')
    expect(taskExitLabel(task({ state: 'done', endedAt: 2_000, exitCode: 0 }))).toBe('exit 0')
    expect(taskExitLabel(task({ state: 'failed', endedAt: 2_000, exitCode: 128 }))).toBe('exit 128')
  })
})
