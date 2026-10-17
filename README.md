# Prism

> 多引擎 AI 编码 Agent 指挥台 —— 把不同引擎的会话收进同一面墙：浏览历史、发指令、执行中插话、权限审批，都在一处。

Tauri 2 + Vue 3 + Rust 桌面应用，不自己实现 Agent，而是驱动各家官方 CLI / 本地服务，把引擎差异封在适配器里：

| 引擎 | 状态 | 接入方式（数据只读） |
|---|---|---|
| OpenCode | ✅ 完整支持 | `opencode-cli serve`（HTTP + SSE） |
| Codex | ✅ 完整支持 | `codex app-server`（JSON-RPC） |
| Claude Code | 🟡 历史浏览 | `claude` CLI（待接入） |

能力一览：归档浏览 · 工作台多列指挥 · 全文搜索 · 工坊（技能 / 命令 / MCP）· 定时任务与通知 · 菜单栏托盘。

## 安装

下载 `Prism_x.y.z_aarch64.dmg`，拖入「应用程序」。未签名版首次打开会被 Gatekeeper 拦下，
请在访达里对 Prism **右键 → 打开**（仅需一次）。细节与发版流程见 [docs/RELEASE.md](docs/RELEASE.md)。

## 开发

```bash
pnpm install
pnpm tauri dev     # 开发模式（需要 Rust 工具链）
pnpm test          # 前端单测（vitest）
pnpm tauri build   # 打包（macOS：.app + .dmg）
```

## 设计原则

- **数据主权**：引擎原始数据架构级只读；Prism 自有数据只存 `~/.prism/`。
- **适配器架构**：新引擎 = 新适配器，界面零改动。
- **中立事件**：运行时事件统一为中立模型，上层不感知引擎差异。
- **干净房间**：不复制任何第三方应用的代码、文案与视觉资源。

路线图见 [docs/ROADMAP.md](docs/ROADMAP.md)。
