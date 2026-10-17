# Prism 发布流程（v0.2 起）

> 当前状态：打包 ✅（未签名）；自动更新 ⏸ 待发布源（见 §4）。
> 签名与公证需要 Apple Developer ID（见 §3）。

## 1. 发版

1. 三处版本号同步：`package.json`、`src-tauri/tauri.conf.json`、`src-tauri/Cargo.toml`。
2. `pnpm tauri build`（需 Rust 工具链；前端会先跑 `vue-tsc` + `vite build`）。
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

## 4. 自动更新（Tauri updater + GitHub Releases）

现状：**未接入**——需要发布源（GitHub 仓库）与 updater 签名密钥。启用步骤：

1. 生成 updater 密钥：`pnpm tauri signer generate -w ~/.prism/updater.key`
   （私钥自行保管，勿入库；公钥写入配置）。
2. 依赖与配置：
   - Rust：`tauri-plugin-updater`；前端：`@tauri-apps/plugin-updater`；
   - capability 加 `updater:default`；
   - `tauri.conf.json`：
     ```json
     "plugins": {
       "updater": {
         "pubkey": "<第一步的公钥>",
         "endpoints": [
           "https://github.com/<owner>/<repo>/releases/latest/download/latest.json"
         ]
       }
     }
     ```
3. 发布：`TAURI_SIGNING_PRIVATE_KEY=... pnpm tauri build` 产出带 `.sig` 的包；
   把 `latest.json`（`version` / `notes` / `platforms.darwin-aarch64.url + signature`）
   与产物一起上传到 GitHub Release（stable 通道）。
4. 客户端：设置面板加「检查更新」按钮（`check()` → `downloadAndInstall()`）；
   启动时可静默检查、有新版再提示。
5. 验收：装 v0.1 → 发布 v0.2 → 点检查更新 → 重启已是 v0.2。

## 5. 版本历史

| 版本 | 日期 | 说明 |
|---|---|---|
| 0.1.0 | 2026-10-17 | 首个打包冒烟版（浏览 / 指挥 / 审批） |
| 0.2.0 | 2026-10-17 | M1–M12：工作台 / 搜索 / 双引擎运行轴 / 工坊 / 自动化 + 通知 / 托盘 + 关窗 ≠ 退出 |
