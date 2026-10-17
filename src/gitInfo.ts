import { reactive } from 'vue'
import { api } from './api'
import type { GitInfo } from './types'

/**
 * 目录 → git 信息（分支 / 主仓库根 / worktree 标记）的缓存。
 *
 * 只查没查过的目录（含「查过但没有」的负缓存）；目录被删除也有缓存兜底。
 */

const EMPTY: GitInfo = { repoRoot: null, branch: null, isWorktree: false }

const cache = reactive(new Map<string, GitInfo>())
const queue = new Set<string>()
let pending = false

/** 批量补查目录 git 信息（带缓存）。 */
export async function ensureGitInfo(paths: (string | null | undefined)[]): Promise<void> {
  for (const path of paths) {
    if (path && !cache.has(path)) queue.add(path)
  }
  if (pending || !queue.size) return
  pending = true
  try {
    while (queue.size) {
      const batch = [...queue]
      queue.clear()
      try {
        const result = await api.gitInfoBatch(batch)
        for (const path of batch) {
          cache.set(path, result[path] ?? EMPTY)
        }
      } catch {
        for (const path of batch) cache.set(path, EMPTY)
      }
    }
  } finally {
    pending = false
  }
}

export function gitInfoOf(path: string | null | undefined): GitInfo | null {
  if (!path) return null
  return cache.get(path) ?? null
}
