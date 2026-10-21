# Prism 发布流程（v0.2 起）

> 当前状态：打包 ✅（未签名）；自动更新 ✅ 已接入（发布源 = GitHub Releases，见 §4）。
> 签名与公证需要 Apple Developer ID（见 §3）。

## 1. 发版

1. 三处版本号同步：`package.json`、`src-tauri/tauri.conf.json`、`src-tauri/Cargo.toml`。
2. 构建：`scripts/release.sh`（推荐：含 updater 签名 + `latest.json`）；
   或普通打包 `pnpm tauri build`（不含可更新产物）。
3. 产物在 `src-tauri/target/release/bundle/`：
   - `macos/Prism.app`
   - `dmg/Prism_<版本>_aarch64.dmg`
4. 冒烟清单：应用启动、菜单栏出现 Prism 三角图标、关窗 ≠ 退出（Cmd+W 后
   调度/通知继续跑）、托盘「打开 Prism」唤起窗口、核心链路（浏览 / 发消息 / 审批）可用。

## 2. 未签名版本的安装（Gatekeeper）

未签名版本首次打开会被系统拦下（"Apple 无法检查其是否包含恶意软件"）：

- 访达 → 应用程序 → 对 Prism **右键 → 打开** → 弹窗再点「打开」（仅首次）；
- 或命令行：`xattr -dr com.apple.quarantine /Applications/Prism.app`。

## 3. 签名与公证（可选，需 Apple Developer ID）

- 证书：Developer ID Application（$99/年）。
- `tauri.conf.json`：`bundle.macOS.signingIdentity` = 证书名；构建时给
  `APPLE_ID` / `APPLE_PASSWORD`（app 专用密码）/ `APPLE_TEAM_ID`，
  Tauri 会自动走 notarytool 公证。
- 效果：下载即开，无 Gatekeeper 提示。

## 4. 自动更新（Tauri updater + GitHub Pages 镜像 / Releases 兜底）

已接入。更新清单有两个端点（`tauri.conf.json` 的 `plugins.updater.endpoints`，按序尝试；
清单里的安装包 URL 指向 gh-pages 镜像）：

    1) https://memmori1.github.io/Prism/latest.json  （首选：Fastly CDN，国内可达性更好）
    2) https://github.com/MemMori1/Prism/releases/latest/download/latest.json  （兜底）

- 签名密钥：私钥在开发机 `~/.prism/updater.key`（**绝不入库**）；公钥已写入
  `tauri.conf.json` 的 `plugins.updater.pubkey`（公开信息）。
- 一条命令发版：`scripts/release.sh`（校验三处版本号一致 → 构建 + 签名 →
  产物收拢到 `release-artifacts/` 并生成 `latest.json`）。
- 发布（tag 用 `v<版本>`，如 `v0.2.1`）：
  - 有 gh CLI：`scripts/release.sh --publish`——创建 GitHub Release **并自动强制推送
    gh-pages 镜像**（latest.json + dmg + app.tar.gz），首次先 `gh auth login`；
  - 手工路径：Releases 网页拖入 `release-artifacts/` 全部文件后，再把同批文件
    推送到 gh-pages 分支（镜像）——只发 Release 时旧版用户仍可经兜底端点更新。
- 客户端：设置面板 →「更新」区块 → 检查更新 / 下载并安装 / 重启生效。
- 验收闭环：装旧版 → 发布新版（版本号 +1）→ 点「检查更新」→ 下载重启后版本已更新。

## 5. Windows 版

macOS 上不能交叉编译 Tauri 的 Windows 目标（NSIS / MSVC 工具链），所以走 GitHub Actions：

1. Actions → **release-windows** → Run workflow，`tag` 填要发的那一版（如 `v0.3.0`）。
2. 在 `windows-latest` 上打 NSIS 安装包：`Prism_<版本>_x64-setup.exe`。
3. 工作流自动把它挂到该 tag 的 Release，并把 Release 里的全部产物同步到 gh-pages 镜像。
4. 自动更新：仓库配好 `TAURI_SIGNING_PRIVATE_KEY`（私钥文件内容）与
   `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` 后，额外签 updater 产物，并把
   `windows-x86_64` 平台项并进 `latest.json`；没配就只出安装包（Windows 端不提示更新）。

已知差异：Windows 没有进程组，停止任务只收直接子进程，`pnpm build` 这类拉起的
孙子进程可能残留；macOS 侧是整组 SIGTERM → SIGKILL。

## 6. 版本历史

| 版本 | 日期 | 说明 |
|---|---|---|
| 0.1.0 | 2026-10-17 | 首个打包冒烟版（浏览 / 指挥 / 审批） |
| 0.2.0 | 2026-10-17 | M1–M12：工作台 / 搜索 / 双引擎运行轴 / 工坊 / 自动化 + 通知 / 托盘 + 关窗 ≠ 退出 |
| 0.2.1 | 2026-10-17 | M13 用量面板（跨引擎 token / 每日曲线 / 成本估算）；更新源切 gh-pages 镜像 |
| 0.2.2 | 2026-10-17 | 版本号 +1（自动更新升级验收用，无功能变更） |
| 0.2.3 | 2026-10-17 | M14–M19 + 组合器升级（引擎能力目录 / @文件 / /命令 / ! shell） |
| 0.3.0 | 2026-10-21 | 后台任务：进程组管理 + 任务坞 + 终端标签，`run_command` 支持 background 并配 `task_status`；面板改成多标签（文件 / 终端），空标签页选入口、拖动排序、横向滚动；工作台内嵌会话复用完整会话头 |
