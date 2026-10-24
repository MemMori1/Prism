# Prism

多引擎 AI 编码 Agent 指挥台。把 OpenCode / Codex / Claude Code / DeepSeek Harness / 内置 Agent 的会话收进同一面墙：浏览历史、下指令、执行中插话、权限审批，都在一处。

Tauri 2 + Vue 3 + Rust 桌面应用（macOS / Windows x64）。引擎原始数据只读，自有数据只写 `~/.prism/`。

![首页](docs/screenshots/home.png)

| 归档 | 工作台 |
|---|---|
| ![归档](docs/screenshots/archive.png) | ![工作台](docs/screenshots/workbench.png) |
| **面板**：文件改动 / 终端 / 以后还能开网页 | **终端标签**：完整输出与任务列表 |
| ![面板](docs/screenshots/panel-files.png) | ![终端](docs/screenshots/panel-terminal.png) |
| **新标签页**：选打开文件还是终端 | **用量**：跨引擎 token / 成本 / 分布 |
| ![新标签页](docs/screenshots/panel-empty.png) | ![用量](docs/screenshots/usage.png) |

## 功能

- **归档**：项目 / 会话 / 时间线三栏。diff、工具卡完整输入输出、权限卡、LaTeX、沙箱 HTML 都能看；每轮 token 与成本、上下文用量条。攒下来的空会话（没有任何 token 记录、也没加星标）可以在列表底部**一键关掉**，也随时一键恢复——只写 Prism 自己的增补表，引擎的会话数据一个字节不动。
- **工作台**：多列并排盯多个会话，发送 / 插话 / 停止 / 审批都在列内；列宽可拖、布局持久化。
- **面板**：侧边抽屉里开多个标签页（文件 / 终端 / 浏览器）。空标签页选入口，标签可拖动排序、多了横向滚；文件页能看变更 diff、预览、直接编辑保存；浏览器页是一块**真 webview**（JS 渲染、登录态都在），地址栏能输入、能后退刷新，切走或弹窗时自动让位；新开时先给一页起始页，列着**最近打开的页面与文件**，点一下就回去。设计与边界见 [docs/PANEL_BROWSER.md](docs/PANEL_BROWSER.md)。
- **图片输入**：输入框里直接粘贴截图、拖进来或点「图片」选文件，发出去前显示成缩略图 chip（悬停看大图，× 移除）。图片落盘到会话目录，时间线里能看到原图（点开交给系统看图程序）。只在 Prism 原生引擎、且该模型在「设置 → 模型服务」里打开了「图片」开关时可用。
- **压缩上下文**：原生会话里输入 `/compact`（可附一句要求，例如 `/compact 只留 API 契约与待办`）——历史按 4 万字符分段压成一份「交接摘要」，然后**重写会话文件**：下一次请求真的少发几万字符，而不是挂个标记假装省了。原文先备份成 `messages.jsonl.bak`；整个过程就发生在会话时间轴上——「会话压缩已开始 · 12 条消息 · 41203 字符」→ 可展开的摘要卡 → 「会话压缩已结束 · 41203 → 1830 字符」，进度显示在最后那条 live 块里（第 2/3 段），取消则补一行「会话压缩已取消」。这三行是 `role = system`，不发给模型、不花 token；一轮还在跑时不能压缩（会明确拒绝，不排队）。设计见 [docs/COMPACT.md](docs/COMPACT.md)。
- **脚本运行时**：`run_script` 在**自己的环境**里跑模型写的脚本（python / node / sh / powershell）——脚本落在 `~/.prism/sandbox/`，子进程只拿到白名单环境变量（密钥一律不带），macOS 上套 seatbelt：只能写工作目录与临时目录、个人目录连读都挡、网络默认关；申请不到沙箱会**降级并明说**，不假装隔离。删除走 `trash`（移进 `~/.prism/trash/`，带 manifest 可找回）；`rm -rf ~`、格式化磁盘、`curl … | sh`、读 `~/.ssh/id_rsa` 这些先被拦下或要你点头。设计见 [docs/RUNTIME_SANDBOX.md](docs/RUNTIME_SANDBOX.md)。
- **后台任务**：`run_command background=true` 立刻返回任务 id，进程进输入框上方的任务坞继续跑；可停止（整组 SIGTERM → SIGKILL）/ 重跑 / 在终端里看。
- **赛马**：同一问题广播给 1–4 个会话同时跑，答案 / 用时 / token / 成本并排对比。
- **搜索**：本地 FTS5 trigram 索引，中文子串即查即中。
- **用量**：跨引擎 token 明细、每日曲线、模型分布、成本估算、Codex 配额实时值。
- **工坊**：四家引擎的技能 / 命令 / 子代理 / 记忆 / 插件只读浏览；MCP 配置列出并探活。
- **Agent 面板**：内置 16 个 preset（8 个软工 + 7 个安全研究 + 编排规划）+ 自定义。每个 Agent 有执行器、模型、系统提示、工具策略（公用白名单 / 私有 / deny）、权限档与工作区；改内置即覆盖，删掉即还原。
- **编排**：一句需求交给 Planner 拆成 DAG，画布上改节点再跑。每个节点按自己 Agent 的执行器跑（native / dsh / codex / opencode 可混用），失败重试、人工闸门、取消都内建，运行记录落盘。执行器是插件（`dag.executor.*`），没装会在开跑前报错，不会静默降级。
- **跑起来能核对**：运行条上写着「第 3/12 轮 · 静默 42s」，静默超 45s 转黄、超 120s 转红，结束给「成功 5/6 + 失败原因（最多 3 条）」；节点产出按 markdown 渲染（标题 / 列表 / 表格 / 代码块），可切原文逐字对照，也可一键复制。
- **制品**：节点跑完的正文自动落盘（`~/.prism/artifacts/<dag_id>/`），下游默认只拿 8 KB 摘要 + 制品引用，要全文再按 id 取——长报告不会再把上下文撑爆。同 DAG 内可读、跨 DAG 拒绝，单件 256 MiB / 单 DAG 2 GiB 配额。设计见 [docs/ARTIFACTS.md](docs/ARTIFACTS.md)。
- **自动化**：cron 定时任务，到点用引擎跑提示词，日志可回看会话。
- **托盘**：关窗 ≠ 退出，定时任务与通知继续跑。

## 引擎

| 能力 | OpenCode | Codex | Claude Code | DeepSeek Harness | Prism 原生 |
|---|---|---|---|---|---|
| 历史浏览 | ✅ | ✅ | ✅ | ✅ | ✅ |
| 发消息 / 流式 | ✅ | ✅ | 待本机 CLI | ✅ | ✅ |
| 打断 / 插话 | ✅ | ✅ | — | 打断 ✅ / 插话 — | 打断 ✅ / 插话 — |
| 权限审批 | ✅ | ✅ | — | ✅ | 只读 / 询问 / 全开三档，写类与命令弹卡 |
| 新建会话 | ✅ | ✅ | — | ✅ | ✅ |
| 用量统计 | ✅ | ✅ | ✅ | 会话内 token | 会话内 token |

DeepSeek Harness 走官方 ACP（`dsh --profile acp`，stdio JSON-RPC）：Prism 是「外部控制器」，
不碰它的内部结构；历史直接读 `$DSH_HOME/sessions/**/session.v*.jsonl.zstd`（多帧 zstd，
只读不写）。CLI 与 Prism 共用同一份会话日志，一边聊的回合另一边刷新就能看到。

## 内置 Agent

不依赖本机 CLI，直接用 OpenAI 兼容端点跑自己的 Agent 循环。设置里可挂多个服务（DeepSeek / OpenAI / Ollama 预设），密钥只落 `~/.prism/native/config.json`（0600），界面只回显末四位。

| 工具 | 作用 |
|---|---|
| `read_file` / `list_files` / `grep` | 读文件、列目录、正则搜（都被限制在工作目录内） |
| `write_file` / `edit_file` | 整文件写、按片段替换，同样只允许工作目录内 |
| `run_command` | 跑命令，默认 120s 超时；`background: true` 转后台任务。**执行前过闸门**：`rm -rf ~` / 格式化 / 删受保护目录直接拒，提权与 `curl \| sh` 要审批 |
| `run_script` | 在受控环境里跑一段脚本（python / node / sh / powershell）：脚本落 `~/.prism/sandbox/`、环境变量白名单、macOS seatbelt（写入限工作目录与临时目录、禁网）、默认 60s 超时 |
| `trash` | 删除走回收站：移到 `~/.prism/trash/<时间戳>/` 并写 `manifest.json`，一次最多 10 项 |
| `task_status` | 查本会话的后台任务 / 看尾部输出 / 停止 |
| `tools.list` | 自省：列出此刻真正能调用的工具（已按权限档、作用域、开关过滤）与被挡住的原因 |
| `fetch` | 抓网页查文档 / CVE：只 GET + http(s)，默认 20s 超时、2 万字符上限，HTML 抽成正文（去脚本 / 样式 / 注释），标注最终 URL / 状态码 / 类型 / 大小 / 耗时 |
| `browser.open` / `browser.read` / `browser.act` | 面板浏览器的三个动作：打开地址、读渲染后的正文与前 30 个链接、点按钮 / 填表单 / 前进后退 / 等元素。和用户在面板里看到的是同一页；`act` 标 `act` 风险档（会改远端状态） |
| `task` | 子代理：隔离上下文跑子任务（`explore` 只读 / `general` 全开），只带回报告 |
| `bin.meta.identify` / `bin.hash.compute` / `bin.strings.extract` / `bin.pack.detect` | 样本静态预处理：格式与架构、三哈希、ASCII/UTF-16 字符串与可疑标记、加壳识别（38 条规则 + 节表结构信号 + 熵） |
| `bin.json.jq` / `bin.xref.search` / `bin.importexport.list` / `bin.crypto.scan` / `bin.crypto.asn1.parse` / `bin.crypto.xor_brute` / `bin.diff` | 静态分析：JSON 路径取值、交叉引用、导入导出表与高危 API、密码常量、证书与密钥解析、多字节 XOR 爆破、两样本差异 |
| `prism.artifact.put` / `prism.artifact.get` | 制品读写；只在 DAG 节点里可见，同 DAG 可读、跨 DAG 拒绝 |
| `prism.dag.*`（7 个） | 改图：读计划、增删节点与依赖、校验；只在 DAG 节点里可见，校验不过整体回滚 |

`bin.*` 全部是纯 Rust 实现、只读、不出网、不依赖外部程序——装好就能用。需要 Ghidra / 沙箱的工具还没接，
调用时会明确报「未接入」，不会返回空结果假装成功。

`browser.*` 驱动的是面板里那块真 webview：需要 JS 渲染、登录态或点按钮时才用它，
只读查文档仍优先 `fetch`（更快、有上限、不带登录态）。`browser.*` 不委派给子代理——
一块浏览器只能有一个司机（见 [docs/PANEL_BROWSER.md](docs/PANEL_BROWSER.md)）。

`fetch` 是唯一主动出站的工具，所以刻意做了两处收口：设置页里能单独关掉（悬停卡片会写「会出网」，关掉等于断网），
安全研究线的七个工种白名单里一个都没给它——样本目录里的内容不可信，模型读到一个 URL 就能把本机信息带出去。

权限三档：**只读**（写与命令直接拒）/ **每次询问**（写文件、改文件、跑命令弹审批卡）/ **完全访问**。档位按会话记，闸门在 Rust 侧执行。
读类工具与 `fetch` 不弹卡：它们不改磁盘，`fetch` 的出站控制走上面的开关与白名单。

## 安装

- **macOS（Apple Silicon）**：下载 `Prism_x.y.z_aarch64.dmg`，拖进「应用程序」。未签名，首次打开在访达里 **右键 → 打开**。
- **Windows x64**：下载 `Prism_x.y.z_x64-setup.exe`。未签名，SmartScreen 点「更多信息 → 仍要运行」。

都在 [Releases](https://github.com/MemMori1/Prism/releases)。发版与签名见 [docs/RELEASE.md](docs/RELEASE.md)。

## 开发

```bash
pnpm install
pnpm tauri dev                     # 开发（需要 Rust 工具链）
pnpm test                          # 前端单测（vitest）
cd src-tauri && cargo test --lib   # Rust 单测
scripts/release.sh                 # macOS 发版：构建 + updater 签名 + latest.json
```

Windows 版不在本机构建（macOS 不能交叉编译 Tauri 的 Windows 目标），走 Actions 的 `release-windows` 工作流。

### 视觉验证

`dev-preview.html` 把真 App 挂在假后端上，在普通浏览器里跑——改完样式想确认“实际长什么样”，
不必启动 Tauri，也不必真的建会话。

```bash
pnpm dev --port 1521        # 换端口，别和真 app（1520 同源）共用 localStorage
open "http://localhost:1521/dev-preview.html?view=archive&theme=light"
```

URL 参数控制落点：`view=`（归档 / 工作台 / agents / dag / workshop）、`theme=`（light / dark）、
`state=`（bound / fresh / legacy / ctx / json / subagent / long——后四个是工具卡 / 代码块 / 长文的样张）、
`settings=6`（开设置第 7 个分类）、
`tasks=open|files|terminal|chooser`、`dag=run|planner|planner-menu`（在编排页点一次运行 / 打开规划弹窗 /
再点开目录下拉）、
`agents=执行器`（切到 Agent 详情的执行器标签）、`workshop=插件`（切到工坊的插件清单）、`drive=flow`（自驱点一遍并把探针写进 `<pre id="probe">`，
无头 Chrome 读 `--dump-dom` 即可断言）。

`dev-preview-fake.ts` 是那个假后端，在 `tsconfig.include` 范围内，`pnpm build` 的类型检查会覆盖它。
真后端新增 `invoke` 命令后要在这里补一条，否则预览会拿到 `null` 并在组件里炸。

```
src/                 Vue 前端；components/ 一组件一文件，样式在 styles/components/
src/*.ts             纯逻辑模块（nav / diff / markdown / tasks / paneTabs …，多数带单测）
src/dev-preview*     浏览器预览与假后端（视觉回归用，不进打包产物）
src-tauri/src/       Rust 后端；ipc.rs 命令入口，engines/{opencode,codex,claude,dsh,native} 适配器
```

## 已知边界

- macOS 是主力验证平台；Windows 由 CI 出 NSIS 包，两者都未签名 / 公证。Windows 没有进程组，停止只收直接子进程。
- Claude Code 只有历史浏览，运行轴等本机 CLI。
- 搜索索引目前只覆盖 OpenCode 归档。
- 原生 Agent 的审批按**动作**放行（写文件 / 改文件 / 跑命令），读类与 `fetch` 不弹卡；不支持图片输入。
- 图片只支持原生引擎，且要模型自己开了「图片」开关（模型服务卡片上的小标签）；执行中不能排队图片（排队只存文字）。
- `/compact` 是有损的：原文只留在 `messages.jsonl.bak`（只存最近一次），会话里从此只有摘要；摘要写歪了就把那个文件换回去再重压一次。
- **沙箱只在 macOS 是真的**：Windows 没有 `sandbox-exec` 等价物，只有策略层（命令检测 + 受保护路径 + 环境清洗），结果头会明写「无 OS 级隔离」。
- 策略层是启发式，不是安全边界：`bash -c "$(echo … | base64 -d)"` 这类绕法能过。它的作用是拦住「顺手一删」，不是防定向攻击。
- `fetch` 只做 GET：需要 POST、下载文件、登录态或 JS 渲染的页面，改用面板浏览器或 `run_command`。
- 面板浏览器依赖 Tauri 的子 webview（`unstable`）：拿不到就退化成「用系统浏览器打开」。没有截图、多标签与按会话隔离；Windows 上子视图的定位仍受 WebView2 限制。
- 「关闭空会话」只作用于归档的会话列表：工作台里已打开的列、搜索结果不受影响（数据本来就没删）。
- 外部引擎执行器（dsh / codex / opencode）只管「开会话 → 发消息 → 收正文」：工具策略与权限档由各引擎自己的配置决定，Prism 不接管；节点里弹审批时没人点，超时（30 分钟）会打断并报超时。
- 定时任务需要 Prism 进程在跑（关窗只是隐藏窗口）。

路线图与里程碑见 [docs/ROADMAP.md](docs/ROADMAP.md)。
