/**
 * 遮罩层的「点空白关闭」。
 *
 * 模板里一行 `@click.self` 有两个坑，都会把用户填到一半的内容弄丢：
 *
 * 1. 在卡片里的输入框按下、拖到遮罩上松手，浏览器的 click 会落在两者的
 *    共同祖先——也就是遮罩本身——上。选个字就把弹层关了。所以要同时看
 *    「按下时在哪儿」，只有按下和松开都在遮罩上才算真的点了空白。
 * 2. 表单已经填了东西，误点一下遮罩就静默丢弃。所以留一道闸门
 *    `hasUnsavedWork`：拦下来交给调用方提示，关不关由它决定。
 *
 * 判断逻辑抽成纯函数（`decideDismiss`）是为了能脱离 DOM 测；
 * `useBackdropDismiss` 只是把一次的按下/点击串起来。
 */
import { ref } from 'vue'

export interface DismissGesture {
  /** 按下时手指落在遮罩本身，而不是卡片里的输入框或按钮。 */
  startedOnBackdrop: boolean
  /** 松开时点击事件落在遮罩本身。 */
  endedOnBackdrop: boolean
  /** 里面有还没交出去的输入（表单有字、正在提交）。 */
  unsaved: boolean
}

/**
 * - `dismiss`：确实点了空白，可以关。
 * - `hold`：点了空白，但里面有东西，交给调用方提示而不是直接关。
 * - `ignore`：这不是一次「点空白」（拖选文字、点在卡片里），当没发生。
 */
export type DismissDecision = 'dismiss' | 'hold' | 'ignore'

export function decideDismiss(gesture: DismissGesture): DismissDecision {
  if (!gesture.startedOnBackdrop || !gesture.endedOnBackdrop) return 'ignore'
  if (gesture.unsaved) return 'hold'
  return 'dismiss'
}

export interface BackdropDismiss {
  /** 挂在遮罩上：`@mousedown="backdrop.onMousedown"`。 */
  onMousedown: (event: MouseEvent) => void
  /** 挂在遮罩上：`@click="backdrop.onClick"`。真的关了才返回 true。 */
  onClick: (event: MouseEvent) => boolean
  /** 弹层重新打开时清掉上一次的手势，免得沿用「在遮罩上按下」的旧状态。 */
  reset: () => void
}

export interface BackdropDismissOptions {
  /** 判定为「点空白」后做什么——通常是把 v-if 那个开关关掉。 */
  dismiss: () => void
  /** 有未交出去的输入时返回 true：拦下这次点击，不关。 */
  hasUnsavedWork?: () => boolean
  /** 被拦下时的回调，给一句「内容还在，点取消才关」之类的提示。 */
  onHold?: () => void
}

export function useBackdropDismiss(options: BackdropDismissOptions): BackdropDismiss {
  const startedOnBackdrop = ref(false)

  function onMousedown(event: MouseEvent): void {
    startedOnBackdrop.value = event.target === event.currentTarget
  }

  function onClick(event: MouseEvent): boolean {
    const decision = decideDismiss({
      startedOnBackdrop: startedOnBackdrop.value,
      endedOnBackdrop: event.target === event.currentTarget,
      unsaved: options.hasUnsavedWork?.() ?? false,
    })
    startedOnBackdrop.value = false
    if (decision === 'hold') {
      options.onHold?.()
      return false
    }
    if (decision === 'ignore') return false
    options.dismiss()
    return true
  }

  function reset(): void {
    startedOnBackdrop.value = false
  }

  return { onMousedown, onClick, reset }
}
