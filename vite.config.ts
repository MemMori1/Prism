import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// 别在这里引 node:url —— 项目没装 @types/node（前端类型环境刻意不带整套 Node 类型），
// 引了会让 `vue-tsc --noEmit` 在干净环境（CI）上报 TS2307。原先的 `@` 别名没有
// 任何引用，已经去掉；将来真要别名，用 import.meta.url 自己拼，顺手补类型。
export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  server: {
    port: 1520,
    strictPort: true,
    watch: {
      ignored: ['**/src-tauri/**'],
    },
  },
})
