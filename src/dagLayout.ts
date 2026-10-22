/**
 * DAG 分层布局（纯函数）。
 *
 * 不引图库：一张图最多二十几个节点，手写分层 + 重心排序足够，而且结果**确定**
 * （同样的输入永远同样的输出），便于单测和视觉回归。
 *
 * 算法（`docs/DAG_PLANNER.md` §8.2）：
 * 1. 最长路径定 rank（根 rank=0，节点 rank = max(前驱)+1）；
 * 2. 同层按初始顺序排，再做 4 轮重心扫描减少交叉；
 * 3. 列坐标 = rank × (宽 + 间距)，各列垂直居中。
 */

/** 节点卡默认尺寸（与 CSS 里的 `.dag-node` 对齐）。 */
export const NODE_W = 200
export const NODE_H = 92
export const GAP_X = 64
export const GAP_Y = 40

/** 布局只需要这两个字段，调用方直接传 `DagNode` 也行。 */
export interface LayoutInput {
  id: string
  dependsOn: string[]
}

export interface NodeBox {
  id: string
  /** 列号，0 是最上游。 */
  rank: number
  x: number
  y: number
}

export interface DagLayout {
  boxes: Map<string, NodeBox>
  /** 世界尺寸（画布「适应窗口」用）。 */
  width: number
  height: number
  maxRank: number
}

interface Size {
  nodeWidth: number
  nodeHeight: number
  gapX: number
  gapY: number
}

const DEFAULT_SIZE: Size = {
  nodeWidth: NODE_W,
  nodeHeight: NODE_H,
  gapX: GAP_X,
  gapY: GAP_Y,
}

/** 环上的节点也得有个位置——返回 0 而不是无限递归。 */
function rankOf(
  id: string,
  byId: Map<string, LayoutInput>,
  ranks: Map<string, number>,
  guard: Set<string>,
): number {
  const known = ranks.get(id)
  if (known !== undefined) return known
  if (guard.has(id)) return 0
  guard.add(id)
  const deps = (byId.get(id)?.dependsOn ?? []).filter(dep => byId.has(dep) && dep !== id)
  const rank =
    deps.length === 0 ? 0 : Math.max(...deps.map(dep => rankOf(dep, byId, ranks, guard) + 1))
  guard.delete(id)
  ranks.set(id, rank)
  return rank
}

/** 重心：用它换掉"按输入顺序排"，能显著减少长边交叉。 */
function barycenter(ids: string[], weights: Map<string, number[]>): number {
  const values = ids.flatMap(id => weights.get(id) ?? [])
  if (values.length === 0) return Number.MAX_SAFE_INTEGER
  return values.reduce((sum, value) => sum + value, 0) / values.length
}

export function layoutDag(nodes: LayoutInput[], size: Partial<Size> = {}): DagLayout {
  const { nodeWidth, nodeHeight, gapX, gapY } = { ...DEFAULT_SIZE, ...size }
  const boxes = new Map<string, NodeBox>()
  if (nodes.length === 0) {
    return { boxes, width: 0, height: 0, maxRank: 0 }
  }

  const byId = new Map(nodes.map(node => [node.id, node]))
  const ranks = new Map<string, number>()
  for (const node of nodes) {
    rankOf(node.id, byId, ranks, new Set())
  }

  // 按 rank 分列，列内先保持输入顺序。
  const columns = new Map<number, string[]>()
  for (const node of nodes) {
    const rank = ranks.get(node.id) ?? 0
    const column = columns.get(rank)
    if (column) column.push(node.id)
    else columns.set(rank, [node.id])
  }
  const maxRank = Math.max(...columns.keys())

  // ---- 重心扫描：向下看前驱、向上看后继，各两轮 ----
  // 存的是「邻居节点 id」，重心扫描时再换成邻居列里的下标。
  const predecessorWeights = new Map<string, string[]>()
  const successorWeights = new Map<string, string[]>()
  for (const node of nodes) {
    for (const dep of node.dependsOn) {
      if (!byId.has(dep) || dep === node.id) continue
      push(successorWeights, dep, node.id)
      push(predecessorWeights, node.id, dep)
    }
  }

  const indexIn = (column: string[]) => {
    const map = new Map<string, number>()
    column.forEach((id, index) => map.set(id, index))
    return map
  }

  for (let sweep = 0; sweep < 4; sweep += 1) {
    const downward = sweep % 2 === 0
    const order = downward
      ? [...columns.keys()].sort((a, b) => a - b)
      : [...columns.keys()].sort((a, b) => b - a)

    for (const rank of order) {
      const column = columns.get(rank) ?? []
      const neighbourRank = downward ? rank - 1 : rank + 1
      const neighbour = columns.get(neighbourRank)
      if (!neighbour || neighbour.length === 0) continue
      const neighbourIndex = indexIn(neighbour)
      const weights = new Map<string, number[]>()
      for (const id of column) {
        const source = downward ? predecessorWeights : successorWeights
        const list = (source.get(id) ?? [])
          .map(peer => neighbourIndex.get(peer))
          .filter((value): value is number => value !== undefined)
        if (list.length > 0) weights.set(id, list)
      }
      const base = indexIn(column)
      column.sort((a, b) => {
        const delta = barycenter([a], weights) - barycenter([b], weights)
        if (Number.isFinite(delta) && delta !== 0) return delta
        return (base.get(a) ?? 0) - (base.get(b) ?? 0)
      })
      columns.set(rank, column)
    }
  }

  // ---- 坐标：列间横排，列内垂直居中 ----
  const columnHeights = new Map<number, number>()
  for (const [rank, column] of columns) {
    columnHeights.set(rank, column.length * nodeHeight + Math.max(0, column.length - 1) * gapY)
  }
  const tallest = Math.max(...columnHeights.values())

  for (const [rank, column] of columns) {
    const offset = (tallest - (columnHeights.get(rank) ?? 0)) / 2
    column.forEach((id, index) => {
      boxes.set(id, {
        id,
        rank,
        x: rank * (nodeWidth + gapX),
        y: offset + index * (nodeHeight + gapY),
      })
    })
  }

  return {
    boxes,
    width: (maxRank + 1) * (nodeWidth + gapX) - gapX,
    height: tallest,
    maxRank,
  }
}

function push(map: Map<string, string[]>, key: string, value: string): void {
  const list = map.get(key)
  if (list) list.push(value)
  else map.set(key, [value])
}

/** 边的三次贝塞尔：从右中点连到左中点。 */
export function edgePath(from: NodeBox, to: NodeBox, nodeWidth = NODE_W, nodeHeight = NODE_H): string {
  const x1 = from.x + nodeWidth
  const y1 = from.y + nodeHeight / 2
  const x2 = to.x
  const y2 = to.y + nodeHeight / 2
  // 回边（目标在左边）时把控制点往外推，避免线穿过节点卡。
  const bend = Math.max(28, Math.abs(x2 - x1) * 0.5)
  return `M ${x1} ${y1} C ${x1 + bend} ${y1}, ${x2 - bend} ${y2}, ${x2} ${y2}`
}

/**
 * 手工坐标优先：自动分层照算（它决定 rank 与世界尺寸），但用户拖过的节点用它
 * 自己的坐标。`stored`（计划里存的 layout）优先于节点自身的 `position`。
 */
export function withOverrides(
  nodes: (LayoutInput & { position?: { x: number; y: number } | null })[],
  stored: Record<string, { x: number; y: number }> = {},
  size: Partial<Size> = {},
): DagLayout {
  const auto = layoutDag(nodes, size)
  const { nodeWidth, nodeHeight } = { ...DEFAULT_SIZE, ...size }
  const boxes = new Map<string, NodeBox>()
  let width = auto.width
  let height = auto.height
  let maxRank = auto.maxRank

  for (const node of nodes) {
    const box = auto.boxes.get(node.id) ?? { id: node.id, rank: 0, x: 0, y: 0 }
    const manual = stored[node.id] ?? node.position ?? null
    const next = manual ? { ...box, x: manual.x, y: manual.y } : box
    boxes.set(node.id, next)
    width = Math.max(width, next.x + nodeWidth)
    height = Math.max(height, next.y + nodeHeight)
    maxRank = Math.max(maxRank, next.rank)
  }
  return { boxes, width, height, maxRank }
}
