import { createApp } from 'vue'
import App from './App.vue'
import './styles.css'
import { setSystemNotifier } from './notify'
import { systemNotify } from './systemNotify'

// 通知中心 → 系统通知（非前台判定与权限处理在 systemNotify 内）。
setSystemNotifier((title, body) => {
  void systemNotify(title, body)
})

createApp(App).mount('#app')
