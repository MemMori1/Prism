# Prism 路线图（v0.2+）

> 定位不变：**多引擎 AI 编码 Agent 指挥台** —— 数据只读、自研视觉、干净房间。
> 2026-10-17 完成一轮功能盘点与差距复核（逐模块核对、关键结论抽查原文），
> 据此修订功能对照与 M6–M13 开发规划。
>
> 一句话路线：**工作台 → 搜索 → Codex → Claude Code → 工坊 → 自动化 → 用量面板 → 平台化发布。**

## 0. 现状快照（M1–M19 完成；M18 组合器升级核心完成——图片输入与输入排队留后；自动更新已接线待首版验收，Claude 运行轴待 CLI）

| 能力 | 说明 |
|---|---|
| 归档浏览 | 三栏（项目 / 会话 / 详情），零导入读 `opencode.db` / Codex state 库 / Claude 档案 |
| 时间线 | 文本 / 推理 / 工具卡片（按需加载详情）/ diff / markdown / 150 条分页 |
| 指挥 | 新建会话组合器（引擎 × 项目 × 模型 × 推理档位 × 首条指令）、发消息、SSE 流式渲染、插话（steer）、中断 |
| 审批 | 权限卡片（允许一次 / 总是允许 / 拒绝），挂起请求补拉 |
| 运行轴 | OpenCode：serve 惰性启动 + 看门狗回收；Codex：`codex app-server` JSON-RPC（流式 / 插话 / 打断 / 审批，配置零覆盖） |
| 工作台 | 多列并行、列宽拖拽、布局持久化、全局审批汇总角标；新建会话走组合器（引擎切换 / 模型 / 推理档位 / 首条指令 / @ 文件 / / 命令 / ＋Git 上下文） |
| 首页 | 仪表盘：今日 token 脉搏（含峰值日）+ 近 7 天合计 + 活跃会话 + 估算成本；28 天热力图（按日强度）+ 工作节奏（按会话开始时刻 24 桶）+ 模型分布（跨引擎 top 5，比例/成本）+ 项目活跃（跨引擎按目录合并，点击跳归档）；复用 M13 用量后端，只读 |
| 搜索 | FTS5 trigram 全库检索（中文子串即查即中；<3 字符回退 LIKE），结果定位高亮 |
| 工坊 | 资产只读浏览：技能 / 命令 / 子代理 / 记忆 / 插件 × 三引擎（用户 / 内置 / 项目）；MCP 列表（三引擎配置真相位置）+ 探活（仅 http GET，不 spawn stdio）；markdown 预览 + 系统程序打开 |
| 自动化 | `~/.prism/routines.json` 定时任务（五字段 cron 自研解析）；应用内 30s tick 调度，到点用引擎运行轴跑提示词；执行日志 + 状态回写，sessionId 一键回看；唤醒合并补一次、关停期间错过不补 |
| 通知 | 回合完成 / 失败 / 待审批 → 顶栏铃铛持久面板 + 系统通知（非前台才发，权限拒绝静默降级）；例行任务结果与权限同样进通知 |
| 托盘 | macOS 菜单栏：图标旁待审批计数 + 菜单状态行 / tooltip（前端合并工作台 + 归档视图实时推送，按 sessionId 去重）；菜单含打开 / 退出；**关窗 ≠ 退出**（隐藏窗口，进程与调度继续跑）；Dock 点击唤起 |
| 三引擎 | OpenCode（全功能）；Codex（浏览 + 运行：发消息 / 流式 / 插话 / 打断 / 审批卡片 / 新建会话）；Claude Code（历史浏览，运行轴待 CLI）；新建会话组合器引擎三选一（Claude 灰显并注明原因） |
| 打磨 | 自研图标、设置面板（双引擎健康）、输入框自适应 |
| 用量 | 第 6 视图：跨引擎 token 明细（输入 / 输出 / 思考 / 缓存读写）+ 每日柱状曲线 + 按引擎 / 模型分列 + 估算成本（内置价格表，未收录模型标注）；OpenCode / Codex / Claude 本地档案只读统计，不存副本；配额实时值（M13b）：Codex 订阅窗口一次性 app-server 读取（API Key 如实标注无订阅配额）；Claude 配额待 CLI |
| 打包 | `.app` + `.dmg`（未签名；Gatekeeper 打开流程见 README）；自动更新已接线（Tauri updater，发布源 = gh-pages 镜像 + GitHub Releases 兜底）；v0.2.0 → **v0.2.3 已发布**（2026-10-17，含 M14–M19 + 组合器升级；产物签名已用公钥独立验证）；升级验收进行中：本机 0.2.0 需手动装一次新版 |

## 1. 差距对照（现状 → 去向）

| 能力 | Prism 现状 | 去向 |
|---|---|---|
| 工作台多列 / 列宽 / 持久化 | ✅ M6 | —— |
| 组合器（引擎切换 / 模型 / 档位 / 文件引用 / 斜杠命令 / shell 直跑） | ✅ M18 核心（2026-10-17）：新建会话组合器 + 会话内发送框增强；图片输入、输入排队留后 | 剩余：图片输入 · 输入排队（随行补） |
| 监控轨（卡内审批）/ 标签页 / 全景图导出 | ❌ | 远期池（监控轨优先级最高） |
| 赛马（广播 + 并排 + token HUD） | ✅ M17（2026-10-17） | 第 8 视图：多引擎同时开跑，答案 / 用时 / token / 成本并排；可中断、可重回档案 |
| 归档排序 / 时间范围 | ✅ M7 顺行 | —— |
| 星标 / 标签（宿主 metadata） | ✅ M15（2026-10-17） | `~/.prism/annotations.json` 原子写；改名合并 / 换色 / 删除 / 星标筛选 |
| 搜索 | ✅ M7 FTS5 trigram | 选型：FTS5 trigram + <3 字符 LIKE 回退（覆盖中文两字词硬伤）；数据量变大再评估无索引内存子串路径 |
| 会话详情块体系 | 🟡 文本 / 推理 / 工具 / diff / 权限 | 按需随行 |
| 文件台账 + git 快照条 | ❌ 已撤下（2026-10-18 移除 M16 账本） | git 快照条留远期 |
| 产物预览（HTML/SVG/图片） | ✅ M14（2026-10-17） | 本地文件卡片 + 沙箱预览（禁脚本禁联网）+ 图片缩略图/放大 |
| 文件变更追踪 + 预览 | ✅（2026-10-18） | 会话目录相对 HEAD：文件列表（状态 / ± 行数）+ 单文件 unified diff + 当前文件预览（行号 / JSON 上色）；以 git 工作树为准，不解析各家工具调用 |
| 多引擎 | 🟡 OpenCode + Codex 全功能；Claude 历史浏览 | **M9 运行轴（待 CLI 安装）** |
| 运行轴事件模型（generation/sequence、配置变即重启） | 🟡 双引擎事件已归一化成同形 | 配置变更即重启随行补（M9 或之后） |
| 权限交互语义（四档 / 问答卡 / 计划卡） | 🟡 三档（once/always/reject） | 随引擎运行轴逐步补（问答卡语义 = allow + updatedInput.answers） |
| 工坊（技能 / 命令 / MCP / 记忆） | ✅ | M10a 资产浏览 + M10b MCP 列表 / 探活已实现（2026-10-17，见 M10）；写操作（增删 MCP）后置，不做 |
| 自动化（定时任务） | ✅ | M11b 已实现（2026-10-17，见 M11）：应用内 30s tick 调度（不用 launchd，理由见 M11）；launchd / 后台常驻后置 |
| Hooks / Turn Signal | ❌ | 远期（依赖 Claude 接入，M9 后再看） |
| 通知三层 | ✅ | M11a 已实现（2026-10-17，见 M11）：单条内存事件流 + 系统通知（非前台才发 + 静默降级）；持久面板为事件流派生视图（不复制引擎状态），跳转以选中态为准 |
| 设置多标签 | 🟡 双引擎状态面板 | 随行逐步扩展 |
| 托盘 | ✅ | M12a 已实现（2026-10-17，见 M12）：Tauri 内置 tray 轻版——状态行 / tooltip / 待审批计数、点击唤起、关窗 ≠ 退出；不引入独立常驻二进制 + LaunchAgent |
| 小组件 | ❌ | 远期池 |
| 自动更新 | ✅ | M12c 已接线（2026-10-17）：Tauri updater；更新清单镜像 gh-pages（Fastly CDN，首选）+ GitHub Releases 兜底，`scripts/release.sh` 发版并生成 `latest.json`；v0.2.0 / v0.2.1（M13）/ v0.2.2（验收目标）已发布（2026-10-17），升级验收进行中 |
| 主题系统 / i18n | 🟡 自研单主题 + 中文 | i18n 架构预留后置（文案集中化重构，随第一次语言需求启动）；主题远期 |
| 用量 / 成本统计 / 配额 | ✅ 用量 + 成本统计 + Codex 配额 | M13a 已实现（2026-10-17）：第 6 视图「用量」跨引擎 token / 每日曲线 / 成本估算（估算口径见 M13）；M13b 已实现（2026-10-17）：Codex 配额实时值（app-server 一次性读取主 / 次窗口 + credits + 套餐，5 分钟缓存 + 手动刷新；API Key 如实标注无订阅配额）；Claude 配额待 CLI（与 M9 运行轴同批） |
| MCP 双向桥（宿主能力给 agent 用） | ❌ | 远期池（宿主即工具：把审批 / 搜索 / 任务能力暴露给会话内 agent） |
| 菜单 / 快捷键 / 开机自启 | 🟡 Cmd+W 关闭即隐藏（关窗 ≠ 退出）、Cmd+Q 退出 | 视图快捷键随行；开机自启远期（托盘常驻 + 关窗不退已覆盖多数场景） |

## 2. 引擎路线修订（重要）

原计划：OpenCode → Claude Code → Codex。
**修订为：OpenCode → Codex → Claude Code。** 依据 2026-10-17 实机核实：

- **Codex 全链路可测**：`~/.codex/sessions` 已有 41 条真实 rollout 档案；
  `/Applications/Codex.app/Contents/Resources/codex` 内嵌 codex-cli 0.133.0（arm64），
  `codex app-server` 运行轴可端到端真机验证。
- **Claude Code 本机无数据无 CLI**（`~/.claude` 不存在、PATH 无 `claude`）：
  浏览层走样例档案开发验收；运行轴等安装 CLI 后补真机复测（或与用户约时间装一个）。
- 若用户先装好了 `claude` CLI，可把 M8 / M9 对调，不受影响。

## 3. 开发规划（M6–M13）

### M6 工作台：多列并行指挥 ★核心补齐
> 状态：已实现（2026-10-17）并提交。
- 顶部区块切换：**归档** ⇄ **工作台**；一屏多列（发送 / 插话 / 停止 / 审批全保留）。
- 列头 = 标题 · 状态 · 关闭；开列 = 新建会话或归档「在工作台打开」。
- 全局提示：任一会话等审批 → 顶部汇总角标；列宽可拖拽；布局持久化。
- 本期不做：标签页、赛马、监控轨、截图（→ 远期池）。

### M7 搜索：本地全文检索
> 状态：已实现（2026-10-17）并提交；真机索引冒烟通过，前端 59 测全绿。
- 自建索引 `~/.prism/search.db`（FTS5 trigram：中文子串即查即中；
  两字符以下查询回退 LIKE 扫描），从 `opencode.db` 增量抽取（Prism 自有数据，不写引擎库）。
- 搜索页：关键词 → 结果（项目 / 时间 / 高亮片段）；点击定位到消息并短暂高亮。
- 顺行：归档列表排序 + 时间范围筛选。

### M8 Codex 引擎接入（浏览 + 运行轴）
> 状态：**已实现（2026-10-17）**：浏览层 + 运行轴（`codex app-server` 长活进程）。
> Rust 45 测全绿；真机集成测试 `drives_real_app_server` 通过（内嵌假模型驱动真
> app-server：文本流式 / 沙箱命令 / 升级审批→放行→落盘 / 归档清理，全程 0.4s）。
- 数据：`$CODEX_HOME/state_*.sqlite`（按 schema 探测 `threads` 表：标题 / 用量 / rollout 路径）
  + `sessions/**/rollout-*.jsonl` 宽容解析（去重规则：`event_msg/user_message` +
  `response_item` 的 assistant message / reasoning；`function_call_output` 按 call_id 回填状态）。
- 会话 id 约定：`codex:<thread-uuid>`；项目 id = `codex:<cwd>`；Rust 层按前缀分发，前端 API 零分支。
- 运行轴实现：行分隔 JSON-RPC —— `initialize`（声明 experimentalApi）→
  `thread/start`（`ephemeral=false` 立即写 state 库，前端"新建后刷新"直接可见）或
  `thread/resume`（历史会话首条消息前自动加载）→ `turn/start`（空闲）/ `turn/steer`
  （执行中插话，带 expectedTurnId）/ `turn/interrupt`（打断，`turn/completed.status=interrupted`）。
  通知归一化成 OpenCode 同形 `{type,data}` 事件（`session.text|reasoning.delta/ended`、
  `session.tool.*`、`session.execution.*`）；审批反向请求按 `availableDecisions` 把前端
  三档映射为 accept / acceptForSession / decline（缺省回退 accept / cancel），
  `serverRequest/resolved` 广播 `permission.replied` 收卡；问答卡 v1 回空答卷不阻塞。
  配置完全继承 `~/.codex/config.toml`（模型 / 审批策略 / 沙箱），Prism 不写入不覆盖。
- 验收：真机发消息 / 流式 / 中断（已实证）；工作台两引擎并行可用；
  浏览层验收（36 项目 / 38 条记录 / token 汇总 / 工具详情）已达成。

### M9 Claude Code 引擎接入
> 状态：**浏览层已实现（2026-10-17）**：`~/.claude/projects` JSONL 解析、三引擎合并
> IPC、项目角标（CC）/ 设置面板 Claude 块；Rust 55 测全绿 + 样例档案（2 项目 /
> 3 会话：工具回填 / structuredPatch diff / 失败结果 / 子代理噪音）全链路通过。
> **运行轴待本机安装 `claude` CLI 后接入**（验收分两段执行）。
- 数据：`~/.claude/projects/<编码 cwd>/<会话 uuid>.jsonl`；`CLAUDE_CONFIG_DIR`
  官方变量优先（与 CLI 视角一致）；cwd 有采信守卫（编码回目录名一致才认，防漂移），
  `agent-*.jsonl`（子代理档案）排除；`isSidechain` / `isMeta` 不进主时间线；
  `file-history-snapshot` 等大行快筛跳过。
- 标题：custom-title > ai-title > summary > 首条用户消息（首行截 80 字符）；
  用量四类缓存字段求和（含子代理，真实计费口径）；`ToolDetail` 输出 200k 字符截断。
- 工具映射：名称统一小写（Bash→bash、Edit→edit…）对齐 OpenCode 卡片；`tool_result`
  按 `tool_use_id` 回填状态（`is_error`→error）；Edit/Write 的
  `toolUseResult.structuredPatch` 转统一 diff 进 `metadata.files[].patch`（复用卡片渲染）。
- 运行轴：`claude` CLI stream-json（待安装后真机验证，验收分两段执行）。
- 届时按需补：AskUserQuestion / PlanApproval 交互语义（问答卡 / 计划卡）。
- 验收：样例档案时间线正确（已达成）；安装 CLI 后执行真机验收清单。

### M10 工坊：引擎资产
- ✅ M10a 资产浏览（2026-10-17 实现并提交）：
  - 纯函数发现器 `engines/workshop.rs::collect(roots, project_cwds)`：命令层只装配家目录与项目列表，
    单测注入 fixture 目录；全程只读，不写引擎任何目录。
  - 引擎口径（源码/官方文档核实）：OpenCode `~/.config/opencode/{skills,agents,commands}`（含遗留单数
    `command/`）+ 项目 `.opencode/`（技能兼容 `.claude/skills` 归 Claude 侧展示避免重复）；Codex
    `~/.codex/skills/<名>/SKILL.md`（`.system` 作内置）+ 插件缓存 `plugins/cache/<市场>/<插件>/<版本>`
    + `memories/`；Claude Code `~/.claude/{skills,commands,agents}` + 项目 `.claude/`；记忆 = `AGENTS.md`
    （Codex / OpenCode）与 `CLAUDE.md`（Claude），用户级 + 项目级。
  - 元数据：frontmatter `name` / `description`（含 `>` / `|` 折叠块标量），缺省回退正文首行；插件描述取
    `plugin.json`；技能录两层（平铺 `*.md` + `<名>/SKILL.md`），命令 / 子代理目录递归并入名称（`team/review`）。
  - IPC：`workshop_assets`（总览）/ `workshop_read`（预览，200k 字符截断）/ `open_path`（系统默认程序打开）。
  - 前端：顶栏第 4 视图「工坊」（AppView 扩 `workshop`；`prism-view` 持久化），三栏 =
    引擎×种类导航（计数）| 列表（过滤 + 归属/时间）| 详情（markdown 预览 + 打开文件）；各引擎空态有引导文案。
  - 验收：本机真机 87 条（Codex 12：技能 6 / 插件 5 / 记忆 1；Claude 项目技能 75；OpenCode 0 → 空态）；
    Rust 63 测（含 4 条 workshop）+ vitest 63 + vue-tsc 0。
- ✅ M10b MCP 列表与探活（2026-10-17 实现）：
  - 真相位置：OpenCode `opencode.jsonc|json` 的 `mcp.servers`（v2 口径 local/remote、`disabled`；兼容旧
    `enabled`；JSONC 注释与尾随逗号容忍）+ 项目根同名文件；Codex `~/.codex/config.toml`
    `[mcp_servers.<名>]`（极简 TOML 子集：单行/多行数组、`.env` 子段只取键名）；Claude `~/.claude.json`
    顶层与 `projects.<cwd>`（local scope）+ 项目 `.mcp.json` + 用户/项目 `settings.json`。
  - 安全边界：环境变量只显示键名（不落值）；stdio 仅查命令可解析性（绝对路径 / PATH），**绝不 spawn**；
    探活仅对 http url 发 GET（4s 超时，任何 HTTP 响应=可达，传输错误=不可达）。
  - IPC：`workshop_mcp` / `mcp_probe`；前端工坊加「资产 | MCP」模式切换——左导航（引擎计数）、列表
    （状态圆点：绿=可达 / 红=失败 / 空心=stdio）、详情（命令或地址、命令状态、环境变量键名、探活按钮、打开配置）。
  - 验收：本机 3 个 stdio 服务（OpenCode imessage；Codex node_repl / stitch）；Rust 63 测（含 4 条 MCP）
    + vitest 65 + vue-tsc 0。
- 写操作（增删 MCP）后置，不做；Hooks 总览不在本期。
- 边界：资产只读 + 系统默认程序打开（无新建 / 编辑 / 运行）。

### M11 自动化与通知（2026-10-17 完成：M11a 通知 + M11b 定时任务）

- ✅ M11a 通知：
  - 事件源：回合完成 / 失败（含错误摘要）/ 待审批（打断是用户主动动作，不打扰）；例行任务结果与运行中权限请求由 Rust 广播 `routine-event`。
  - 应用内持久条：顶栏铃铛 + 未读角标 + 面板（点击跳回会话并高亮定位、清空）；条目是内存事件流，不做引擎状态副本。
  - 去重：同键 10s 窗口合并（归档 + 工作台双实例会对同一条引擎事件各上报一次）；权限请求按 request id 永久去重（引擎重连可能重放）。
  - 系统通知：tauri-plugin-notification；窗口非前台才发；首次询问权限、明确拒绝后本次运行不再请求；一切失败静默降级（应用内条目不受影响）。
- ✅ M11b 定时任务：
  - 定义与日志：`~/.prism/routines.json`（单一事实源，原子写：临时文件 + rename）；日志 `~/.prism/routine-logs/<id>/<startedAt>.json`。
  - cron：自研五字段解析与求值（Vixie 语义：日 / 周双限定取 OR、周日 0|7、支持 `*/n` 与列表；1500 天无解判"不可达"），不引三方 cron 库。
  - 执行器：走引擎运行轴（新建会话 → 发提示词 → 等终态）；工作区固定 `~/.prism/routine-workspace`（数据主权：不动用户项目目录）；15 分钟超时先打断再按 timeout 收口；运行中权限请求立即进通知；日志带 sessionId，界面可"打开会话"回看完整过程。
  - 互斥：进程内 running 集合 + 触发前查重（调度与手动触发全在同一进程内，无需跨进程文件锁）。
  - 界面：第 5 视图「自动化」——任务列表（启停状态点）+ 详情（计划人话描述 / 下次触发倒计时 / 提示词 / 执行日志）+ 新建 / 编辑弹窗（预设 + 自定义 cron）。
  - **调度承载决策：应用内 tick（30s），不用 launchd**。理由：触发与运行轴同进程，免安装 plist / 免独立 runner 二进制、免二进制路径漂移（dev 与打包路径不同是独立 runner 方案的长期坑）；代价是应用未运行时任务不跑。若以后要"退到后台也能跑"，再评估 launchd 或 Tauri 侧后台常驻。
  - **错过语义（与本机睡眠场景）**：合盖休眠时进程挂起、唤醒后按 tick 窗口**合并补一次**（近似 launchd StartCalendarInterval 的平台惯例）；应用关停期间的错过**不补跑**（启动即重置窗口，避免开机连环触发）。
- 验收（真机，2026-10-17 10:15）：探针任务「每分钟」→ 调度器到点触发 → opencode 在 routine-workspace 完成回合（4 秒，会话自动标题「收到确认」）→ 日志 `status=succeeded` + 定义回写 `lastTrigger / lastStatus / lastSessionId` → 前端收到 `routine-event`（通知中心可见）。
- 本期不做：睡眠唤醒（pmset 提权）、Hooks 总览、任务结果的 webhook / IM 渠道。

### M12 平台化 + v0.2 发布
> 状态（2026-10-17）：**M12a 托盘 ✅ / M12b v0.2.0 打包 ✅ / M12c 自动更新 ✅（待首版发布验收）**。

- 托盘（✅ M12a）：Tauri 内置 tray 轻版（自研，不引入独立常驻二进制 + LaunchAgent）——
  单色模板图标（自研光栅化脚本 `src-tauri/icons/tray-icon.cjs`，可复跑）
  + 菜单「状态行 / 打开 Prism / 退出 Prism」；状态由前端合并「工作台 + 归档视图」后
  经 `tray_status` IPC 实时推送（按 sessionId 去重，防同一会话两处显示虚高）；
  有待审批时图标旁直接显示数字，tooltip / 菜单行同步文案。
- 关窗 ≠ 退出（✅ M12a）：`CloseRequested` 拦截为隐藏窗口——进程、定时调度、
  通知继续跑；退出走托盘菜单或 Cmd+Q（`ExitRequested` 置位放行）；
  Dock 点击（`Reopen`）唤起主窗口。
- 打包（✅）：三处版本号升 0.2.0（`package.json` / `tauri.conf.json` / `Cargo.toml`）；
  `pnpm tauri build` 产出 `.app` + `.dmg`（未签名——无 Apple Developer ID，暂不签名公证；
  README 写清 Gatekeeper「右键 → 打开」流程）。
- 自动更新（✅ M12c）：Tauri updater（stable 通道）。签名密钥已生成
  （私钥 `~/.prism/updater.key` 仅本机、不入库；公钥写入 `tauri.conf.json`）；
  更新端点首选 gh-pages 镜像 `memmori1.github.io/Prism/latest.json`（Fastly CDN，
  github.com 直连不稳定时的可达路径），Releases `latest/download` 兜底（公开仓库免鉴权）；
  发版一条命令 `scripts/release.sh`（产出 `.app.tar.gz` + `.sig` + `latest.json`，
  `--publish` 走 gh：创建 Release **并同步 gh-pages 镜像**）；设置面板「更新」区块：
  检查更新 / 下载并安装 / 重启生效。
- 菜单与快捷键：Cmd+W（关窗 = 隐藏）/ Cmd+Q 已按 macOS 惯例工作；
  视图快捷键与 i18n 架构预留后置（文案集中化随第一次语言需求启动）。
- 验收：他人机器可安装（README 流程）✅；v0.2.0–v0.2.2 已发布（2026-10-17）✅；
  自动升级验收 🔄 进行中（v0.2.1 已含 M13；装上后应用内「检查更新」应升级到 v0.2.2，
  按 `docs/RELEASE.md` §4）。

### M13 用量面板：跨引擎 token 统计与成本估算
> 状态（2026-10-17）：**✅ M13a（用量 + 成本统计）与 M13b（Codex 配额实时值）已实现；Claude 配额待 CLI（与 M9 运行轴同批）**。

- 数据源（全部只读，口径已归一可加总）：
  - OpenCode：`session_v2` 的 `tokens_*` 列（输入 / 输出 / 思考 / 缓存读写天然分列）；
  - Codex：`~/.codex/sessions/**/rollout-*.jsonl` 尾部最后一次 `token_count`
    （累计值；OpenAI 语义里缓存命中是输入的子集 → 归一为「未缓存输入 + 缓存读」）；
  - Claude：`~/.claude/projects/**/*.jsonl` 的 `message.usage` 累加（含子代理，
    与浏览层口径一致）。
- 视图：第 6 视图「用量」——范围（今日 / 7 日 / 30 日 / 全部）、总量卡片（分项
  明细）、每日柱状图（「全部」最多回看 60 天）、按引擎 / 模型分列（token + 会话数 + 成本）。
- 成本估算：内置价格表（Claude 4.x 系 / GPT-5 系 / `-free` 后缀记 0；美元 / 百万 token，
  思考按输出价计）；未收录模型显示「未收录」且不计入合计。
- 归档口径：按「会话归档时间」过滤与归档（OpenCode = 会话创建时刻；Codex = 最后一次
  `token_count` 事件时刻；Claude = 档案内最后时间戳）；会话级数据不跨天拆分。
- ✅ M13b 配额实时值（Codex 段，2026-10-17）：
  - 取数：一次性拉起 `codex app-server`（stdio JSON-RPC，查询后立即回收，不留常驻）→
    `account/read` 判定认证类型 → ChatGPT 登录才 `account/rateLimits/read` 读单桶
    `rateLimits`（`primary` / `secondary` 窗口：usedPercent + 窗口时长 + resetsAt
    （协议为 Unix 秒，归一毫秒）+ `credits` + `planType`）；API Key 如实标注
    「无订阅配额（按量计费）」；CLI 缺失 / 超时 / 解析失败均有独立状态与说明。
  - 缓存：进程内 5 分钟 TTL（`quota_summary`），前端「刷新配额」强制刷新。
  - 前端：「用量」视图顶部「配额（实时）」区块——窗口进度条 + 已用 % +
    重置倒计时（60s 心跳）+ 套餐 chip + 更新时刻；Claude 一栏显示「待安装 CLI」。
  - 口径核对：本机 codex 0.133.0 真机探针 + 官方生成 schema（`GetAccountRateLimitsResponse`
    / `RateLimitSnapshot` / `RateLimitWindow`）；本机为 API Key 认证，实测
    `no_subscription`（不造数）；ChatGPT 登录场景待用户机器回归。
- 本期不做（→ 远期池）：按项目分组、导出报表、预算告警；Claude 配额（待 CLI，与 M9 同批）。

### M14+ 追赶计划（对照 Monet 2.2.23，2026-10-17 盘点）

> Monet（同类产品，MIT、已公证、Homebrew 收录，v2.2.23）功能面显著更厚。本轮逐模块
> 盘点（README / 源码结构 / 发布说明），按「效果 ÷ 成本 × 本机可验证性」排追赶顺序。
> 口径不变：引擎数据只读、Prism 增值数据进 `~/.prism/`、干净房间、不照抄任何代码/文案。
> Monet 官方支持 Claude Code + Codex（OpenCode 适配器在其源码树中开发；Prism 已先行支持）。

- **M14 阅读与运行信息增强**（2026-10-17 完成）：
  LaTeX（KaTeX：行内 / 块级 / 反斜杠形式，trust 关闭）· 回复内 HTML/SVG 沙箱渲染
  （iframe sandbox 禁脚本、源码可展开；超 200 KB 降级为普通代码块）· 交付物就地预览
  （本地 HTML/SVG/图片链接 → 卡片 + 图片缩略图 + 点击放大弹窗；后端类型白名单 +
  大小上限，静态加载、禁脚本禁联网）· 每轮 token 四联（输入 / 缓存读 / 输出 / 思考 +
  成本）+ 上下文用量条（三引擎均有数据；窗口来自 Codex rollout）· 长会话「回底浮标」
  （含新消息角标）。
- **M15 档案增值数据**（2026-10-17 完成；只写 `~/.prism/annotations.json`，不碰引擎原档）：
  星标（列表内点星 / 头部星标 / 星标筛选）· 标签（新建回车即挂、点色块换色、✎ 重命名、
  同名即合并、拖删二次确认；会话头部标签行 + 列表色点）· 会话标题编辑（双击或 ✎，
  空串恢复引擎标题）· Worktree 会话聚合（项目列表同仓 ≥2 个 worktree 收组 + 分支标注，
  会话面板标题行分支徽标）。存储：原子写、读盘容错、空记录自动清理；前端全量替换式缓存。
- **M16 文件账本**（2026-10-17 完成）：新「🗂」抽屉（会话头部）——按文件聚合的
  操作清单（改 / 写 / 删 / 读 + 增删行数合计，只看改动筛选、相对路径显示）；
  展开逐次操作（时间 + 工具 + ±），点一下跳回时间线对应轮次并高亮（完整 diff
  在工具卡片里）。三引擎提取：OpenCode 用 `edit.metadata.files`（补丁 + 增删行数，
  旧数据回退输入文本估行）· Codex 解析 `apply_patch` 的 V4A 补丁（contain 多种
  参数形态：patch / cmd / command / repr 嵌套）+ shell heredoc 形态 · Claude
  Edit / MultiEdit / Write / NotebookEdit / Read（tool_result 行不切记录）。
  纯读引擎档案；事件上限 5000 条（超出保留最近并标注）。
  （2026-10-18 撤下：面板、IPC 与三引擎提取代码整体移除。同日晚些时候以「变更」抽屉回归，
  但换了口径：不再逐条解析工具调用，直接读 git 工作树——见功能矩阵「文件变更追踪 + 预览」。）
- **M17 赛马模式**（2026-10-17 完成）：新「🏁 赛马」视图（第 8 个）——选工作目录与\n  1–4 个参赛引擎（OpenCode / Codex 任意组合），同一问题先并行新建各自会话、\n  再同刻开跑；每列流式显示（复用运行轴事件与 LiveTurn）、列头 HUD 显示\n  状态 / 用时 / token 四联与成本，完成最快打 🏁；可单列中断 / 全部停止 /\n  同题再跑 / 一键切回归档档案；事件丢失有轮询兜底（完成即冻结最终记录）。
- **M18 组合器升级**（核心完成 2026-10-17；图片输入与输入排队留后）：
  **新建会话组合器**（工作台「新建会话」对话框）：引擎卡片（OpenCode / Codex 可选，Claude
  灰显并注明原因）× 项目 × 模型 × 推理档位（OpenCode variant / Codex effort，`variant`
  字段统一承载）× 首条指令）+ @ 文件补全（Rust `git ls-files` 索引 + 打分排序；opencode
  结构化附件、codex 折算文本清单）+ / 斜杠命令（引擎目录实时拉取）+ ＋Git 上下文
  （分支 / 未提交变更 / 最近提交一键带入）+ 附件 chips；提交即创建并可立即发送。模型 / 档位
  在创建后首条消息应用（opencode 先 `POST /api/session/{id}/model`；codex `turn/start` 带
  `model` + `effort`）；**不选 = 继承引擎默认**（尊重 `~/.codex/config.toml`）。
  **会话内发送框增强**：@ 文件防抖补全 · / 命令弹层 · `!命令` 本机 shell 直跑（引擎原生
  shell 接口，不发给模型）· 模型 / 档位 chip（按引擎记忆于 localStorage，下一轮生效）。
  留后：图片输入 · 输入排队（流式期间提交）。
- **M19 首页仪表盘**（2026-10-17 完成）：新「首页」视图（顶栏第一个，作为新装默认页）——
  token 脉搏（今日 / 近 7 天 / 峰值日 + 28 天迷你柱）· 工作节奏（按会话开始时刻的
  24 小时分布，后端 `usage_summary` 新增 `hours` 桶）· 28 天热力图（按日强度 0–4 档，
  周一…周日网格）· 模型分布（跨引擎 top 5：占比条 + 成本，一键跳用量面板）·
  项目活跃（跨引擎按工作目录合并：会话数 / 引擎徽标 / 最近活动，点击跳归档）。
  全部复用 M13 用量后端与只读统计口径，纯展示助手抽成 `home.ts`（带单测）。
- **M20 常驻信息面**：托盘配额分区展示（Codex 已有后端；Claude 段待凭据）·
  权限体检（主应用 / 后台两本账）。
- **M21 MCP 服务桥**：内置 MCP server，把历史检索 / 定时任务 / 日志暴露给会话内 agent。
- **M22 质感与主题**：亮 / 暗双主题 + 主题库 · 启动无白闪 · PerfHud（调试快捷键）。
- **M23 终端会话识别**：识别本机终端里已跑的 CLI 会话并实时跟看 / 终止
  （只读识别，操作显式）。
- 延缓 / 条件项（大件或依赖）：自然语言 → 定时任务 · AI 标题 / 标签 / 摘要与
  自然语言搜索（BYOAI，走用户自有渠道）· 睡眠唤醒执行 · 异步任务面板 ·
  Runner 托管 dev server · 桌面小组件（Swift WidgetKit）· 多语言 / AI 界面翻译 ·
  工具三档显示与过程合并 · 虚拟滚动 · 全景图导出 ·
  Apple 公证 + Homebrew（阻塞：无开发者账号）· Windows 支持。

## 4. 远期池（不排期，按兴趣取用；文件台账 / 赛马 / MCP 双向桥 / 主题系统 / 图片输入 / 斜杠命令 / 输入队列 / 产物预览等已转入 M14+ 追赶计划）

监控轨（卡内审批）· 工作台标签页 · 全景图导出 · 赛马 · 小组件 · 主题系统与主题库 ·
AI 元数据（标题 / 标签 / 摘要）· 智能搜索（AI 归纳）· **文件台账（含 git 快照条）** ·
异步任务面板 · Runner 跑单（孤儿清扫双校验）· 附件 / 图片输入 · 斜杠命令面板 ·
会话 find bar / 锚点导航 / 置顶提示 · 输入队列（流式期间提交）· 权限四档与问答/计划交互卡 ·
运行轴 generation 热重启 · Turn Signal · **MCP 双向桥（宿主能力暴露给 agent）** ·
睡眠唤醒 · 多语言 · 产物预览（HTML/SVG/图片，沙箱：CSP 白名单 + 脚本按 nonce 激活）。

## 5. 明确不做

- 主题 AI 生成与社区投稿。
- cc-switch 导入（第三方配置迁移链路，与 Prism 无关）。
- 睡眠唤醒 pmset 提权。
- 独立托盘 / 小组件二进制 + App Group 那套系统级基建（Prism 用 Tauri 内置能力做轻版）。
- webhook / IM 通知渠道（只做系统通知 + 应用内）。
- 遥测 / 账号系统（零遥测）。
- 复刻第三方应用的视觉资源 / 文案 / 代码（干净房间原则）。
