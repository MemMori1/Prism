/** 一级视图：左侧图标栏与视图切换共用同一份定义，避免两边漂移。 */
export type AppView =
  | 'home'
  | 'archive'
  | 'workbench'
  | 'race'
  | 'search'
  | 'usage'
  | 'workshop'
  | 'automation'

export const VIEW_KEY = 'prism-view'

export interface NavItem {
  view: AppView
  label: string
  /** 悬停提示：保留原顶栏按钮里的完整说明。 */
  hint: string
  /** 24×24 线性图标，每条 path 的 d 指令。 */
  icon: string[]
}

export const NAV_ITEMS: readonly NavItem[] = [
  {
    view: 'home',
    label: '首页',
    hint: '首页仪表盘：token 脉搏 / 工作节奏 / 28 天热力图 / 模型分布 / 项目活跃',
    icon: ['M4 10.5 12 4l8 6.5V20H4z', 'M9.5 20v-6h5v6'],
  },
  {
    view: 'archive',
    label: '归档',
    hint: '历史浏览：项目 / 会话 / 时间线',
    icon: ['M3.5 7.5h17V20h-17z', 'M3.5 4h17v3.5h-17z', 'M9.5 12h5'],
  },
  {
    view: 'workbench',
    label: '工作台',
    hint: '多列并行指挥：同时盯着多个跑着的会话',
    icon: ['M3.5 5.5h17v13h-17z', 'M9 5.5v13', 'M15 5.5v13'],
  },
  {
    view: 'race',
    label: '赛马',
    hint: '赛马：同一问题广播多个引擎，答案 / 用时 / token / 成本并排对比',
    icon: ['M5.5 21V3.5', 'M5.5 4.5h12.5l-2.5 4 2.5 4H5.5'],
  },
  {
    view: 'search',
    label: '搜索',
    hint: '全文搜索：检索全部会话内容（本地索引，支持中文子串）',
    icon: ['M10.8 4a6.8 6.8 0 1 0 0 13.6 6.8 6.8 0 0 0 0-13.6Z', 'm20 20-4.4-4.4'],
  },
  {
    view: 'usage',
    label: '用量',
    hint: '用量：跨引擎 token 消耗与成本估算（只读统计引擎本地档案）',
    icon: ['M4 20h16', 'M6.5 20v-6', 'M12 20V7', 'M17.5 20v-9'],
  },
  {
    view: 'workshop',
    label: '工坊',
    hint: '工坊：引擎资产（技能 / 命令 / 子代理 / 记忆 / 插件）只读浏览',
    icon: [
      'M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z',
    ],
  },
  {
    view: 'automation',
    label: '自动化',
    hint: '自动化：定时任务——到点用引擎跑提示词，结果进通知中心',
    icon: ['M12 3.5a8.5 8.5 0 1 0 0 17 8.5 8.5 0 0 0 0-17Z', 'M12 7.5V12l3 2'],
  },
]

export function isAppView(value: string | null): value is AppView {
  return value !== null && NAV_ITEMS.some(item => item.view === value)
}

export const NAV_ORDER_KEY = 'prism-nav-order'

const DEFAULT_NAV_ORDER: readonly AppView[] = NAV_ITEMS.map(item => item.view)

/**
 * 首页永远固定在顶部；其余视图按保存的顺序排列。
 * 未知项、重复项会被丢弃，缺失项按默认顺序补回，避免旧存储导致图标消失。
 */
export function normalizeNavOrder(value: unknown): AppView[] {
  const requested = Array.isArray(value) ? value : []
  const seen = new Set<AppView>()
  const rest: AppView[] = []

  for (const candidate of requested) {
    if (typeof candidate !== 'string' || candidate === 'home' || !isAppView(candidate) || seen.has(candidate)) {
      continue
    }
    seen.add(candidate)
    rest.push(candidate)
  }

  for (const view of DEFAULT_NAV_ORDER) {
    if (view !== 'home' && !seen.has(view)) {
      seen.add(view)
      rest.push(view)
    }
  }

  return ['home', ...rest]
}

type StorageLike = Pick<Storage, 'getItem' | 'setItem'>

function defaultStorage(): StorageLike | null {
  return typeof localStorage === 'undefined' ? null : localStorage
}

export function loadNavOrder(storage: StorageLike | null = defaultStorage()): AppView[] {
  if (!storage) return normalizeNavOrder(null)
  try {
    return normalizeNavOrder(JSON.parse(storage.getItem(NAV_ORDER_KEY) ?? 'null'))
  } catch {
    return normalizeNavOrder(null)
  }
}

export function saveNavOrder(order: readonly AppView[], storage: StorageLike | null = defaultStorage()): void {
  if (!storage) return
  try {
    storage.setItem(NAV_ORDER_KEY, JSON.stringify(normalizeNavOrder(order)))
  } catch {
    /* 隐私模式或配额满时写入会抛错；本次排序仍保留在内存里。 */
  }
}

/** 把 view 移到第 gap 个间隙（0 在首页之前，1 在首页之后），首页不可移动。 */
export function reorderNavItems(order: readonly AppView[], view: AppView, gap: number): AppView[] {
  const next = normalizeNavOrder(order)
  if (view === 'home') return next

  const from = next.indexOf(view)
  if (from < 1) return next

  const target = Number.isFinite(gap)
    ? Math.max(1, Math.min(next.length, Math.trunc(gap)))
    : from
  if (target === from || target === from + 1) return next

  const [item] = next.splice(from, 1)
  next.splice(target > from ? target - 1 : target, 0, item)
  return next
}
