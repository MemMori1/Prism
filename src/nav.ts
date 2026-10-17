/** 一级视图：左侧导航栏与顶栏共用同一份定义，避免两边漂移。 */
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
