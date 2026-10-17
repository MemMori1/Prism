import { createApp } from 'vue'
import App from './App.vue'
import 'katex/dist/katex.min.css'
import './styles.css'
import { setSystemNotifier } from './notify'
import { systemNotify } from './systemNotify'
import { initTheme } from './theme'

initTheme()

// 桌面应用不需要浏览器自带右键菜单（重新加载 / 检查元素 / 在新标签页打开…）。
// 输入类控件保留：粘贴、拼写检查、输入法候选还依赖系统菜单。
document.addEventListener('contextmenu', event => {
  const target = event.target as HTMLElement | null
  if (target?.closest('input, textarea, [contenteditable="true"]')) return
  event.preventDefault()
})

// 通知中心 → 系统通知（非前台判定与权限处理在 systemNotify 内）。
setSystemNotifier((title, body) => {
  void systemNotify(title, body)
})

createApp(App).mount('#app')
