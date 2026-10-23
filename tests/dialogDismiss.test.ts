import { describe, expect, it } from 'vitest'
import { decideDismiss } from '../src/dialogDismiss'

describe('decideDismiss', () => {
  it('按下和松开都在遮罩上、里面没东西：关', () => {
    expect(
      decideDismiss({ startedOnBackdrop: true, endedOnBackdrop: true, unsaved: false }),
    ).toBe('dismiss')
  })

  it('从卡片里按下、拖到遮罩上松手（选字）：当没发生', () => {
    // 浏览器的 click 落在两者的共同祖先上，会走到遮罩的 click 里——
    // 这是「选了一段文字」，不是「点了空白」。
    expect(
      decideDismiss({ startedOnBackdrop: false, endedOnBackdrop: true, unsaved: false }),
    ).toBe('ignore')
  })

  it('点击不在遮罩上：当没发生', () => {
    expect(
      decideDismiss({ startedOnBackdrop: true, endedOnBackdrop: false, unsaved: false }),
    ).toBe('ignore')
  })

  it('确实点了空白，但里面有没交出去的输入：拦下', () => {
    expect(
      decideDismiss({ startedOnBackdrop: true, endedOnBackdrop: true, unsaved: true }),
    ).toBe('hold')
  })

  it('选字带出来的假点击不弹「内容还在」的提示', () => {
    // 提示只在真点了空白时出现，否则拖一次选字就跳一句废话。
    expect(
      decideDismiss({ startedOnBackdrop: false, endedOnBackdrop: true, unsaved: true }),
    ).toBe('ignore')
  })
})
