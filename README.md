# Prism

> 多引擎 AI 编码 Agent 指挥台 —— 把不同引擎的会话收进同一面墙：浏览历史、
> 发指令、执行中插话、权限审批，都在一处。

Prism 是一个桌面应用（Tauri 2 + Vue 3 + Rust），不自己实现 Agent，
而是驱动各家官方的 CLI / 本地服务，把引擎差异封在适配器里：

| 引擎 | 状态 | 数据（只读） | 运行 |
|---|---|---|---|
| OpenCode | ✅ 完整支持 | `~/.local/share/opencode/opencode.db` | `opencode-cli serve`（HTTP + SSE） |
| Codex | ✅ 完整支持 | `~/.codex/sessions/`（rollout） | `codex app-server`（JSON-RPC） |
| Claude Code | 🟡 历史浏览 | `~/.claude/projects/`（JSONL） | `claude` CLI（stream-json，待安装） |

## 安装（v0.2.0）

发布产物在 `src-tauri/target/release/bundle/`：
`macos/Prism.app` 与 `dmg/Prism_0.2.0_aarch64.dmg`（未签名）。

双击 dmg，把 Prism 拖入「应用程序」即可。未签名版本首次打开会被 Gatekeeper 拦下
（"Apple 无法检查其是否包含恶意软件"）：

- 打开方式：在 **访达 → 应用程序** 里对 Prism **右键 → 打开**，弹窗再点一次「打开」
  （只需一次；直接双击会被拦）。
- 或命令行：`xattr -dr com.apple.quarantine /Applications/Prism.app`。

> 自动更新（Tauri updater + GitHub Releases）尚未接入——需要发布源与签名密钥；
> 当前升级方式 = 下载新版 dmg 覆盖安装。发版流程见 [docs/RELEASE.md](docs/RELEASE.md)。

## 开发

```bash
pnpm install
pnpm tauri dev     # 开发模式（需要 Rust 工具链）
pnpm test          # 前端单测（vitest）
pnpm tauri build   # 打包（macOS：.app + .dmg）
```

桌面图标为自研生成：`python3 scripts/make-icon.py app-icon.png`，再
`pnpm tauri icon app-icon.png` 生成全套平台尺寸。

菜单栏（托盘）图标同为自研生成（单色模板，44×44）：
`node src-tauri/icons/tray-icon.cjs src-tauri/icons/tray.png`。

## 设计原则

- **数据主权**：引擎原始数据架构级只读；Prism 自己的增值数据（标题、标签等）独立存放。
- **适配器架构**：新引擎 = 新适配器，界面零改动。
- **中立事件**：运行时事件（流式文本、工具调用、权限请求）统一为中立模型，上层不感知引擎差异。

路线图与范围见 [docs/PLAN.md](docs/PLAN.md)。
