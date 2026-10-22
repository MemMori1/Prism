# Agent 注册表（设计稿）

> 把 Agent 从「藏在 `task` 工具里的两个枚举」提升为**可插拔的一等公民**：
> 内置一批、用户自定义一批，全部走同一个注册表；每个 Agent 声明自己的
> 执行器、系统提示、工具集（公用 / 私有）与权限档。DAG Planner 从这些
> Agent 里挑人、按依赖编排成图。
>
> 本文是设计稿，不包含实现。与 `docs/DAG_PLANNER.md` 互补：DAG 讲「怎么
> 编排」，本文讲「编排的是谁」。

## 0. 一句话

**Agent = 配置 + 可插拔能力**。内置 Agent 与用户自定义 Agent 在同一个
注册表里，区别只是 `origin` 字段（`builtin` / `user`）；插件化的落点是
「注册表 + 撤销语义」，而不是「能删掉内置 Agent」。

## 1. 为什么需要它

现状（`engines/native/subagent.rs`）：

- Agent 只有两个：`explore`（探索，只读）、`general`（通用，可写）；
- 它们不是「实体」，是 `AgentType` 枚举的两个分支，能力写死在
  `specs_for_with(enabled, agent)` 里；
- 想加一个「前端工程师」只能改枚举、改 `system_prompt` 分支、改工具表，
  是编译期行为，用户改不了。

目标：

1. Agent 是**数据**（`agents.json`），可以读、写、复制、删除、导出；
2. 内置一批开箱即用的 profile，用户可复制改写，或从零新建；
3. 每个 Agent 自带一套**工具策略**（公用 + 私有 + 拒绝 + 权限档），
   可插拔；
4. DAG Planner 拿到的「候选人目录」就是这份注册表的投影；
5. 外部引擎（dsh / codex / opencode）也能当 Agent 的**执行器**。

## 2. 数据模型

### 2.1 存储（`~/.prism/agents/`）

```
~/.prism/agents/
  builtin/<id>.json      只读 presets，应用升级时覆盖
  user/<id>.json         用户自定义（可覆盖 builtin 同名 id）
```

解析顺序：`user/<id>.json` 优先，缺省回退 `builtin/<id>.json`。这样用户能
「覆盖」内置 Agent 而不破坏它——关掉覆盖即还原。

### 2.2 形状

```rust
pub struct AgentProfile {
    pub id: String,                 // 短、稳定：explore / frontend / verifier
    pub name: String,               // 中文名：前端工程师
    pub origin: AgentOrigin,        // Builtin | User
    pub description: String,        // 卡片一句简介

    pub executor: ExecutorKind,     // Native | Engine(String) | Shell
    pub model: Option<String>,      // None = 跟随全局默认
    pub system_prompt: String,

    pub permission: PermissionMode, // read-only / workspace-write / danger-full-access
    pub workspace: WorkspaceMode,   // 默认工作区（DAG 节点可覆盖）
    pub tools: ToolSelection,       // 见下
    pub mcp: Vec<String>,           // 引用 ~/.prism/mcp.json 的 server id

    pub meta: AgentMeta,
}

pub struct AgentMeta {
    pub tags: Vec<String>,          // 调研 / 前端 / 后端 / 文档 / 验证…
    pub icon: String,               // 头像底色 + 首字（前端渲染用）
    pub link_dirs: Vec<String>,     // worktree 缺依赖时 symlink（node_modules）
    pub gate_default: bool,         // 该 Agent 的节点默认要人工确认？
}
```

### 2.3 工具策略（公用 / 私有）

```rust
pub struct ToolSelection {
    /// None = 该 Agent 默认可见的全部公用工具。
    pub shared: Option<Vec<String>>,
    /// Agent 私有工具 id（含 MCP 工具，命名 srv__tool）。
    pub private: Vec<String>,
    /// 显式禁用（优先于 allow）。
    pub deny: Vec<String>,
}
```

解析顺序（同一个 Agent 看到的最终清单）：

1. 内置工具 + `audience=Shared` 的外部工具；
2. 加上 `audience=Private(agent_id)` 的外部工具；
3. 去掉 `deny`；有 `shared` 白名单时只留白名单；
4. 删掉权限档不够的（`Permission::within`）；
5. 同名私有覆盖公用；内置名永远不可覆盖。

> 这与 `docs/DAG_PLANNER.md` §5 的 `ToolAudience` / `ToolPolicy` 是同一套
> 语义——Agent 面板和 DAG 节点共用一份解析器，避免两套规则漂移。

## 3. 内置 presets

内置由 `dag.presets` 插件提供（见 DAG_PLANNER.md §6.2），第一版给这些：

| id | name | executor | 权限 | 工作区 | 工具（私有） | 标签 |
|---|---|---|---|---|---|---|
| `explore` | 探索者 | native | 只读 | shared | — | 调研 |
| `general` | 通用助手 | native | 工作区可写 | shared | — | 通用 |
| `frontend` | 前端工程师 | native | 工作区可写 | worktree | `figma__export` | 前端 |
| `backend` | 后端工程师 | native | 工作区可写 | worktree | — | 后端 |
| `architect` | 架构师 | native | 只读 | shared | — | 设计 |
| `docs` | 文档工程师 | native | 工作区可写 | worktree | — | 文档 |
| `verifier` | 验证者 | native | 工作区可写 | worktree | — | 验证 |
| `dsh-research` | DeepSeek 调研 | engine(dsh) | 只读 | shared | — | 调研 |

内置只给「最小可用」，用户照猫画虎。`explore` / `general` 直接映射现有
`AgentType`，保证向后兼容：老会话里的 `task` 工具调用解析结果不变。

## 4. 注册表与插件化

### 4.1 原则

和 `kernel.rs` 一致：Rust 没有稳定 ABI，「插件」是**编译进二进制的模块 +
清单 + 可逆关停**，不是运行时 dlopen。Agent 的能力来源有三类，都是「注册
表 + 撤销」：

| 能力 | 来源 | 撤销 |
|---|---|---|
| 内置工具 | `engines/native/tools.rs` 静态表 | 关停对应 plugin |
| 外部工具 | `ToolRegistry`，带 `owner` | `release(owner)` |
| Agent profile | `agents.json` | 删除 / 覆盖还原 |
| 私有 MCP 工具 | `dag.agents.mcp` 按 profile 懒启动 | `release("agent:<id>")` |

### 4.2 注册表形状

```rust
pub struct AgentRegistry {
    profiles: RwLock<HashMap<String, AgentProfile>>,
    // 私有工具以 Private(agent_id) 登记，撤销时按 id 反查。
    tool_owners: Mutex<HashMap<String, Vec<String>>>,
}

impl AgentRegistry {
    pub fn upsert(&self, profile: AgentProfile) -> Result<(), String>;
    pub fn remove(&self, id: &str);          // 同时 release 它的私有工具
    pub fn get(&self, id: &str) -> Option<AgentProfile>;
    pub fn list(&self) -> Vec<AgentSummary>; // 供 Planner 与 UI
}
```

### 4.3 关停 / 删除的降级语义

- 删掉一个 Agent：它登记的私有工具一并 `release`；引用它的**草稿计划**在
  下次校验时报「缺少 Agent xxx」，运行前拦截，不崩；
- 覆盖内置 Agent 后还原：删 `user/<id>.json` 即回到 builtin；
- 引擎执行器不存在（如删了 dsh）：节点校验报「缺少执行器」，见
  DAG_PLANNER.md §6.1。

## 5. 面板（一级视图 `agents`）

新增 `AppView = 'agents'`，挂在 `nav.ts` 的 `NAV_ITEMS`（在「工坊」之前）。
`normalizeNavOrder` 会自动给旧存储补上，不用迁移。

布局（三栏）：

```
┌ Agents ───────────────────────────────────────────────────────┐
│ 筛选：全部 / 内置 / 自定义 · 按标签    [＋ 新建 Agent]          │
├────────────┬───────────────────────────────────┬──────────────┤
│ 分类列表    │   Agent 卡片网格                   │  详情 / 编辑  │
│ · 内置 7    │  ┌─────────┐ ┌─────────┐          │  身份 / 执行器│
│ · 自定义 3  │  │ 前端工程师│ │ 后端工程师│ …        │  系统提示     │
│ · 标签分组  │  └─────────┘ └─────────┘          │  工具策略     │
│            │  每个卡片：头像/名/简介/标签/         │  权限 / 工作区│
│            │  执行器徽标/工具数/权限档/来源角标     │  MCP / 预览  │
└────────────┴───────────────────────────────────┴──────────────┘
```

卡片要点（与工坊「子代理」资产对齐，但 Agent 面板是**编辑入口**，工坊是
**只读浏览**）：

- 头像（首字 + 底色）+ 名称 + 一句简介；
- 标签（调研 / 前端 / …）；
- 执行器徽标：`原生` / `DSH` / `OC` / `CDX`（复用 `ENGINE_BADGES`）；
- 工具数：`公用 N · 私有 M`；
- 权限档：只读 / 工作区可写 / 全盘可写（最后一档标「谨慎」）；
- 来源角标：`内置`（灰）/ `自定义`（accent）。

详情 / 编辑右侧栏标签页：

| 标签 | 内容 |
|---|---|
| 身份 | id / 名称 / 简介 / 标签 / 头像 |
| 执行器 | executor 选择、model、系统提示 |
| 工具 | 公用白名单 / 私有列表 / deny / 权限档 |
| 工作区 | 默认 WorkspaceMode、linkDirs、gate 默认值 |
| 预览 | 只读渲染「这个 Agent 会给模型看到的工具清单」 |

编辑保存走 `dag_agent_save`，删除走 `dag_agent_delete`（见 DAG_PLANNER.md
§9.1 的 IPC 命令——Agent 的读写复用 DAG 的命令集，不另起炉灶）。

## 6. 与 DAG Planner 的关系

```
需求分析（DAG Planner）
      │  读 AgentRegistry.list()
      ▼
生成 DAG 草案 ──► 每个节点.agent 指向注册表里的 id
      │
      ▼
节点运行 ──► 按 agent 的 executor / tools / permission 启动会话
```

- **Planner 的候选人目录 = Agent 注册表投影**：`PlanRequest.agents` 就是
  `AgentRegistry::list()` 的 `AgentSummary`（id / name / tags / executor /
  permission / 工具摘要）。
- **节点复用 Agent**：`DagNode.agent` 引用 profile id；不填走
  `DagDefaults.agent`。
- **私有工具跟着 Agent 走**：前端工程师节点的 `figma__export` 来自它的
  profile，不是 Planner 现场发明。
- **外部引擎执行器边界**：`engine(dsh)` 这类 Agent 不支持私有工具 / 系统
  提示强约束（见 DAG_PLANNER.md §5.4 能力矩阵），Agent 面板要标出。

一句话：**DAG Planner 负责「谁先谁后」，Agent 注册表负责「每个是谁」**。

## 7. 插件化装配（新增）

沿用 DAG_PLANNER.md §6 的插件清单，Agent 面板额外依赖：

| 插件 id | requires | 提供 | 关停时 |
|---|---|---|---|
| `dag.presets` | `dag.core` | 内置 Agent / 模板 | — |
| `dag.agents.mcp` | `dag.core`, `tools.mcp` | 私有 MCP 工具 | 撤销 `agent:*` 工具 |
| `dag.agents.ui` | `dag.core` | `agents` 视图 / IPC 薄壳 | 视图隐藏 |

装配顺序不变：引擎 → `engine.host` → `tools.mcp` → `dag.core` → 各 executor
/ workspace / planner → `dag.agents.ui`。

## 8. 实施顺序

### A1 Agent 数据与注册表（约 1.5 天）

- `AgentProfile` / `AgentRegistry` / `~/.prism/agents/` 读写；
- 内置 presets（`explore` / `general` 映射现有 `AgentType`）；
- 工具 audience 扩展（`Shared` / `Private(agent)`，复用 DAG_PLANNER.md §5）；
- IPC：`dag_agents` / `dag_agent_save` / `dag_agent_delete`；
- 验收：手写 JSON 加一个 Agent，能在 `dag_agents` 里读到，删掉即消失。

### A2 面板 UI（约 1.5 天）

- `agents` 视图：三栏布局 + 卡片网格 + 详情/编辑；
- 工具策略编辑器（公用白名单 / 私有 / deny / 权限档）；
- 预览（「模型会看到的工具清单」只读）；
- dev-preview 假后端补 `dag_agents` 等命令；
- 验收：卡片网格正确渲染内置 + 自定义，编辑保存后刷新仍在。

### A3 Planner 接入（与 DAG_PLANNER D3 合并）

- `PlanRequest.agents` 用注册表投影填充；
- 规划弹窗里的「Agent 目录」来自注册表；
- 验收：一句目标生成的 DAG，每个节点.agent 都是注册表里的 id。

### A4 多引擎执行器（与 DAG_PLANNER D4 合并）

- `engine(dsh)` 等 Agent 作为节点执行器；
- 能力矩阵明示（系统提示 / 私有工具 / MCP 支持）；
- 验收：一张图里 native + dsh 两种 Agent 并行。

## 9. 测试与验收

**Rust 单测（不碰网络）**

- 注册表：upsert / remove / 覆盖还原 / origin 区分；
- 工具解析：allow / deny / 私有覆盖公用 / 内置不可覆盖 / 权限档过滤；
- 删除 Agent 后：私有工具释放、草稿计划校验报「缺少 Agent」；
- presets：`explore` / `general` 与现有 `AgentType` 解析一致（回归）。

**前端单测**

- 卡片渲染：内置 / 自定义角标、权限档、工具数；
- 工具策略编辑器：allow / deny 的增删与预览联动；
- 视图切换：`agents` 在 `nav.ts` 注册后旧存储自动补位。

**集成**

- 新建 Agent → 在 DAG Planner 里出现 → 生成节点引用它 → 运行成功；
- 删除 Agent → 草稿计划校验报错 → 手动改回后恢复；
- 视觉：`dev-preview.html?view=agents` 断言卡片网格与角标。

## 10. 风险与边界

| 风险 | 处理 |
|---|---|
| 用户删了被计划引用的 Agent | 草稿校验报「缺少 Agent」，运行前拦截 |
| 私有 MCP 工具泄漏 | `Private(agent)` 只对该 Agent 可见，撤销随 owner |
| 内置 Agent 被改坏 | builtin 只读，用户覆盖走 `user/`，可还原 |
| 外部引擎不支持私有工具 | 能力矩阵明示，UI 标「此执行器不支持私有工具」 |
| Agent 数量膨胀 | 标签分组 + 筛选 + 面板内搜索 |
