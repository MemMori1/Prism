import { describe, expect, it } from 'vitest'
import { ASSET_KIND_ICONS, assetKindIcon } from '../src/assetIcons'
import { ASSET_KIND_LABELS } from '../src/workshop'
import type { AssetKind } from '../src/types'

describe('资产种类图标', () => {
  it('每种资产都有非空图标', () => {
    for (const kind of Object.keys(ASSET_KIND_LABELS) as AssetKind[]) {
      expect(ASSET_KIND_ICONS[kind].length).toBeGreaterThan(0)
      for (const path of ASSET_KIND_ICONS[kind]) {
        expect(path.trim()).not.toBe('')
      }
    }
  })

  it('未知种类回退到通用图标', () => {
    expect(assetKindIcon('nope').length).toBeGreaterThan(0)
    expect(assetKindIcon('skill')).toBe(ASSET_KIND_ICONS.skill)
  })
})
