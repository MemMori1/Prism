# DAG 编排器（设计稿）

> 目标：把一个目标拆成有依赖的任务图，每个节点交给不同 Agent、在各自的
> 工作空间里跑，工具按「公用 / 私有」授权，整张图能实时看见、能画出来。
>
> 本文是设计稿，不包含实现。代码位置与现有模块的对应关系都写在文里，
> 落地时可以按第 11 节拆里程碑。

## 0. 一句话

**Plan（LLM 生成的 DAG）→ 人工确认 → Scheduler 按依赖调度 → 每个节点是一个
独立会话（自己的上下文 + 自己的工作区 + 自己的工具集）→ 事件流驱动画布。**

和现有 `task` 子代理的区别：

| | `task` 子代理 | DAG 节点 |
|---|---|---|
| 编排 | 主 Agent 临时决定，扁平 | 显式图，可 review、可重跑、可恢复 |
| 并行 | 一次一个 | 多层并行（受 `maxParallel` 限制） |
| 交付 | 只回一份报告 | 报告 + 工作区 diff / 分支 / 结构化产物 |
| 可见性 | 工具卡里的折叠轨迹 | 画布 + 节点详情 + 独立会话 |
| 复用 | 不可 | 计划可保存、可当模板 |

## 1. 总体架构

```
                       ┌─────────────────────────────┐
  用户目标 ──► Planner │ LLM 出 JSON → 校验 → 修复循环 │──► DagPlan（草稿）
                       └─────────────────────────────┘         │
                                                               │ 人工确认 / 编辑
                                                               ▼
  ┌──────────────┐   读/写    ┌──────────────┐   调度   ┌──────────────┐
  │  DagStore    │◄──────────►│  Scheduler   │─────────►│  NodeRunner  │
  │ ~/.prism/dags│  快照/事件  │ ready set    │          │ native/engine│
  └──────────────┘            │ 并发/重试/取消│          │ shell/gate   │
         ▲                    └──────┬───────┘          └──────┬───────┘
         │                           │ dag-event               │
         │                           ▼                         ▼
  ┌──────┴───────┐            ┌──────────────┐          ┌──────────────┐
  │  DagPane     │◄───────────│  Tauri emit  │          │ Workspace    │
  │ 画布/详情/列表│            └──────────────┘          │ worktree/... │
  └──────────────┘                                       └──────────────┘
```

三条边界：

1. **计划是数据，不是代码。** DAG 定义可存 JSON、可手改、可导出；运行时的
   能力（执行器 / 工作区 / 规划器）才是插件。
2. **节点 = 会话。** 每个节点在引擎侧有真实 session（归档可见），DAG 只是
   在会话外面加了一层「谁依赖谁、产物怎么传」。
3. **事件是唯一真相。** 前端画布是 `dag-event` 的投影；刷新时用
   `dag_run_get` 拉快照，不靠本地状态拼。

## 2. 数据模型

### 2.1 Rust 形状（`src-tauri/src/dag/model.rs`）

```rust
pub struct DagPlan {
    pub version: u32,              // schema 版本，便于迁移
    pub id: String,                // dag-<时间>-<序号>
    pub name: String,
    pub goal: String,              // 用户原始目标
    pub project: String,           // 项目目录（绝对路径）
    pub defaults: DagDefaults,
    pub nodes: Vec<DagNode>,
    /// 画布坐标；自动排版只在没有坐标时用。
    pub layout: HashMap<String, Position>,
}

pub struct DagDefaults {
    pub agent: String,             // 默认 Agent profile id
    pub permission: PermissionMode,
    pub max_parallel: u8,          // 默认 2
    pub failure_policy: FailurePolicy, // FailFast | Continue
    pub token_budget: Option<i64>, // 整个 run 的软预算
}

pub struct DagNode {
    pub id: String,                // 短、稳定、人可读：research / frontend / verify
    pub title: String,
    pub task: String,              // 提示词模板，支持 {{nodes.<id>.output}}
    pub depends_on: Vec<String>,

    pub agent: Option<String>,     // None = 用 defaults.agent
    pub executor: Option<String>,  // None = 由 Agent profile 决定
    pub workspace: WorkspaceMode,  // Shared | Worktree | Scratch
    pub tools: Option<ToolSelection>,
    pub permission: Option<PermissionMode>,

    pub retry: RetryPolicy,        // 次数 + 退避
    pub timeout_secs: Option<u64>,
    pub gate: Option<Gate>,        // 运行前 / 运行后要人点一下
    pub position: Option<Position>,
}

pub struct ToolSelection {
    /// None = 该 Agent 默认可见的全部公用工具。
    pub shared: Option<Vec<String>>,
    pub private: Vec<String>,      // Agent 私有工具 id
    pub deny: Vec<String>,         // 显式禁用（优先于 allow）
}
```

`WorkspaceMode`：

| 值 | 含义 | 适用 |
|---|---|---|
| `Shared` | 直接在项目目录跑 | 只读调研、单写者串行链 |
| `Worktree` | 每个节点一个 git worktree + 独立分支 | 并行写、需要 diff 的节点 |
| `Scratch` | 空目录，产物只能通过 report / patch 带出 | 纯生成、试验性任务 |

`FailurePolicy`：

- `FailFast`（默认）：一个节点失败 → 取消正在跑的兄弟节点，下游标 `skipped`。
- `Continue`：无关分支继续跑；失败节点的下游照样 `skipped`，最后汇总失败清单。

### 2.2 计划 JSON 示例

```json
{
  "version": 1,
  "id": "dag-20261022-01",
  "name": "给设置页加自动更新",
  "goal": "在设置里加自动更新，包含检查/下载/重启",
  "project": "/Users/me/project/prism",
  "defaults": { "agent": "general", "permission": "workspace-write", "maxParallel": 2, "failurePolicy": "failFast" },
  "nodes": [
    {
      "id": "research",
      "title": "摸清现有更新链路",
      "task": "调查 Tauri updater 在本仓库的接线、缺口与风险，输出文件:行号。",
      "agent": "explore",
      "workspace": "shared",
      "dependsOn": []
    },
    {
      "id": "frontend",
      "title": "设置页 UI",
      "task": "基于上游调研实现设置页的检查/下载/重启交互。\n{{nodes.research.output}}",
      "agent": "frontend",
      "workspace": "worktree",
      "dependsOn": ["research"]
    },
    {
      "id": "backend",
      "title": "IPC 与 updater 接线",
      "task": "实现后端命令与更新流程。\n{{nodes.research.output}}",
      "agent": "backend",
      "workspace": "worktree",
      "dependsOn": ["research"]
    },
    {
      "id": "verify",
      "title": "合并并验证",
      "task": "把上游两个分支的改动合到一个工作区，跑构建与测试，输出结论。",
      "agent": "verifier",
      "workspace": "worktree",
      "dependsOn": ["frontend", "backend"],
      "gate": { "before": "manual" }
    }
  ]
}
```

## 3. 执行语义

### 3.1 节点状态机

```
pending ──deps 全 succeeded──► ready ──取到并发槽──► running
                                                      │
                       ┌──────────────────────────────┼──────────────┐
                       ▼                              ▼              ▼
                   succeeded                       failed         cancelled
                                                      │
                                             还有重试次数？
                                             是 → ready（退避）
                                             否 → failed
pending/ready ──上游失败 / run 取消──► skipped
running ──等待审批──► waiting_approval ──批准──► running / 拒绝──► failed
```

Run 状态：`draft → queued → running → succeeded | failed | cancelled | interrupted`。

### 3.2 调度器

单 run 一个协调线程（与 `routines` / `tasks` 的线程模型一致，不把长任务塞进
Tauri async runtime）：

```
loop {
  if cancelled: 取消全部 running；把 pending/ready 标 skipped；收口
  ready = nodes where state==pending && deps 全 succeeded
  按 plan 顺序（或优先级）取前 (max_parallel - running) 个 → spawn 线程
  等任意一个节点终态事件 → 落盘 → 更新依赖计数
  若没有 running 且没有 ready → 收口（成功 / 失败 / 跳过）
}
```

要点：

- **并发上限**：默认 2，防止多个 Agent 同时烧 token / 抢 CPU。可在计划里改。
- **每节点超时**：默认 15 分钟（与 `routines::runner::RUN_TIMEOUT` 对齐）；
  超时先 `interrupt` 会话，再按失败收口。
- **重试**：只重跑该节点，不重跑上游；attempt 递增，产物目录按 attempt 分开。
- **预算**：run 级 token 软预算；每个节点结束累加，到顶后不再调度新的
  pending 节点，正在跑的不强杀（避免半截文件），最后标 `cancelled(budget)`。
- **快照**：`dag_run` 时把 DagPlan 深拷贝进 run；运行中编辑计划不影响本次 run。
- **恢复**：进程退出时把 running 节点标 `interrupted`；重启后不自动跑，
  用户点「继续」只重跑 interrupted / failed 节点，已 succeeded 的产物直接复用。

### 3.3 产物传递

每个节点固定产出：

```
report.md                最终报告（无工具调用的最后一条 assistant 正文）
changed-files.json       工作区 diff 摘要（worktree 模式下）
artifact/…               节点自己写的交付文件（可选）
```

下游拿产物的方式：

1. **显式占位符**：`{{nodes.<id>.output}}`、`{{nodes.<id>.report}}`、
   `{{nodes.<id>.files}}`。校验期解析占位符，引用了非祖先节点直接报错。
2. **默认注入**：没有占位符时，把**直接依赖**的 report 拼成「上游产出」段落，
   每段截断到 8k 字符，避免上下文爆炸。

报告是主交付物；结构化数据（JSON / patch）走 artifact 目录，不进提示词，
除非显式引用。

## 4. Agent 与「自己的空间」

### 4.1 Agent profile（`~/.prism/agents/user/<id>.json`）

Agent 是**配置**，不是代码：

```json
{
  "id": "frontend",
  "name": "前端工程师",
  "executor": "native",
  "model": "deepseek-chat",
  "systemPrompt": "你负责前端实现。改动保持仓库现有风格，先读再改。",
  "permission": "workspace-write",
  "workspace": "worktree",
  "tools": {
    "shared": ["read_file", "list_files", "grep", "edit_file", "run_command"],
    "private": ["figma__export"],
    "deny": ["web__fetch"]
  },
  "mcp": ["docs"]
}
```

内置 presets 编译在 `agents.rs` 里；同 id 的 user 文件覆盖内置，删文件即还原。
Planner 拿到的 Agent 目录就是这个注册表的投影。

### 4.2 工作区提供者

```rust
pub struct WorkspaceRequest {
    pub project: String,
    pub run_id: String,
    pub node_id: String,
    pub mode: WorkspaceMode,
    pub base_ref: Option<String>,
}

pub struct WorkspaceLease {
    pub path: String,
    pub provider: String,
    pub meta: serde_json::Value, // 分支名 / commit / 临时目录 id
}

pub trait WorkspaceProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn prepare(&self, req: &WorkspaceRequest) -> Result<WorkspaceLease, String>;
    fn finalize(&self, lease: &WorkspaceLease, outcome: &NodeOutcome) -> Result<WorkspaceSummary, String>;
    fn release(&self, lease: &WorkspaceLease);
}
```

- **worktree**：`git -C <project> worktree add -b prism/dag/<run>/<node>
  ~/.prism/dags/runs/<run>/ws/<node> <base>`。收尾时若 dirty，`git add -A &&
  git commit`，记录 commit 与 `git diff <base>..HEAD` 作为 artifact。工作树保留，
  用户 review 后自己合；不自动改用户当前分支。
- **shared**：一个 run 内按项目加写锁；写类节点串行，读类节点可并行。明确
  告知用户「这不是隔离」。
- **scratch**：`~/.prism/dags/runs/<run>/scratch/<node>`，空目录。
- **copy**（后置）：非 git 项目的兜底，复制目录并排除 `node_modules` /
  `target` / `.git`；成本高，只作为降级选项。

**依赖目录问题**：worktree 没有 `node_modules` / `target`。两种处理：

1. profile 配 `linkDirs: ["node_modules"]`，prepare 时从主仓 symlink 过去
   （只读依赖可以共享；`target` 不要共享，cargo 会锁冲突）。
2. 计划里加一个 `setup` 节点先跑安装；慢，但正确。

默认：JS 项目允许 link `node_modules`，Rust 项目不 link `target`。

### 4.3 上下文隔离

节点之间不共享消息历史，只共享显式声明的产物。这是和「多列工作台」最大的
区别：工作台是平行看，DAG 是**有向地传**。

## 5. 工具：公用 / 私有

### 5.1 现状与缺口

现有 `engines/native/tools.rs`：

- 内置工具是静态表；外部工具进进程级 `ToolRegistry`，按 `owner` 成批撤销；
- `EnabledTools` 是**全局**开关；`specs_for(enabled, max_permission)` 只按
  权限档过滤，没有「这个工具只给某个 Agent」的概念；
- 执行前有一次兜底校验（模型幻觉调用被禁工具会被拒），这个模式要保留。

### 5.2 扩展

```rust
pub enum ToolAudience {
    Shared,
    Private(String),   // agent profile id
}

pub struct ToolDef {
    // …现有字段…
    pub audience: ToolAudience,
}

pub struct ToolPolicy {
    pub allow: Option<Vec<String>>, // None = 全部可见
    pub deny: Vec<String>,
    pub permission: Permission,
}
```

解析顺序（同一个 Agent 看到的清单）：

1. 内置工具 + `Shared` 外部工具；
2. 加上 `Private(agent)` 的外部工具；
3. 去掉 `deny`；有 `allow` 时只留 allow；
4. 删掉权限档不够的（`Permission::within`）；
5. 同名的 private 覆盖 shared（内置名永远不可覆盖）。

执行时 `run_with_args` 用同一份解析结果再查一次，不信任模型「只调清单里的」。

native 侧的落点：`agent::send` 目前只接收 `approval_mode`，工具清单从全局
`config::load().tools` 读。要支持节点级策略，需要把这一轮的执行参数收成一个
`RunOptions { provider, model, approval, tools: ToolPolicy, systemPrompt,
maxTurns, tokenBudget }`，由 `send` 一路传到 `run_turns`；不传时保持现在的
行为。子代理（`subagent`）也复用同一份 `ToolPolicy` 推导工具集，不再只看
权限档。

### 5.3 MCP 作为私有工具的主要来源

- Agent profile 的 `mcp` 列表引用 `~/.prism/mcp.json` 里的 server；
- `dag.agents.mcp` 插件按 profile 懒启动 `StdioClient`，工具以
  `Private(profile)` 登记，名字带 server 前缀（沿用 `srv__tool`）；
- profile 删除 / 应用退出时 `ToolRegistry::release("agent:<id>")`；
- 外部工具默认 `enabled_by_default=false`，仍走审批卡（与现在一致）。

### 5.4 引擎执行器的边界（必须说清）

| 执行器 | 系统提示 | 工具白名单 | 私有工具 | 工作区 | 权限档 |
|---|---|---|---|---|---|
| `native` | ✅ | ✅ Prism 侧强制 | ✅ | ✅ | ✅ 执行器内拦截 |
| `codex` / `dsh` / `opencode` | ⚠️ 拼进首条 prompt | ❌ 引擎自己管 | ❌ | ✅ 通过 cwd | ❌ 见下 |

外部引擎的工具是它自己管的，Prism 只能通过会话参数（如 ACP `session/new`
的 `mcpServers`）影响，不能像 native 那样在每次调用前拦。UI 要在 Agent
编辑器里标出「此执行器不支持私有工具」，Planner 也不会给这类节点派私有工具。

**权限档不下传**：`read-only / workspace-write / danger-full-access` 只约束原生
执行器——dsh 的 sandbox、Codex 的 approval、OpenCode 的配置各有各的开关，
硬把 Prism 的档位翻译过去等于假装控制住了。要收紧就去改对应引擎自己的配置。

**无人值守的审批**：节点线程没人看屏幕，引擎弹 `permission.asked` 就没人点。
执行器不自动放行，只把它记进节点步骤（运行面板上能看见"卡在等授权"），
到硬上限（30 分钟）后打断并报超时。

**模型覆盖**：节点按 Agent 的 `model` 传下去，三家写法不同——`dsh` 是会话级
config option、`codex` 是 thread 参数、`opencode` 是 `provider/model` 两级引用
（只给 model id 无法确定 provider，执行器会直接报错而不是猜）。

### 5.5 权限上限

- `effective = min(node.permission, run.permissionCeiling)`；
- 无人值守的 run 必须显式勾选，且 ceiling ≤ `workspace-write`；
- `danger-full-access` 只允许交互式 run，且每个节点首次危险动作仍弹一次确认；
- 权限请求事件带 `dagId / runId / nodeId`，通知中心点进去直接定位节点。

## 6. 插件化装配

### 6.1 原则

Rust 没有稳定 ABI，所以「插件」和现有 `kernel.rs` 一样是**编译进二进制的
模块 + 清单 + 可逆关停**，不是运行时 dlopen。运行时扩展走 MCP（工具）和
JSON 配置（Agent / 计划）。这和第 3 条内核约束一致。

能力用 trait + 注册表表达，新增一种节点执行器 = 实现 trait + 在
`lib.rs` 清单加一行；关掉插件 = 从清单去掉，依赖它的计划在运行前报
「缺少执行器 xxx」，而不是运行到一半崩。

### 6.2 插件（实际装配，`plugins.rs` / `lib.rs`）

| 插件 id | requires | 提供 | 关停时 |
|---|---|---|---|
| `dag.executor.native` | `engine.native` | `NodeRunner` id=`native` | 从注册表撤掉 |
| `dag.executor.codex` | `engine.codex` | `NodeRunner` id=`codex` | 立即打断该引擎上跑着的节点 |
| `dag.executor.dsh` | `engine.dsh` | `NodeRunner` id=`dsh` | 立即打断该引擎上跑着的节点 |
| `dag.executor.opencode` | `engine.opencode` | `NodeRunner` id=`opencode` | 立即打断该引擎上跑着的节点 |

引擎执行器**一个引擎一个插件**，而不是合成一个 `dag.executor.engine`：这样
「编排别用 Codex」只关一个开关，Codex 聊天照常；反过来关掉 `engine.codex`，
执行器随 `requires` 级联关掉，不会留一个指向空引擎的注册项。

调度、并发、重试、闸门、占位符替换都在 `dag::executor` 里，**与执行器无关**：
加一个引擎 = 实现 `NodeRunner`（外部引擎直接用 `dag::engine_runner::EngineRunner`）
+ 在清单里加一行。计划里出现的执行器没装，`dag_run` 会在**开跑前**报错并列出
已装的，而不是静默降级成 native。

装配顺序：各 `engine.*` → `engine.host` → 其余 → 各 `dag.executor.*`。
撤销栈逆序执行，正好保证「执行器先撤、引擎最后停」。

### 6.3 注册表

```rust
pub struct DagState {
    store: DagStore,
    runs: Mutex<HashMap<String, RunHandle>>,
    executors: RwLock<Vec<Arc<dyn NodeRunner>>>,
    workspaces: RwLock<Vec<Arc<dyn WorkspaceProvider>>>,
    planners: RwLock<Vec<Arc<dyn Planner>>>,
}

impl DagState {
    pub fn register_executor(&self, runner: Arc<dyn NodeRunner>);
    pub fn register_workspace(&self, provider: Arc<dyn WorkspaceProvider>);
    pub fn register_planner(&self, planner: Arc<dyn Planner>);
    pub fn release_* // 按 owner，插件关停用
}
```

注册项带 `owner`（插件 id），和 `ToolRegistry` 同一套撤销语义。

## 7. Planner

### 7.1 接口

```rust
pub struct PlanRequest {
    pub goal: String,
    pub project: String,
    pub context: PlanContext,      // README / AGENTS.md / 目录树摘要 / git 分支
    pub agents: Vec<AgentSummary>, // 可选 Agent + 能力
    pub tools: Vec<ToolSummary>,   // 公用 / 每个 Agent 私有
    pub limits: PlanLimits,        // max_nodes / max_parallel / 语言
}

pub trait Planner: Send + Sync {
    fn id(&self) -> &'static str;
    fn plan(&self, req: &PlanRequest, cancel: &Cancel) -> Result<DagPlan, String>;
}
```

### 7.2 LLM 出 JSON 的稳妥流程

1. 提示词里给：目标、项目摘要、Agent 目录、工具目录、**严格 JSON schema**、
   规划规则（3–8 个节点、写者用 worktree、最后要有验证节点、不许环）。
2. 模型返回 JSON → `serde_json` 解析 → `validate()`。
3. 校验失败把**错误原文**喂回去修，最多 2 次；仍失败就降级成单节点计划
   （一个 Agent 直接干），不让用户卡在「规划中」。
4. 草稿保存前不运行；UI 明确显示「计划待确认」。

规划规则（写进 system prompt）：

- 每个节点原子、可独立验证，标题用动词开头；
- 调研节点只读、可并行；写节点必须声明 worktree；
- 下游只依赖它真正需要的上游，不要串成一条链；
- 最后一个节点是验证 / 汇总，依赖所有写节点；
- 不生成「安装依赖」这种环境节点，除非项目摘要显示缺少依赖；
- 节点 task 必须自足：子 Agent 看不到别的节点上下文，只能看到显式注入的产物。

### 7.3 重新规划

节点失败时提供「让 Planner 修这一段」：输入失败报告 + 已完成产物 + 原目标，
要求只替换失败节点的子图，输出补丁式计划（新增 / 删除 / 改依赖），人工确认
后应用到草稿。运行中的 run 不可变，重新规划作用于下一次 run。

### 7.4 预算

- 计划期：`max_nodes`（默认 8）、每个 task ≤ 4k 字符；
- 运行期：run token 软预算 + 单节点轮次 / token 上限（native 执行器复用
  `subagent` 的 `MAX_TURNS` / `TOKEN_BUDGET` 思路）；
- 规划器本身也是一次模型调用，token 计入 run 的「规划」开销。

## 8. 可视化

### 8.1 视图

新增两个一级视图，挂在 `nav.ts` 的 `NAV_ITEMS`（「Agents」排在「工坊」之前、
「编排」排在「赛马」之前），`normalizeNavOrder` 会自动给旧存储补上，不用迁移：

- `agents`（Agent 管理台）：内置 + 自定义 Agent 走同一注册表，见
  `docs/AGENTS.md` §5；
- `dag`（编排）：本文 §8.2–8.5 的画布与节点详情。

三个视图的串联关系（对应 `design/index.html` 的静态设计稿）：

```
需求分析（DAG Planner 弹层）
      │  读 AgentRegistry.list()
      ▼
Agents 管理台 ──挑选/新建 Agent──► 编排画布 ──节点打开──► 归档会话
      ▲                                │
      └── 工坊（插件清单：dag.* 可插拔开关）──┘
```

工坊里的 `dag.*` / `dag.agents.*` 插件开关直接决定 Agents 视图与编排视图的
可用性；关掉 `dag.core` 时两者都返回「DAG 插件未启用」。

```
┌ 编排 ─────────────────────────────────────────────────────────────┐
│ 计划：给设置页加自动更新     [自动排版] [导出] [运行] [停止]  3/7  │
├───────────┬───────────────────────────────────────┬───────────────┤
│ 计划 / 运行│               画布                    │ 节点详情       │
│           │                                       │ 任务 / 工具    │
│ ● 运行中   │   ┌────────┐      ┌────────┐          │ 工作区 / 输出  │
│ ○ 草稿     │   │ research│─────►│frontend│──┐      │ 日志          │
│ ○ 已完成   │   └────────┘      └────────┘  ├─►┌────┴───┐        │
│           │        └──────────►┌────────┐──┘  │ verify │        │
│           │                    │backend │─────►└────────┘        │
└───────────┴───────────────────────────────────────┴───────────────┘
```

- 左栏：计划列表 + 最近运行（状态点 / 时间 / 耗时 / token）。
- 中栏：画布（草稿可拖节点；运行中只读 + 实时状态）。
- 右栏：选中节点的详情；双击节点或点「打开会话」→ 工作台 / 归档定位。

### 8.2 布局算法（`src/dag/layout.ts`，纯函数 + 单测）

不引图库，手写分层布局，约 200 行：

1. **分层**：Kahn 拓扑 + 最长路径定 rank（根 rank=0，节点 rank = max(前驱)+1）。
2. **同层排序**：初始按计划顺序；做 4 轮 barycenter 上下扫描，减少交叉。
3. **坐标**：`x = rank * 260`，`y = index * 120`，整层垂直居中。
4. **边**：从右中点到左中点的三次贝塞尔；同层/回边理论上不存在（校验拦截）。
5. **手动坐标优先**：`layout` 里有坐标就不覆盖；「自动排版」清空后重算。

节点卡尺寸固定 220×96，布局结果确定（同样输入同样输出），便于测试和
视觉回归。

### 8.3 画布交互

- 拖拽平移、滚轮缩放（0.4–1.6）、`适应窗口`、`1:1`；
- 节点点击选中；草稿模式可拖动节点（写回 `layout`）；
- 运行中：状态色 + 呼吸动画（`prefers-reduced-motion` 时静止）；
- 边状态：已满足实线、运行中流动虚线、跳过灰色虚线、失败红色；
- 跟随模式：自动把 running 节点滚进视野（可关）；
- 键盘：方向键切换选中，Enter 打开详情，Esc 取消选择。

### 8.4 节点卡与详情

卡片：状态点 / 标题 / Agent 名 / 执行器徽标 / 工具数 / 重试次数 / 耗时。
详情标签页：

| 标签 | 内容 |
|---|---|
| 任务 | task 原文、解析后的占位符、依赖列表、验收标准 |
| 工具 | 公用 / 私有工具清单、权限档、工作区模式 |
| 输出 | report.md、changed-files、artifact 列表、diff |
| 日志 | 实时事件（复用 `LiveTurn` / 时间线组件）、错误、重试历史 |

### 8.5 运行态入口

**已实现**（`DagPane` 的运行条）：

- header 右上「▶ 运行 / 停止」。跑不了时按钮是灰的，**悬停即说明原因**
  （未保存 / 有问题 / 图里没节点 / 正在跑）——用户不用猜为什么点不动；
- 运行条：状态点 + 进度条 + `已落定/总数` + token 计数 + 计时 + 节点 chips。
  chip 点一下就选中该节点并切到「运行」标签看产出；「收起」只收起这条，
  运行本身不受影响；
- 人工闸门：卡住的节点在运行条里弹一条横幅（放行 / 拒绝），拒绝会连带把
  下游标跳过——不是报错，是"没跑"；
- 画布节点：运行态走描边（不铺底色，节点里已经有 Agent 头像与引擎徽标），
  右上角一个状态记号，跑起来后第二行显示**最后一步工具调用**；
- 产出：节点报告写的是 markdown。右栏「产出」默认按渲染稿看（标题 / 列表 /
  表格 / 代码块），标题行右边可切「原文」逐字对照，也可一键复制原文；
  「查看全文」的制品弹层同一套（`kind = text` 才有预览，json / 二进制照旧原文）。
  默认渲染而不是默认原文：报告是给人读的，要贴到别处时再切原文；
- 运行结束 / 失败 / 待审批 → 现有通知中心 + 托盘计数（后置）；
- 节点会话出现在归档：`SessionMeta` 加 `dag: { planId, runId, nodeId, project }`，
  归档按 `project`（而不是 worktree 路径）归组，显示「DAG」角标，可按计划筛选 /
  隐藏节点会话。否则每个 worktree 都会被当成一个新项目，归档会被切碎。


## 9. IPC 与事件

### 9.1 命令（`ipc.rs` 薄壳，逻辑在 `dag` 模块）

已实现（与 `src-tauri/src/ipc.rs` 一一对应）：

```text
agents_list / agent_save / agent_delete      Agent 注册表
dag_list                                     计划摘要（最近改过的在前）
dag_get(planId)                              计划详情
dag_new(project)                             空白草稿（带新 id）
dag_save(plan)                               保存（校验是独立命令，见下）
dag_delete(planId)
dag_validate(plan)                           前端编辑后实时校验
dag_plan(goal, project, maxNodes)            Planner 生成草稿（不落盘）
dag_run(planId)                              校验 + 起运行，立刻返回首份快照
dag_run_cancel(runId)                        停止（节点在下次调用前收手）
dag_run_approve(runId, nodeId, allowed)      人工闸门的答复
dag_run_latest(planId)                       最近一次运行（重启后从磁盘捡）
dag_run_get(runId)                           按 id 取（刷新用）
dag_runners()                                已装的节点执行器 id（Agent 编辑器标红用）
plugins_list / plugin_set_enabled            内核装配清单与开关（下次启动生效）
```

两条设计约束：

- **「存不下」与「跑不了」分开报**：`dag_save` 只管磁盘，`dag_validate` 只管图。
  混成一条会让用户不知道该改哪里。
- **运行不在命令里等**：`dag_run` 返回的那一刻图刚开跑，进度全靠事件推。
  一张图可能要跑几十分钟，让 invoke 挂着既超时也没意义。

### 9.2 事件

单通道 `dag.run`，载荷是**整份** `DagRun` 快照（不是增量）：

```json
{
  "id": "run-dag-20261022-120400001-1760000000000-001",
  "planId": "dag-20261022-120400001",
  "status": "running",
  "startedAt": 1760000000000,
  "finishedAt": null,
  "message": null,
  "nodes": [
    { "id": "research", "status": "succeeded", "attempt": 1, "startedAt": 1760000000000,
      "finishedAt": 1760000004200, "output": "…", "error": null,
      "tokensIn": 1800, "tokensOut": 420, "steps": ["read_file src/lib.rs"] }
  ]
}
```

为什么推全量而不是增量：节点只有十几个，一份快照几十 KB 上限，换来的是前端
不需要维护「补丁合并」这套逻辑——少一类状态不同步的 bug。

**运行中的过程另走一条增量通道** `dag.node.live`（`NodeLive`）：

```json
{ "runId": "…", "nodeId": "research", "delta": "先看头部结构。",
  "step": "read_file a.bin", "tokensIn": 12000, "tokensOut": 320,
  "turn": 3, "maxTurns": 12 }
```

- 正文按 120ms 一档合并后发（每个 delta 都发会把 IPC 打满）；遇到新的一步工具
  调用时立刻把攒着的正文一起带出去，保证顺序不乱；
- `step` / `tokens` 一有变化就发，运行条据此显示「现在谁在干什么」、跑了几分
  钟、以及「静默 N 秒」的卡住提示；
- `turn` / `maxTurns` 由执行器在**每一轮模型调用开始时**报（`on_turn`）。这一条
  是给「模型在思考、一两分钟不吐字也不调工具」那段空白用的：正文不动时，
  至少「第 3/12 轮」在往前走，用户能分清「在慢慢跑」和「真挂了」；
- 前端按 `nodeId` 叠在快照之上（`DagPane` 的 `liveByNode`）。节点一落定，快照里
  就有完整正文与步骤，增量随之丢弃——两份数据不会打架。

`NodeRun` 也存 `turns` / `maxTurns`，刷新或重启后不会退回「第 0 轮」。

**兜底是轮询**：运行期间前端每 2s 拉一次 `dag_run_latest`。事件通道要是因为版本
不匹配或 IPC 卡顿断了，进度最多慢 2 秒，不会整场空白——因为 `push_text` /
`push_step` / `push_usage` / `push_turn` 都同时写进了内存里的快照。

没有这条通道时，节点跑两分钟面板就是两分钟空白，看起来和卡死没区别。

快照还有一条兜底：磁盘上的记录如果写着 `running`，但内存里已经没有句柄
（应用重启过），读取时会被改写成「中断」并把在跑/等确认的节点标失败。否则
这张计划会永远显示「正在跑」，重跑按钮也被「这张计划正在跑」挡住。

前端侧（`src/dagRun.ts`）只做纯函数换算：进度、耗时、闸门列表、按钮禁用原因，
都带单测，组件里不写这些判断。

## 10. 存储

```
~/.prism/dags/
  agents.json                       Agent profiles
  plans/<planId>.json               计划（含 layout）
  plans/<planId>.history/           可选：编辑历史
  runs/<runId>/
    run.json                        快照 + 节点状态 + 汇总
    events.jsonl                    追加写，排障 / 回放
    nodes/<nodeId>/
      attempt-1/report.md
      attempt-1/changed-files.json
      attempt-1/artifact/…
    ws/<nodeId>/                    worktree 工作区
    scratch/<nodeId>/
```

- 原子写（tmp + rename），和 `annotations` / `native::store` 一致；
- `run.json` 每次状态迁移落盘，崩溃最多丢最后一个事件；
- 清理：默认保留最近 20 个 run；worktree 只在用户点「清理」时删，且先确认
  分支已提交；不自动删用户分支；
- 数据主权：只写 `~/.prism/`，不写引擎目录；worktree 是 git 的合法工作树，
  提交也在分支上，不动用户当前分支。

## 11. 实施顺序

按「先能跑、再好看、最后聪明」拆，每一步都可独立验收：

### 落地状态（2026-10-22）

| 步 | 内容 | 状态 |
|---|---|---|
| D1 | 内核（model / store / validate）+ 画布 + 布局 + 运行态 | ✅ 已完成 |
| D2 | Agent 注册表 + `dag.executor.native`（真跑节点，不是 stub） | ✅ 已完成 |
| D3 | `dag_plan`（LLM 拆图）+ 规划弹窗 | ✅ 已完成 |
| D5 | 失败策略 / 重试 / 闸门 / 取消 / 运行条 / 运行记录落盘 | ✅ 已完成 |
| D4 | 多引擎执行器（dsh / codex / opencode 各一个 `NodeRunner`） | ✅ 已完成 |
| D5 | worktree 产物 diff、审批卡通知跳转、模板市场 | ⬜ 未做 |

D4 的落地形状：`dag/engine_runner.rs` 里的 `EngineRunner` 是**一个**通用执行器，
按持有的 `Arc<dyn Engine>` 分别注册成 `codex` / `dsh` / `opencode`。它只做
「开会话 → 发提示词 → 从事件流认领这一轮的文本 → 等终态」，四件事全在
`Engine` trait 上，所以再加引擎不用动这一层。

文本从事件流取，规则两条：`session.text.delta` 累加、`session.text.ended`
用整段覆盖；一轮里可能有多条助手消息（工具调用前后各一段），交付物取**最后
一段非空文本**——那是这一轮的结论，前面的是过程。

外部引擎不回报 token 用量（各家协议没有统一字段），节点卡上的用量显示 `—`，
而不是编一个数。测试覆盖：增量累加、多消息取末段、`ended` 覆盖、空白段跳过、
工具步骤摘要、prompt 拼装。

### D1 内核 + 画布（约 2 天）

- `dag` 模块：model / store / validate（环、未知依赖、重复 id、占位符）；
- Scheduler + 一个 `stub` 执行器（测试专用，固定延时 + 报告）；
- IPC：`dag_list/get/create/run/run_get/cancel`；
- `DagPane` 只读画布 + 布局算法 + 节点状态色；
- dev-preview 假后端补命令，加 `?view=dag&dag=run` 视觉状态；
- 验收：手写 JSON 计划能跑通 4 节点图，刷新后画布状态正确。

### D2 原生 Agent + 工作区（约 3 天）

- Agent profile 读写 + 内置 presets；
- `dag.executor.native`：节点 = native 会话，收集最终报告；
- 工具 audience / policy 扩展 + 执行兜底；
- `dag.workspace.worktree`：分支、commit、diff 产物；
- 节点详情 + 打开会话 + 归档角标；
- 验收：两个写节点并行在各自 worktree 改文件，互不污染，产物可 diff。

### D3 Planner（约 2 天）

- `dag.planner.llm`：JSON schema + 校验修复 + 降级；
- 规划弹窗（目标 / 项目 / 约束 / 模型）→ 草稿 review → 编辑 → 运行；
- 计划校验实时提示；模板（功能开发 / 调研 / 重构 / 修 bug）；
- 验收：一句目标生成 3–8 节点计划，人工改一个 Agent 后能跑通。

### D4 多引擎执行器（已完成）

- `dag/engine_runner.rs`：`EngineRunner` 复用 `Engine` trait（subscribe →
  create_session → send_prompt_with_model → interrupt）；
- 三个插件 `dag.executor.{codex,dsh,opencode}`，各自 `requires` 对应引擎；
- 能力矩阵见 §5.4（外部引擎不接管工具策略，权限档不下传）；
- 混合计划：调研用 dsh，实现用 native，验证用 codex —— 每个节点按 Agent 的
  `executor` 逐节点解析，不是"整张图共用一个"；
- 验收：一张图里至少两种引擎，节点步骤里带会话 id，可在归档里打开那一轮。

### D5 治理与打磨（约 2–3 天）

- 失败策略 / 重试 / 预算 / 恢复 / 取消；
- 审批卡接入（run / node 上下文、通知跳转）；
- 重新规划、导出 SVG、模板市场（后置）；
- 文档 + 截图。

## 12. 测试与验收

**Rust 单测（不碰网络）**

- 校验：环、自依赖、未知依赖、重复 id、占位符引用非祖先；
- 调度：ready 集合、并发上限、失败传播、`Continue` 分支独立、重试次数、
  超时、取消、预算到顶；
- 存储：原子写、schema 迁移、崩溃后 `interrupted` 标记；
- 工具：audience 解析、allow/deny、权限档、私有覆盖 shared、内置不可覆盖；
- 恢复：已 succeeded 的节点不重跑。

**前端单测**

- `layout.ts`：分层正确、同层顺序确定、空图 / 单节点 / 菱形图；
- 事件 reducer：乱序事件、重复事件、快照 + 增量合并幂等；
- 模板渲染：占位符、转义、超长截断。

**集成**

- `stub` 执行器跑完整 DAG（失败 / 重试 / 取消）；
- 真机验收：在 Prism 仓库的 worktree 上跑「调研 → 前端 + 后端 → 验证」，
  检查分支、diff、报告、画布状态、归档角标；
- 视觉：`dev-preview.html?view=dag&dag=empty|draft|run|failed`，无头 Chrome
  断言关键节点与状态色。

**性能**

- 50 节点图的布局 < 10ms；1000 条事件合并 < 16ms；
- worktree 创建在 Prism 仓库上 < 1s（不含依赖安装）。

## 13. 风险与边界

| 风险 | 处理 |
|---|---|
| worktree 缺依赖（node_modules / target） | profile 配 `linkDirs`；或计划里加 setup 节点；Rust 不共享 target |
| 并行写冲突 | 默认 worktree；shared 模式加写锁串行；不自动 merge |
| 外部引擎不遵守工具策略 | 能力矩阵明示；Planner 不给它派私有工具；必要时只用 native |
| 成本失控 | maxNodes / maxParallel / token 预算 / 单节点轮次上限 |
| 应用退出时节点在跑 | 落盘 interrupted；重启不自动跑，手动恢复 |
| worktree 不是安全沙箱 | 文档写明；不可信任务用 dsh 的 sandbox 执行器 |
| 计划质量差 | 必须人工确认；校验修复；模板；重新规划 |
| 归档被 DAG 会话刷屏 | 会话带 dag 元数据 + 归档筛选「隐藏 DAG 节点」 |
| 前端图库依赖 | 手写分层布局 + SVG，不引 Vue Flow，保持包体和视觉一致 |

## 14. 待定问题

1. **第一版执行器范围**：只做 native（工具/工作区完全可控），还是同时接
   engine？建议 D1–D3 只做 native，D4 再接引擎。
2. **写隔离默认值**：worktree 是默认，还是「单写者 shared + 锁、多写者
   worktree」由 Planner 判断？建议后者，减少不必要的 worktree 开销。
3. **自动运行**：计划生成后是否允许一键直接跑？建议默认必须确认；可给
   「信任的模板」加自动运行开关。
4. **DAG 会话是否进归档**：建议进（带角标 + 可筛选），这样历史、搜索、用量
   都能自然覆盖；代价是归档会变长。
5. **是否需要「人在环」节点**：`gate.before/after` 先做审批卡复用，还是
   单独的人工输入节点？建议先复用审批卡，人工输入节点后置。

## 15. 与 Agent 面板的衔接

本文讲「怎么编排」，`docs/AGENTS.md` 讲「编排的是谁」。两篇共享同一套概念，
避免落地时漂移：

| 概念 | 本文 | AGENTS.md |
|---|---|---|
| Agent 数据 | `DagNode.agent` 引用 profile id | `AgentProfile` 定义 + `agents.json` 存储 |
| 工具作用域 | `ToolAudience` / `ToolPolicy`（§5.2） | `ToolSelection`（§2.3），同一份解析器 |
| 内置 Agent | `dag.presets` 插件提供 | §3 的 presets 表 |
| 私有 MCP | `dag.agents.mcp`（§6.2） | `Private(agent)` + 撤销语义（§4） |
| 视图 | `AppView = 'dag'`（§8.1） | `AppView = 'agents'`（§5） |
| Planner 候选人 | `PlanRequest.agents`（§7.1） | `AgentRegistry::list()` 的投影（§6） |

落地顺序建议：**先 A1/A2（Agent 数据 + 面板），再做 D1–D4（编排）**。因为
Planner 的 `PlanRequest.agents` 依赖注册表，先有 Agent 实体，Planner 生成的
DAG 才能引用真实的 `agent` id，而不是临时编名字。

静态设计稿 `design/index.html` 把「需求分析 → Agents → 编排 → 工坊」串成一个
可切换的单页，`design/dag-planner.html` 是编排画布的聚焦稿。两者共用同一套
设计令牌，落 UI 时以 `src/styles/tokens.css` 为准。
