# 面板浏览器与 Agent 浏览器能力

> 目标：侧边面板里能开一个**真的**浏览器；Agent（原生引擎）能用这块浏览器读页面、
> 点按钮、填表单——用户看到的就是 Agent 看到的。

## 1. 为什么不按常规做法来

| 方案 | 结果 |
|---|---|
| `<iframe>` 嵌页面 | `X-Frame-Options` / CSP `frame-ancestors` 会把 GitHub、Google、多数文档站直接拒掉。剩下能开的只有本机 dev server——那不叫浏览器 |
| 另开一个系统浏览器窗口 | 能看，但不在面板里；Agent 也控制不了（要另接调试协议） |
| 内嵌一块原生 webview | ✅ 真浏览器：JS 渲染、登录态、点击都正常，且可以被程序驱动 |

所以走第三条：Tauri 的**子 webview**（`Window::add_child`）。这块 API 目前还标着
`unstable`（`Cargo.toml` 里为它开了 `unstable` 特性），风险收在 `browser.rs`
一个模块里——创建失败就退回「用系统浏览器打开」，不连累其它功能。

## 2. 布局：它不在我们的 DOM 里

子 webview 是**原生视图**，不参与 CSS 布局。所以：

- 前端量出内容区矩形（`getBoundingClientRect`，CSS 像素＝逻辑像素），调
  `browser_bounds` 报上去；
- 抽屉有 160ms 开合动画，一次同步不够，在 0 / 60 / 140 / 260ms 各同步一次；
- 窗口缩放、标签切换都用同一个口子重新同步。

**z 序是最大的坑**：原生视图永远画在所有 DOM 之上。只要它该让位，就必须显式
`browser_visible(false)`：

| 什么时候 | 怎么发现 |
|---|---|
| 切到别的标签页 | `active` prop 变化 |
| 收起侧栏 / 切换主视图 | 内容区矩形变成空（`display:none` 下 rect 为零）——每 250ms 检查一次 |
| 弹出设置 / 新建会话 / 规划弹层 / 制品预览等浮层 | 同一个 250ms 检查 `OVERLAY_SELECTOR` 命中 |
| 窗口最小化 | 同上，矩形拿不到 |

> 关键约定：**新增弹窗时，把它的根类名补进 `WebPane.vue` 的 `OVERLAY_SELECTOR`**，
> 漏一个的表现是「打开设置，设置被网页盖住了」。

## 3. 会话模型：一块浏览器，一个司机

- 全局只有一块 webview（label = `browser`），面板与 Agent 共用；
- 关标签 / 收侧栏只是**藏起来**，会话（cookie、登录态、history）都留着，下次打开还在；
- Agent 把页面导走时，用户会在面板里直接看到——这是**故意**的：可核对比隔离重要。
  需要「Agent 自己偷偷查」的场景请用 `fetch`（服务端抓取，不占面板）。

`delegable = false`：`browser.*` 不进子代理与 DAG 节点。一块浏览器只能有一个司机，
并行节点互相把页面导走只会得到一堆错乱的结论。要并行，得先做「每节点一块隔离
webview」——那是下一步的事。

## 3.5 起始页：最近打开的页面与文件

新开浏览器标签时，原生视图先不露脸，DOM 里给一页「起始页」：上面是地址栏（自动填上
上次的地址，回车即续上），下面是两列——**最近打开的页面**与**最近打开的文件**。

- 数据放纯前端：`localStorage` 的 `prism-recent-pages-v1` / `prism-recent-files-v1`，
  各留 30 条、展示 6 条；一个功能一个 key、结构一变就换 v2（与 `projectPins` /
  `panes` 的约定一致）。丢了不影响任何已有数据——它只是「少输一会儿」的便利。
- 页面记在 `browser.state` 事件落定之后（`loading=false`），所以 **Agent 导过去的
  页面也会出现在这里**；`about:blank` / `data:` 不记。
- 文件记在 `FilePane.loadDetail()`：用户真的点开过一个文件才记（diff / 预览 / 编辑
  三种看法都算），带项目目录，起始页优先列本项目的。
- 文件行点一下 = 开一个文件标签（`@open-file` → `openFiles`），不是在这个标签里渲染
  ——浏览器标签只装网页。
- 想从某个页面回到起始页：地址栏右边那个时钟图标。

## 4. Agent 的三个工具

| 工具 | 作用 | 风险档 |
|---|---|---|
| `browser.open` | 打开地址（必要时创建），等页面就绪再返回 | read |
| `browser.read` | 读标题 / 地址 / 正文 / 前 30 个链接，可 `selector` 收窄 | read |
| `browser.act` | `click`（选择器或文字）、`type`（可选提交）、`back/forward/reload`、`wait`（等元素出现） | **act** |

`act` 是新增的风险档：本地磁盘没动，但会改变远端状态（下单、发评论、删数据）。
它在设置页工具卡片上单独标色，而不是混进「只读」。

**JS 怎么来回**：`eval_with_callback` —— Tauri 把脚本返回值序列化成 JSON 交给 Rust，
不需要给页面注入 Tauri IPC、也不需要开 `remote` 能力。换句话说，**网页碰不到我们的
任何命令**，这是选这条路的主要原因（另一条路是给远程页面开 IPC 白名单，等于把
命令面暴露给任意网站）。

`eval` 只支持同步求值（不 await Promise），所以：
- 等页面就绪 → Rust 侧轮询 `document.readyState`；
- 等元素出现 → Rust 侧轮询 `!!document.querySelector(sel)`。

## 5. 安全边界

- 只认 `http(s)`：`normalize_url` 归一地址、`on_navigation` 再拦一道；`file://`
  与 `javascript:` 进不来（页面里的链接也拿它读不到本地文件）；
- 没有审批闸门：DAG 节点里弹审批没人点会卡死。控制点是设置里的工具开关 +
  `risk = act` 的显式标注 + 面板可见；
- 登录态是用户的真实登录态——这和「Agent 能读你的邮箱」是同一类风险，所以
  `browser.*` 默认只给聊天会话，不给子代理；
- `fetch` 仍然是首选：只读、有超时与字符上限、不带登录态。需要 JS 渲染或交互时才升级到浏览器。

## 6. 已知限制

- **Windows**：子 webview 依赖 WebView2，wry 在 Windows 上对子视图有已知问题
  （位置/裁剪）。macOS（WKWebView）是主力验证平台；Windows 上创建失败会走
  「用系统浏览器打开」的退路；
- 浮层检测是 250ms 轮询 + 类名清单，不是响应式信号。将来可以把弹窗统一挂到一个
  `overlay` 注册表上，改成事件驱动；
- 没有截图、没有多标签、没有「每会话隔离」（共用一块 webview 的登录态与 history）；
- 下载、新窗口（`target=_blank`）都在同一个 webview 里处理，没有专门 UI。

## 7. 怎么验

```bash
pnpm tauri dev
```

1. 面板「＋」→「打开浏览器」→ 输入 `example.com` → 回车，页面应出现在面板里；
2. 缩窗口 / 拖抽屉 / 切标签 → 页面跟着走位，切走时不可见；
3. 打开设置 → 页面应消失，关掉设置 → 回来（这就是 `OVERLAY_SELECTOR` 的作用）；
4. 让 Agent 试：`browser.open` 一个文档站 → `browser.read` 看正文 →
   `browser.act {action:"click", text:"..."}` → 再 `browser.read` 复核。

浏览器预览（`dev-preview.html`）里没有真 webview：`browser.*` 命令返回空壳，
地址栏可用但页面不渲染——这是预期行为。
