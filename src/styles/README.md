# 样式与主题

## 目录

- `src/styles.css`：全局样式入口，只负责 @import。
- `src/styles/tokens.css`：所有颜色、字体、圆角、阴影令牌；主题只在文件里换值。
- `src/styles/base.css`：reset、body、滚动条等全局基础样式。
- `src/styles/markdown.css`：`.md` 渲染（v-html 内容必须是全局样式）。
- `src/styles/components/`：每个组件一个 CSS 文件，由组件里的
  `<style scoped src="../styles/components/Xxx.css"></style>` 引入。

## 约束

1. 组件 CSS 不写死颜色，只使用 `var(--...)` 令牌。
   彩色底上的文字用对应的 `--on-*` 令牌（`--on-accent` / `--on-amber` / `--on-danger`），不要直接写白色。
2. 新组件样式放到 `src/styles/components/<Component>.css`，不要在 `.vue` 里堆样式。
3. 需要主题化的新值，先加到 `tokens.css` 的浅色块，再补深色块和
   `prefers-color-scheme` 块。

## 主题模式

- 默认：`:root`（浅色）。
- 显式深色：`:root[data-theme='dark']`。
- 跟随系统：不设置 `data-theme`，由 `prefers-color-scheme` 决定。
- 切换与持久化在 `src/theme.ts`，设置弹窗的「外观」区域调用它。

新增主题（例如 sepia）：

1. 在 `tokens.css` 增加 `:root[data-theme='sepia'] { ... }` 覆盖同名令牌。
2. 在 `src/theme.ts` 的 `ThemeMode` / `THEME_MODES` 中加入 `sepia`。
3. 设置弹窗会按 `THEME_MODES` 自动多出一个按钮。
