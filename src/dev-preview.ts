/**
 * 浏览器预览入口：在真浏览器里跑真 App，后端换成 `dev-preview-fake` 的假实现。
 *
 * 用途是**视觉与交互回归**——改完样式想知道"实际长什么样"时，不必启动 Tauri
 * 也不必真的建会话。和一键截图配合使用，见 README「视觉验证」一节。
 *
 * ```
 * pnpm dev --port 1521          # 另一个端口，别污染真 app（1520 同源）的存储
 * open "http://localhost:1521/dev-preview.html?view=archive&theme=light"
 * ```
 *
 * 面包屑：真 App 的 `main.ts` 还挂了系统通知 / F12 热键，预览不需要，所以这里
 * 只保留 `initTheme` + 渲染错误钩子，其余照 `main.ts` 最小化起。
 */
import './dev-preview-fake'
import './dev-preview-drive'
import { createApp } from 'vue'
import App from './App.vue'
import 'katex/dist/katex.min.css'
import './styles.css'
import { initTheme } from './theme'
import { installRenderErrorHooks } from './renderErrors'

// 截图用：?view=archive 落到归档；?theme=light 固定浅色（系统是深色时也能看浅色稿）。
const params = new URLSearchParams(location.search)
try {
  const view = params.get('view')
  if (view) localStorage.setItem('prism-view', view)
  const theme = params.get('theme')
  if (theme) localStorage.setItem('prism-theme', theme)
} catch {
  // 存不进去不影响预览。
}

initTheme()

document.addEventListener('contextmenu', event => {
  const target = event.target as HTMLElement | null
  if (target?.closest('input, textarea, [contenteditable="true"]')) return
  event.preventDefault()
})

/** 截图用：?tasks=open 展开任务坞；chooser 开空标签页；term / new 走到终端页。 */
const taskView = params.get('tasks')
if (taskView) {
  window.setTimeout(() => {
    if (taskView === 'agent') {
      // 只发事件、不调 task_spawn：模拟 agent 的 run_command background=true。
      ;(window as unknown as { __agentTask?: () => string }).__agentTask?.()
      return
    }
    if (taskView === 'open') {
      ;(document.querySelector('.dock-strip') as HTMLElement | null)?.click()
      return
    }
    ;(document.querySelector('.changes-btn') as HTMLElement | null)?.click()
    if (taskView === 'files') return
    window.setTimeout(() => {
      ;(document.querySelector('.pane-add') as HTMLElement | null)?.click()
      if (taskView === 'chooser') return
      window.setTimeout(() => {
        const choices = document.querySelectorAll('.pane-choice')
        const index = taskView === 'files' ? 0 : 1
        ;(choices[index] as HTMLElement | undefined)?.click()
        if (taskView === 'new') {
          window.setTimeout(() => {
            ;(document.querySelector('.tl-add') as HTMLElement | null)?.click()
          }, 350)
        }
      }, 260)
    }, 360)
  }, 1500)
}

/** 截图用：?settings=6 打开设置并切到第 7 个分类（0 起，扩展能力是 6）。 */
const settingsIndex = params.get('settings')
if (settingsIndex !== null) {
  const index = Number(settingsIndex)
  window.setTimeout(() => {
    ;(document.querySelector('[aria-label="设置"]') as HTMLElement | null)?.click()
    if (!Number.isFinite(index)) return
    window.setTimeout(() => {
      const items = document.querySelectorAll('.settings-nav-item')
      ;(items[index] as HTMLElement | undefined)?.click()
    }, 320)
  }, 900)
}

/** 截图用：?dag=run 在编排页点一次「运行」，看运行条与节点状态。 */
const dagAction = params.get('dag')
if (dagAction) {
  window.setTimeout(() => {
    if (dagAction === 'planner') {
      ;(document.querySelector('.pane-foot .primary-btn') as HTMLElement | null)?.click()
      return
    }
    ;(document.querySelector('.primary-btn.run') as HTMLElement | null)?.click()
  }, 1400)
}

/** 截图用：?workshop=plugins 切到工坊的插件清单。 */
const workshopMode = params.get('workshop')
if (workshopMode) {
  window.setTimeout(() => {
    const buttons = document.querySelectorAll<HTMLElement>('.mode-btn')
    buttons.forEach(button => {
      if (button.textContent?.trim() === workshopMode) button.click()
    })
  }, 900)
}

/** 截图用：?agents=executor 切到 Agent 详情里的「执行器」标签（看可用性提示）。 */
const agentsTab = params.get('agents')
if (agentsTab) {
  window.setTimeout(() => {
    const tabs = document.querySelectorAll<HTMLElement>('.insp-tabs button')
    tabs.forEach(button => {
      if (button.textContent?.trim() === agentsTab) button.click()
    })
  }, 900)
}

const app = createApp(App)
installRenderErrorHooks(app)
app.mount('#app')
