/** 设置工作区的分类元数据：左侧导航与右侧页头共用同一份。 */

export type SettingsSectionId =
  | 'system'
  | 'appearance'
  | 'models'
  | 'engines'
  | 'toolchain'
  | 'channels'
  | 'phone'
  | 'intelligence'
  | 'extensions'
  | 'permissions'

export interface SettingsSectionMeta {
  id: SettingsSectionId
  label: string
  title: string
  description: string
  /** 24x24 线性图标路径。 */
  icon: string[]
}

export const SETTINGS_SECTIONS: readonly SettingsSectionMeta[] = [
  {
    id: 'system',
    label: '系统',
    title: '系统',
    description: '应用更新、版本与本机运行方式。',
    icon: [
      'M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7Z',
      'M19.4 15a1.7 1.7 0 0 0 .34 1.87l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.7 1.7 0 0 0-1.87-.34 1.7 1.7 0 0 0-1.03 1.56V21a2 2 0 1 1-4 0v-.09A1.7 1.7 0 0 0 9 19.36a1.7 1.7 0 0 0-1.87.34l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.7 1.7 0 0 0 4.64 15a1.7 1.7 0 0 0-1.56-1.03H3a2 2 0 1 1 0-4h.09A1.7 1.7 0 0 0 4.64 9a1.7 1.7 0 0 0-.34-1.87l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.7 1.7 0 0 0 9 4.64h.09A1.7 1.7 0 0 0 10.12 3.1V3a2 2 0 1 1 4 0v.09A1.7 1.7 0 0 0 15.15 4.64a1.7 1.7 0 0 0 1.87-.34l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.7 1.7 0 0 0 19.36 9v.09A1.7 1.7 0 0 0 20.9 10.12H21a2 2 0 1 1 0 4h-.09A1.7 1.7 0 0 0 19.4 15Z',
    ],
  },
  {
    id: 'appearance',
    label: '外观',
    title: '外观',
    description: '主题与界面显示；只影响 Prism 本身。',
    icon: [
      'M12 3a9 9 0 0 0 0 18h1.2a2.3 2.3 0 0 0 0-4.6h-.8a1.8 1.8 0 0 1 0-3.6H16a5 5 0 0 0 0-10h-4Z',
      'M7.5 10.5h.01',
      'M9.5 7.5h.01',
      'M14 7.5h.01',
    ],
  },
  {
    id: 'models',
    label: '模型',
    title: '模型管理',
    description: '原生引擎的模型服务、密钥与默认模型。',
    icon: [
      'M4 7.6 12 3l8 4.6-8 4.6-8-4.6Z',
      'm4 12.2 8 4.6 8-4.6',
      'm4 16.6 8 4.6 8-4.6',
    ],
  },
  {
    id: 'engines',
    label: '引擎',
    title: '引擎',
    description: '本机 CLI、数据库与运行轴状态。',
    icon: [
      'm12 3 8 4.5v9L12 21l-8-4.5v-9L12 3Z',
      'm12 12 8-4.5',
      'm12 12-8-4.5',
      'M12 12v9',
    ],
  },
  {
    id: 'toolchain',
    label: '工具链',
    title: '工具链',
    description: '脱壳、反汇编、反编译要用的外部程序。',
    icon: [
      'm4 7.5 8-4.2 8 4.2-8 4.2-8-4.2Z',
      'm4 12.4 8 4.2 8-4.2',
      'm4 17.1 8 4.2 8-4.2',
    ],
  },
  {
    id: 'channels',
    label: '渠道',
    title: '渠道',
    description: '引擎的本机接入方式；Prism 不保存 API Key。',
    icon: [
      'M9 8V5',
      'M15 8V5',
      'M7 8h10v4a5 5 0 0 1-10 0V8Z',
      'M12 17v4',
    ],
  },
  {
    id: 'phone',
    label: '手机链接',
    title: '手机链接',
    description: '扫码把手机连上桌面 Agent，任务进展推送到微信。',
    icon: [
      'M8 3h8a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2Z',
      'M10.5 18.5h3',
      'M9 3v1.5a1.5 1.5 0 0 0 1.5 1.5h3A1.5 1.5 0 0 0 15 4.5V3',
    ],
  },
  {
    id: 'intelligence',
    label: '智能增强',
    title: '智能增强',
    description: 'Prism 对会话内容的加工范围。',
    icon: [
      'M12 3l1.7 4.3L18 9l-4.3 1.7L12 15l-1.7-4.3L6 9l4.3-1.7L12 3Z',
      'M18 15l.8 2.2L21 18l-2.2.8L18 21l-.8-2.2L15 18l2.2-.8L18 15Z',
    ],
  },
  {
    id: 'extensions',
    label: '扩展能力',
    title: '扩展能力',
    description: '工坊资产与 MCP 服务。',
    icon: [
      'M4 4h6v6H4z',
      'M14 4h6v6h-6z',
      'M4 14h6v6H4z',
      'M14 14h6v6h-6z',
    ],
  },
  {
    id: 'permissions',
    label: '权限',
    title: '权限',
    description: 'Prism 读取与执行数据的边界。',
    icon: [
      'M12 3l7 3v6c0 4.5-3 7.5-7 9-4-1.5-7-4.5-7-9V6l7-3Z',
      'm9 12 2 2 4-4',
    ],
  },
]

export function isSettingsSection(value: string | null): value is SettingsSectionId {
  return SETTINGS_SECTIONS.some(section => section.id === value)
}
