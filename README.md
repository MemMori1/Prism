# Prism

> 多引擎 AI 编码 Agent 指挥台 —— 把不同引擎的会话收进同一面墙：浏览、搜索、并行指挥。

Prism 是一个桌面应用（Tauri 2 + Vue 3 + Rust），不自己实现 Agent，
而是驱动各家官方的 CLI / 本地服务，把引擎差异封在适配器里：

| 引擎 | 状态 | 数据（只读） | 运行 |
|---|---|---|---|
| OpenCode | ✅ 完整支持 | `~/.local/share/opencode/opencode.db` | `opencode-cli serve`（HTTP + SSE） |
| Claude Code | 规划中 | `~/.claude/projects/`（JSONL） | `claude` CLI（stream-json） |
| Codex | 规划中 | `~/.codex/sessions/`（rollout） | `codex app-server`（JSON-RPC） |

## 开发

```bash
pnpm install
pnpm tauri dev     # 开发模式（需要 Rust 工具链）
pnpm tauri build   # 打包
```

## 设计原则

- **数据主权**：引擎原始数据架构级只读；Prism 自己的增值数据（标题、标签等）独立存放。
- **适配器架构**：新引擎 = 新适配器，界面零改动。
- **中立事件**：运行时事件（流式文本、工具调用、权限请求）统一为中立模型，上层不感知引擎差异。

路线图与范围见 [docs/PLAN.md](docs/PLAN.md)。
