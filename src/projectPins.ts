/**
 * 项目置顶（pin）偏好：纯前端记忆，写在 localStorage，不落引擎档案。
 *
 * 为什么按 worktree 路径存而不是 project.id：
 * - opencode 的 id 是它自己库里的行 id，codex / claude 的 id 是路径哈希；
 *   同一条目录在不同引擎下 id 不同，只有路径能保证「一个项目只置顶一次」。
 * - 路径在重新扫描后保持稳定，id 则可能随引擎改版变化。
 */
import { computed, ref } from 'vue'

export const PINS_KEY = 'prism-pinned-projects'

function readPinned(): string[] {
  try {
    const raw = localStorage.getItem(PINS_KEY)
    if (!raw) return []
    const parsed: unknown = JSON.parse(raw)
    if (!Array.isArray(parsed)) return []
    return parsed.filter((item): item is string => typeof item === 'string' && item.length > 0)
  } catch {
    return []
  }
}

/** 置顶路径，顺序即展示顺序（新置顶插到最前）。 */
const pinned = ref<string[]>(readPinned())

export const pinnedPaths = computed<readonly string[]>(() => pinned.value)

export const pinnedSet = computed(() => new Set(pinned.value))

export function isPinned(worktree: string): boolean {
  return pinned.value.includes(worktree)
}

/** 置顶 / 取消置顶，返回操作后的状态。 */
export function togglePinned(worktree: string): boolean {
  const next = isPinned(worktree)
    ? pinned.value.filter(item => item !== worktree)
    : [worktree, ...pinned.value]
  pinned.value = next
  try {
    localStorage.setItem(PINS_KEY, JSON.stringify(next))
  } catch {
    // 隐私模式等写入失败：本次会话内仍然生效，只是不持久化。
  }
  return next.includes(worktree)
}
