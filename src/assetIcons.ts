import type { AssetKind } from './types'

/** 工坊资产种类图标：24×24 线性 path，用于左侧分组、资产列表和详情标签。 */
export const ASSET_KIND_ICONS: Record<AssetKind, string[]> = {
  skill: [
    'M12 3.5l1.9 4.6 4.6 1.9-4.6 1.9L12 16.5l-1.9-4.6L5.5 10l4.6-1.9z',
    'M18 15.5l.8 1.9 1.9.8-1.9.8-.8 1.9-.8-1.9-1.9-.8 1.9-.8z',
  ],
  command: ['M4 6.5h16v11H4z', 'm7.5 10 2.5 2-2.5 2', 'M12.5 14H16'],
  agent: ['M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8z', 'M5 20a7 7 0 0 1 14 0'],
  memory: [
    'M7 7h10v10H7z',
    'M9 3v4M15 3v4M9 17v4M15 17v4',
    'M3 9h4M3 15h4M17 9h4M17 15h4',
  ],
  plugin: [
    'M10 4.5a2 2 0 0 1 4 0V6h3a2 2 0 0 1 2 2v3h-1.5a2 2 0 1 0 0 4H19v3a2 2 0 0 1-2 2h-3v-1.5a2 2 0 1 0-4 0V20H7a2 2 0 0 1-2-2v-3h1.5a2 2 0 1 0 0-4H5V8a2 2 0 0 1 2-2h3z',
  ],
}

const FALLBACK_ICON = ['M4 7.5 12 3l8 4.5v9L12 21l-8-4.5z', 'M4 7.5 12 12l8-4.5', 'M12 12v9']

/** 未知种类也能显示一个通用资产图标，避免出现空白占位。 */
export function assetKindIcon(kind: string): string[] {
  return ASSET_KIND_ICONS[kind as AssetKind] ?? FALLBACK_ICON
}
