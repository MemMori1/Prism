/** 托盘状态汇总（M12）：把工作台与归档视图的会话状态合并成「执行中 / 待审批」计数，
 *  推给菜单栏托盘。同一个会话可能同时出现在工作台列与归档视图（两边都在跑），
 *  按 sessionId 去重，避免菜单栏数字虚高。 */

export interface WorkbenchStatus {
  running: number
  waiting: number
  runningIds: string[]
  waitingIds: string[]
}

export interface ArchiveStatus {
  running: boolean
  waiting: number
}

export function mergeTrayCounts(
  workbench: WorkbenchStatus,
  archive: ArchiveStatus,
  archiveSessionId: string | null,
): { running: number; waiting: number } {
  if (archiveSessionId == null) {
    return { running: workbench.running, waiting: workbench.waiting }
  }
  const running = workbench.runningIds.includes(archiveSessionId)
    ? workbench.running
    : workbench.running + (archive.running ? 1 : 0)
  const waiting = workbench.waitingIds.includes(archiveSessionId)
    ? workbench.waiting
    : workbench.waiting + archive.waiting
  return { running, waiting }
}
