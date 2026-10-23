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

- **归档**：项目 / 会话 / 时间线三栏。diff、工具卡完整输入输出、权限卡、LaTeX、沙箱 HTML 都能看；每轮 token 与成本、上下文用量条。
- **工作台**：多列并排盯多个会话，发送 / 插话 / 停止 / 审批都在列内；列宽可拖、布局持久化。
- **面板**：侧边抽屉里开多个标签页（文件 / 终端，以后可加网页）。空标签页选入口，标签可拖动排序、多了横向滚；文件页能看变更 diff、预览、直接编辑保存。
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
| `run_command` | 跑命令，默认 120s 超时；`background: true` 转后台任务 |
| `task_status` | 查本会话的后台任务 / 看尾部输出 / 停止 |
| `tools.list` | 自省：列出此刻真正能调用的工具（已按权限档、作用域、开关过滤）与被挡住的原因 |
| `fetch` | 抓网页查文档 / CVE：只 GET + http(s)，默认 20s 超时、2 万字符上限，HTML 抽成正文（去脚本 / 样式 / 注释），标注最终 URL / 状态码 / 类型 / 大小 / 耗时 |
| `task` | 子代理：隔离上下文跑子任务（`explore` 只读 / `general` 全开），只带回报告 |
| `bin.meta.identify` / `bin.hash.compute` / `bin.strings.extract` / `bin.pack.detect` | 样本静态预处理：格式与架构、三哈希、ASCII/UTF-16 字符串与可疑标记、加壳识别（38 条规则 + 节表结构信号 + 熵） |
| `bin.json.jq` / `bin.xref.search` / `bin.importexport.list` / `bin.crypto.scan` / `bin.diff` | 静态分析：JSON 路径取值、交叉引用、导入导出表与高危 API、密码常量、两样本差异 |
| `prism.artifact.put` / `prism.artifact.get` | 制品读写；只在 DAG 节点里可见，同 DAG 可读、跨 DAG 拒绝 |
| `prism.dag.*`（7 个） | 改图：读计划、增删节点与依赖、校验；只在 DAG 节点里可见，校验不过整体回滚 |

`bin.*` 全部是纯 Rust 实现、只读、不出网、不依赖外部程序——装好就能用。需要 Ghidra / 沙箱的工具还没接，
调用时会明确报「未接入」，不会返回空结果假装成功。

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
- `fetch` 只做 GET：需要 POST、下载文件、登录态或 JS 渲染的页面，改用 `run_command` 自己来。
- 外部引擎执行器（dsh / codex / opencode）只管「开会话 → 发消息 → 收正文」：工具策略与权限档由各引擎自己的配置决定，Prism 不接管；节点里弹审批时没人点，超时（30 分钟）会打断并报超时。
- 定时任务需要 Prism 进程在跑（关窗只是隐藏窗口）。

路线图与里程碑见 [docs/ROADMAP.md](docs/ROADMAP.md)。
