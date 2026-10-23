/**
 * 侧栏标签页的纯逻辑。
 *
 * 侧栏是个小标签容器：每个标签装一件东西（文件浏览 / 终端 / 空）。以后要加
 * 「网页」之类的类型，只需要多一个 PaneKind + 一块内容，复用/关闭规则不用动。
 */

/** 标签里装什么。`empty` = 刚点「＋」建出来、还没选内容。 */
export type PaneKind = 'empty' | 'files' | 'term' | 'web'

export interface PaneTab {
  id: string
  kind: PaneKind
  /** files：要定位的文件（绝对路径）；null = 只列目录。 */
  path: string | null
  /** term：选中的任务 id；null = 跟随最新一条。 */
  taskId: string | null
  /** term：新建表单的触发计数，涨一次开一次表单。 */
  newTaskTick: number
  /** web：这个标签想打开的地址（外部意图带进来的）；null = 沿用浏览器当前页。 */
  url: string | null
  /** 标签上显示的细名字（文件名 / 任务命令 / 网页标题）；null = 用类型名。 */
  title: string | null
}

/** 标签上的文字。 */
export const PANE_LABEL: Record<PaneKind, string> = {
  empty: '新标签页',
  files: '文件',
  term: '终端',
  web: '浏览器',
}

export function makeTab(id: string, kind: PaneKind): PaneTab {
  return { id, kind, path: null, taskId: null, newTaskTick: 0, url: null, title: null }
}

/**
 * 挑一个标签来装新内容：
 * 1. 当前标签是空的 → 就用它（「＋ 新标签页」后选打开文件 / 终端，替换当前内容）；
 * 2. 已经有同类型的标签 → 复用它，不再堆积重复的标签；
 * 3. 都没有 → 新建一个。
 */
export function pickTab(
  tabs: PaneTab[],
  activeId: string,
  kind: Exclude<PaneKind, 'empty'>,
  nextId: string,
): { tabs: PaneTab[]; tabId: string } {
  const active = tabs.find(tab => tab.id === activeId)
  if (active && active.kind === 'empty') return { tabs, tabId: active.id }
  const same = tabs.find(tab => tab.kind === kind)
  if (same) return { tabs, tabId: same.id }
  return { tabs: [...tabs, makeTab(nextId, kind)], tabId: nextId }
}

/**
 * 关一个标签：返回剩下的标签、该选哪个、以及「侧栏是不是该关掉」。
 * 关掉最后一个标签就把整个侧栏收起来（和浏览器一致）；任务不会因此停。
 */
export function closeTab(
  tabs: PaneTab[],
  activeId: string,
  id: string,
): { tabs: PaneTab[]; activeId: string; closed: boolean } {
  const index = tabs.findIndex(tab => tab.id === id)
  if (index < 0) return { tabs, activeId, closed: tabs.length === 0 }
  const rest = tabs.filter(tab => tab.id !== id)
  if (!rest.length) return { tabs: rest, activeId: '', closed: true }
  if (activeId !== id) return { tabs: rest, activeId, closed: false }
  // 关掉的是当前标签：优先落到右边的邻居，没有就落到左边。
  const next = rest[Math.min(index, rest.length - 1)]
  return { tabs: rest, activeId: next.id, closed: false }
}

/**
 * 拖动重排：把 from 挪到 to 的前面（after=false）或后面（after=true）。
 * 拖到自身上 / id 对不上时原样返回，调用方直接赋值也不会抖。
 */
export function moveTab(
  tabs: PaneTab[],
  fromId: string,
  toId: string,
  after: boolean,
): PaneTab[] {
  if (fromId === toId) return tabs
  const from = tabs.findIndex(tab => tab.id === fromId)
  if (from < 0 || !tabs.some(tab => tab.id === toId)) return tabs
  const rest = [...tabs]
  const [moved] = rest.splice(from, 1)
  const target = rest.findIndex(tab => tab.id === toId)
  if (target < 0) return tabs
  rest.splice(after ? target + 1 : target, 0, moved)
  return rest
}
