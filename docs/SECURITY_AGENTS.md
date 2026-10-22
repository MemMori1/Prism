# 安全研究 Agent 与工具矩阵 · 需求分析

> 状态：**P0 / P0.5 / P1 / P1.5 已实现**（制品仓库、节点数据流、工具标签、
> 7 个安全研究工种 + 编排规划、8 个 `bin.*` 分析工具、Planner 七工具）。
> 未实现：P2（Ghidra 反编译）、P3（真实沙箱）、渗透线 `recon.*` / `exp.*`。
> 详见 §0.1 与 `docs/ARTIFACTS.md`。
> 关联 `docs/AGENTS.md`、`docs/DAG_PLANNER.md`、`docs/ARTIFACTS.md`。
> 输入：逆向线 / 渗透线的 Agent 与工具清单（用户整理）。本文只做一件事——
> 把那份清单和**现在代码里真实存在的东西**对齐，说清差在哪、先补什么、怎么验收。

## 0. 一句话

Prism 现在是一套能跑起来的**软工编排底座**（Planner 拆图 → 多 Agent 并行 → 节点执行），
但它的 Agent 工种、工具集、数据流都还是"改代码"那一套。要变成安全研究线，
缺的不是再登记 60 个工具名，而是四层地基：**制品、节点数据流、工具标签、Planner 工具面**。

## 0.1 落地回填（P0 / P0.5 / P1 / P1.5 已完成）

需求分析里判"0%"的四层地基，四层都已经补完：

| 项 | 落点 | 状态 |
|---|---|---|
| 制品仓库 | `src-tauri/src/dag/artifact.rs`：`~/.prism/artifacts/<dag_id>/`，索引 + 内容分离，单件 256 MiB / 单 DAG 2 GiB 配额，跨 DAG 读取拒绝 | ✅ |
| 节点数据流 | `dag/model.rs` 的 `input_artifacts` / `output_artifact_key`；`dag/runner.rs::resolve_task` 支持 `{{nodes.x.output}}`（超 8 KB 截断并附 `art-` 引用）与 `{{artifacts.<别名>.id|summary|meta}}` | ✅ |
| 隐式别名 | 未声明 `input_artifacts` 时，上游主产出自动以上游节点 id 为别名，老计划零改动 | ✅ |
| 工具标签 | `engines/native/tools.rs` 的 `ToolMeta`（`tags` / `risk` / `network` / `sandbox` / `scope`）；`scope=dag` 的工具在聊天里不进 schema | ✅ |
| 制品工具 | `engines/native/artifact_tools.rs`：`prism.artifact.put` / `prism.artifact.get`，`dag_id` 由执行器注入 | ✅ |
| 静态样本工具 | `engines/native/forensics.rs`：`bin.meta.identify` / `bin.hash.compute` / `bin.strings.extract`，纯 Rust 解析 PE/ELF/Mach-O/DEX/APK | ✅ |
| P1 分析工具 | `engines/native/analysis.rs`：`bin.json.jq` / `bin.xref.search` / `bin.importexport.list` / `bin.crypto.scan` / `bin.diff`，全部纯 Rust、只读、带上限 | ✅ |
| **安全研究工种** | `src-tauri/src/agents.rs::builtin()` 新增 7 个 preset：`sample-prep` / `unpacker` / `static-analysis` / `crypto-analysis` / `dynamic-sandbox` / `control-flow` / `sec-assist`；各自绑定方法论提示、工具白名单、权限档、工作区 | ✅ |
| Planner 工具面 | `engines/native/dag_tools.rs`：`prism.dag.plan.read` / `node.add` / `node.update` / `node.remove` / `edge.add` / `edge.remove` / `validate`；`scope=dag`，校验不过整体回滚 | ✅ |
| 编排规划工种 | `dag-planner` preset：在 DAG 节点里调上面七个工具改图 | ✅ |
| 前端 | `DagPane.vue` 节点卡片展示制品（key / kind / 大小 / sha256 / 引用 id / 查看全文 / 派生血缘）；设置页工具清单带标签与风险色 | ✅ |

验收：`cargo test --lib` 401 passed（含"200 KB 节点输出 → 下游 prompt < 3 KB 且含 `art-`、
取回长度一致"的端到端用例、PE 导入表遍历、AES 常量命中、单字节 XOR 破译、改图回滚、
UPX 节名定案与 `.adata` 弱特征不定案）；
`vue-tsc --noEmit` 0 错误；`vitest` 376 passed。

**仍未做**：P2 Ghidra（反编译、函数边界、xref 到函数级）、P3 真实沙箱、
渗透线 `recon.*` / `exp.*`、`bin.crypto.xor_brute` 的多字节版本。


## 1. 结论先行：差在哪

| 维度 | 分析时的代码事实 | 目标（清单要求） | 差距（回填后） |
|---|---|---|---|
| Agent 工种 | 8 个软工 preset：探索 / 通用 / 前端 / 后端 / 架构 / 文档 / 验证 / DeepSeek 调研 | 样本预处理 / 脱壳 / 静态分析 / 密码 / 动态沙箱 / 控制流 / 辅助 | **已补**：7 个工种全部落地，各自绑定方法论、工具白名单与风险档 |
| 工具集 | 7 个裸名工具：`read_file` `list_files` `grep` `write_file` `edit_file` `run_command` `task_status` | 60+ 个 `bin.*` / `recon.*` / `exp.*`，带权限 tag | **≈ 28%**：命名空间与 tag 已建，落地 9 个 `bin.*`（4 个样本 + 5 个分析）；渗透线 `recon.*` / `exp.*` 未动 |
| 产物传递 | 节点输出 `NodeOutput.text`，下游用 `{{nodes.x.output}}` **整段内联** | artifact 引用 + `input_artifacts` / `output_artifact_key` + 血缘 | **已补**：8 KB 截断 + `{{artifacts.<别名>.*}}` + 显式/隐式别名 |
| 制品仓库 | `artifact.rs` 只是**交付物预览**（读本地 html/svg/图片给前端看） | `artifact.put/get`，同 DAG 内可读、跨 DAG 隔离 | **已补**：`dag/artifact.rs` + `prism.artifact.*`，配额与隔离齐 |
| Planner 形态 | UI 弹窗 + `dag_plan` IPC，调一次模型出一张图 | Planner 是可调 `prism.dag.*` 的 Agent，能动态改图 | **已补**：七个 `prism.dag.*` + `dag-planner` 工种；弹窗那条路保留不变 |
| 权限模型 | 三档（read-only / workspace-write / full-access）+ shared / private / deny 名单 | 风险标签（`dag:planner` `sec:exp-verify` `bin:dyn-sandbox`）+ 高危显式开关 + 网络策略 | **约 40%**：`risk` / `network` / `sandbox` / `scope` 四维已入工具表；高危闸门与网络策略未接 |

**底座不是零**：DAG 模型 / 校验 / 调度 / 并发 / 重试 / 闸门 / 运行记录 / 执行器插件全都在，
新工种只是往这套底座上挂数据。这一点决定了工作量是"补领域层"，不是"重写编排"。

## 2. 现状盘点（都是可核对的事实）

### 2.1 Agent 注册表

`src-tauri/src/agents.rs::builtin()` 八个 preset，全部是软工角色；
`~/.prism/agents/user/<id>.json` 可覆盖同名内置，删文件即还原。
数据结构已经有 `executor` / `permission` / `workspace` / `tools{shared,private,deny}` / `mcp` / `tags`，
**扩工种不需要改结构**，只是往 `builtin()` 里加数据。

### 2.2 工具注册表

`src-tauri/src/engines/native/tools.rs::builtin_tools()` 七个工具，`ToolDef` 字段是：

```rust
name / schema / origin / permission / action / resource_key / summary / blurb
/ delegable / enabled_by_default / run
```

有权限档与审批动作，**但没有**：命名空间、风险等级、网络策略、沙箱归属。
`specs_for(enabled, max_permission)` 已经能按权限档裁工具，扩展标签是在这条路上加过滤。

### 2.3 `artifact.rs` 的误会

它做的是"时间线里点了本地 HTML / SVG / 图片 → 就地预览"，`read_artifact(path)` 只读**用户机器上的普通文件**。
和下面要的制品仓库不是一回事：没有 id、没有归属、没有写入、没有血缘。
新模块不要复用这个名字，叫 `dag::artifact`（或 `store::artifact`），职责是**节点产物的托管与引用**。

### 2.4 DAG 节点的数据流

`DagNode`：`id / title / task / depends_on / agent / workspace / tools / permission / retry / timeout / gate / position`。
`task` 里的占位符只有 `{{nodes.<id>.output}}`（校验器 `validate.rs` 会检查越界引用）。

`NodeOutput`：`text / tokens_in / tokens_out / turns / steps`。

也就是说：上游跑出 200 KB 报告，下游 prompt 里就是 200 KB 文本。
单机能跑，但两三层之后必然撑爆上下文——清单里说的"没有制品管理，DAG 一跑多节点、大输出，上下文直接炸掉"，
在现在的代码里是**成立**的。

### 2.5 Planner 的形态

`dag::planner::draft(goal, project, agents, max)`：拼提示词 → 调模型 → 解析 JSON → 校验 → 失败喂回一次。
它是**一次性生成器**，不是常驻 Agent。图上没有"Planner 自己决定下一步派谁"的回路，
调度由 `dag::executor` 的 Rust 循环负责。

这带来一个必须说清的设计约束：**Prism 的调度在 Rust 侧**。
所谓"Planner 工具"只能是落在现有调度器上的薄壳，不能真把调度搬进模型回合。

### 2.6 权限与工作区

`Permission` 三档；`Workspace` 三档（shared / worktree / scratch）。
没有"网络出站策略"，也没有"恶意样本沙箱"这个概念——`run_command` 直接在项目目录里跑。

## 3. 目标形态

### 3.1 四层结构

```text
┌ Planner（可选 Agent 化）── prism.dag.create / modify / cancel / status.query
├ 子 Agent 工种 ── 每个只持有自己那组工具（按 tag 白名单）
├ 制品仓库 ────── 所有大产物只存 id，节点之间传引用
└ 节点数据流 ──── input_artifacts / output_artifact_key / parent_artifact_id
```

顺序不能反：**先有制品与数据流，工具才有地方放产出**；否则接进来的每个工具都在往上下文里倒垃圾。

### 3.2 工具标签体系（`ToolDef` 扩展）

**命名**：核心能力用 `prism.*`（`artifact` / `text` / `json` / `dag` / `agent`）；
领域工具按线加前缀——`bin.*` 逆向、`recon.*` 侦察、`web.*` Web、`exp.*` 验证。
清单里的 `json.jq` 归到 `prism.json.jq`，不要出现三个不同前缀的同类工具。

```rust
pub tags: &'static [&'static str],  // bin:static / bin:dyn-sandbox / dag:planner / sec:exp-verify
pub risk: Risk,                     // read / write / exec / attack
pub network: Network,               // none / outbound / isolated
pub sandbox: Option<Sandbox>,       // code / malware
```

- 风险 `attack`（POC 执行、爆破、提权）与 `bin:dyn-sandbox`：**默认不发给任何 Agent**，
  必须 profile 里 `allowHighRisk: true` 且节点执行前有一次人工闸门；
- `network` 由执行器落到进程环境（本期只做"记录 + 拒绝越权"，真正断网在 P2 沙箱里做）；
- 标签只增不改语义，`permission` 三档继续管文件系统，两者正交。

### 3.3 制品仓库（P0 的核心）

```text
~/.prism/artifacts/<dag_id>/
  index.json          # 元数据列表（原子写）
  <artifact_id>       # 内容原样存（二进制/文本均可）
```

```rust
pub struct ArtifactRef {
    pub id: String,            // art-<时间>-<序号>
    pub dag_id: String,
    pub node_id: String,       // 谁产的
    pub key: String,           // 语义键：report / pcap / strings / pseudocode
    pub kind: String,          // text / json / binary / image
    pub name: String,          // 原始文件名（有的话）
    pub size: u64,
    pub sha256: String,
    pub parent_id: Option<String>,  // 血缘：脱壳产出指向原始样本
    pub created_at: i64,
}
```

规则：

1. **节点结束自动落制品**：`NodeOutput.text` 存成 `key = outputArtifactKey`（默认 `report`），
   执行器把 `artifactRef` 写进 `DagNodeRun`；
2. **同 DAG 内可读，跨 DAG 拒绝**：`get(dag_id, id)` 里先校验 `ref.dag_id == dag_id`；
3. **原始样本只读**：`put` 只写不改；脱壳 / dump 的产物是新 id + `parent_id`，原样本 hash 永存；
4. **配额**：单制品默认 256 MiB，单 DAG 默认 2 GiB；超限报错而不是静默截断；
5. **清理**：删计划不自动删制品（留一次确认），运行记录里保留引用，便于回溯。

### 3.4 节点数据流（P0）

`DagNode` 增加两个字段（都可缺省，老计划不受影响）：

```json
{
  "inputArtifacts": [{ "from": "research", "key": "report", "as": "notes" }],
  "outputArtifactKey": "report"
}
```

占位符扩展：

| 写法 | 注入内容 |
|---|---|
| `{{nodes.research.output}}` | 兼容保留；超过 2 KB 时**自动截断**成摘要 + 一行「完整内容见 artifact art-xxx」 |
| `{{artifacts.notes.id}}` | 引用 id（下游拿去调 `prism.artifact.get`） |
| `{{artifacts.notes.summary}}` | 前 2 KB 摘要 |
| `{{artifacts.notes.meta}}` | `{kind,size,sha256,key}` JSON |

截断阈值做成常量（`PROMPT_INLINE_LIMIT = 2048`），不是每个节点自己配——
否则同一个 DAG 里每个节点都要解释一遍"为什么你看到的是半截"。

### 3.5 Planner 工具面（P1.5，7 个）

| 工具 | 落到哪 | 边界 |
|---|---|---|
| `prism.dag.create` | `dag::store::save` | 只建计划，不运行 |
| `prism.dag.status.query` | `DagHub` 运行快照 | 只读 |
| `prism.dag.node.dispatch` | 现有的"就绪即放行" | 用于动态补跑单个 pending 节点 |
| `prism.dag.node.update` | `DagNodeRun` 状态 | **子 Agent 唯一可用的 dag 工具**，且只能写自己节点 |
| `prism.dag.modify` | 计划落盘 + 仅改 pending 节点 | 运行中的 done/running 节点不可改 |
| `prism.dag.cancel` | `RunHandle::cancel_flag` | 连带 interrupt 子会话 |
| `prism.agent.capability.query` | `agents::list()` 投影 | 只读，给 Planner 做匹配 |

关键边界（清单里最强调的一条）：

- Planner 的私有工具集含 `dag:*`；**子 Agent 的 private 只放 `prism.dag.node.update`**，
  并且执行器在调用前校验 `node_id == 当前节点`；
- 子 Agent 拿不到 `dag.create` / `dag.modify` / `dag.cancel`，
  "越权改拓扑"在工具解析层就不成立，不靠提示词自觉。

### 3.6 两个沙箱

| | code 沙箱 | 恶意样本沙箱 |
|---|---|---|
| 用途 | LLM 算脚本、转数据 | 跑样本、动态分析 |
| 权限 tag | 无（原生工具） | `bin:dyn-sandbox` |
| 网络 | 跟随节点策略 | 默认**阻断**，显式 `allow_network: true` 才放行 |
| 本期 | 已有 `run_command`（无隔离） | 不实现，只留标签与审批位 |

清单里"两套沙箱物理隔离"是对的，但本期不做；先把标签和开关位置留出来，
避免以后接 Cuckoo 时被迫改 Agent profile 结构。

## 4. 工具矩阵

### 4.1 逆向线（主体）

| 子 Agent | 工具 | 能力 | 权限 / 风险 | 产出 artifact | 状态 |
|---|---|---|---|---|---|
| 样本预处理 | `bin.meta.identify` | PE/ELF/Mach-O/DEX/APK 识别、架构、位数、段 | 只读 / 低 | 元信息 JSON | ✅ 已实现 |
| 样本预处理 | `bin.hash.compute` | md5 / sha1 / sha256 | 只读 / 低 | 哈希清单 | ✅ 已实现 |
| 样本预处理 | `bin.strings.extract` | ASCII/UTF-16 字符串 + 可疑标记（IP/URL/路径/cmd） | 只读 / 低 | strings JSON | ✅ 已实现 |
| 样本预处理 | `bin.pack.detect` | UPX / VMProtect / Themida / ConfuserEx / 加固厂商等 38 条规则 + 节表结构信号 | 只读 / 低 | 壳名 + 置信度 + 证据偏移 | ✅ 已实现（`forensics.rs::detect_pack`） |
| 脱壳 | `bin.unpack.upx` | UPX 自动解压 | 写临时区 / 中 | 新二进制 + parent | ⬜ 需本机装 `upx` |
| 脱壳 | `bin.unpack.dumper` | 沙箱内 dump 脱壳 | `bin:dyn-sandbox` / 高 | 新二进制 + parent | ⬜ 需沙箱 |
| 脱壳 | `bin.dotnet.deobfuscate` | .NET 反混淆导出 IL | 写 / 中 | IL | ⬜ 需 dnlib / ILSpy |
| 静态分析 | `bin.ghidra.analyze` | Ghidra headless 自动分析 | 只读 / 中 | 项目归档 + 函数索引 | ⬜ 需 Ghidra |
| 静态分析 | `bin.ghidra.decompile_func` | 按需反编译单函数 | 只读 / 中 | 单函数伪 C | ⬜ 需 Ghidra |
| 静态分析 | `bin.xref.search` | 交叉引用检索 | 只读 / 低 | 引用地址列表 | ✅ 已实现（带段与 VA 换算） |
| 静态分析 | `bin.importexport.list` | IAT/EAT + 高危 API 标记 | 只读 / 低 | 高危 API 清单 | ✅ 已实现（PE IAT/EAT + ELF 动态符号） |
| 静态分析 | `bin.radare2.scan` | 轻量 opcode / 函数边界扫描 | 只读 / 中 | 扫描报告 | ⬜ 需 radare2 |
| 密码分析 | `bin.crypto.signature_scan` | AES S 盒 / RC4 表 / RSA 模数特征 | 只读 / 中 | 算法 + 地址 | ✅ 已实现（`bin.crypto.scan`） |
| 密码分析 | `bin.crypto.xor_brute` | 单/多字节 XOR 爆破 | 只读 / 中 | 密钥 + 解密结果 | ✅ 单字节已实现（`xorProbe`），多字节未做 |
| 密码分析 | `bin.crypto.asn1.parse` | PEM/DER 证书与密钥解析 | 只读 / 低 | 密钥信息 | ⬜ 可纯 Rust 做，未开始 |
| 密码分析 | `bin.crypto.hash_identify` | 哈希常量与循环识别 | 只读 / 低 | 算法 + 地址 | ⬜ 循环识别需反汇编 |
| 动态沙箱 | `bin.dyn.sandbox.run` | 隔离沙箱跑样本 | `bin:dyn-sandbox` / 高 | pcap + syscall + 进程树 | ⬜ 需系统级隔离 |
| 动态沙箱 | `bin.dyn.syscall.filter` | 聚焦文件/网络/注入/加密 API | 只读 / 中 | 聚焦日志 | ⬜ 需沙箱 |
| 动态沙箱 | `bin.dyn.mem.dump` | 指定时机 dump 内存 | `bin:dyn-sandbox` / 高 | 内存 artifact | ⬜ 需沙箱 |
| 动态沙箱 | `bin.dyn.trace.breakpoint` | 断点跟踪寄存器与参数 | `bin:dyn-sandbox` / 高 | 轨迹 | ⬜ 需调试器 + 沙箱 |
| 控制流 | `bin.cfg.extract` | CFG 导出 dot/mermaid/json | 只读 / 中 | CFG | ⬜ 需反汇编器 |
| 控制流 | `bin.vm.detect` | 虚机保护识别 | 只读 / 中 | 类型 + 置信度 | ⬜ 需 Ghidra 支撑 |
| 控制流 | `bin.diff.bin` | 二进制比对（原版 vs 补丁） | 只读 / 中 | 差异函数列表 | ✅ 已实现（`bin.diff`，提前到本轮） |
| 通用支撑 | `prism.artifact.put` / `.get` | 制品写入 / 读取引用 | 同 DAG / 低 | 引用链 | ✅ 已实现 |
| 通用支撑 | `json.jq` | 过滤、提取字段 | 只读 / 低 | 清洗后 JSON | ✅ 已实现（`bin.json.jq`，不支持管道） |
| 通用支撑 | `prism.text.extract` | 伪 C 分块、摘要、清洗 | 只读 / 低 | 摘要块 | ⬜ 等 Ghidra 产出伪 C 才有意义 |
| 通用支撑 | `bin.yara.rule_gen` | 自动生成 yara 规则 | 写 / 低 | 规则集 | ⬜ P3 |
| 通用支撑 | `bin.report.rev` | 逆向报告渲染 | 只读 / 低 | 报告 artifact | ⬜ P3（现由 `sec-assist` 工种代劳） |

**这张表 28 行，已实现 11 行**（其中 2 行合并成一个工具）。另外还落地了表里没有的
**7 个 Planner 工具**（`prism.dag.*`，见 `engines/native/dag_tools.rs`），
所以本机可用的领域工具是 **9 个 `bin.*` + 2 个 `prism.artifact.*` + 7 个 `prism.dag.*` = 18 个**。

剩下 17 行不是"没写"，是**卡在环境依赖**上，分三类：

| 依赖 | 涉及条目 | 说明 |
|---|---|---|
| 外部程序（装了就能接） | Ghidra ×2、radare2、upx、dnlib | 走 CLI / MCP，Prism 侧只负责调用与产物落盘 |
| 系统级隔离 | `bin.dyn.*` ×4、`unpack.dumper`、`vm.detect` | 没有真沙箱就不该跑样本，`dynamic-sandbox` 工种现在明确写「本机没有隔离」 |
| 需要反汇编基础设施 | `cfg.extract`、`crypto.hash_identify`、`text.extract` | 都是 Ghidra 的下游，先有反汇编才有它们 |


### 4.2 渗透侦察线（摘要，不在本期）

侦察（主机发现 / 端口扫描 / Web 侦察）→ 验证（CVE 匹配 / POC 验证 / Web 审计）→ 利用（提权 / 凭证）→ 支撑。
结构与本表一致，风险等级更高：`sec:exp-verify` 起全部需要显式开关 + 审批 + 独立日志。
**本期不实现任何攻击类工具**，只在标签体系里预留 `attack` 风险位。

## 5. 实施顺序（已确认）

### P0 制品与数据流（本次先做）

1. `dag::artifact` 模块：`put / get / list / stat`，路径 `~/.prism/artifacts/<dag_id>/`；
2. `DagNode` 加 `input_artifacts` / `output_artifact_key`，老计划缺省即空；
3. 执行器：节点结束自动落制品；拼 prompt 时按 `artifacts.*` 注入摘要/id，
   `nodes.x.output` 超 2 KB 自动截断；
4. 前端：节点检查器加"产物"一栏（key、大小、sha256、父产物），运行态里可点开看摘要。

**验收**：三节点图，研究节点产 200 KB；下游 prompt 中该段 ≤ 2 KB 且带 `art-xxx`；
`artifact.get` 取回全文长度一致；老计划（无新字段）仍能跑通、单测全绿。

### P0.5 四个真能跑的本机工具

`bin.meta.identify`（手写 PE/ELF/Mach-O 头解析，不引重型库）、
`bin.hash.compute`（新增 `sha2` / `sha1` / `md-5`）、
`bin.strings.extract`（ASCII + UTF-16LE，限条数、可疑标记）、
`bin.pack.detect`（38 条壳/保护器/打包器规则 + 节表结构信号 + 熵）。
四者都走 `ToolDef` 新标签，产物写 artifact，不进上下文。

`bin.pack.detect` 的置信度只有三档，且**纯特征串最高只到 `high`**：
节名在 PE 头里是结构事实（`certain`），字符串可能只是编译器内嵌的（`high` + `note` 提示按偏移人工确认）。
`.adata`、`.text1` 这类多壳共用节名单独出现只算 `low`，不单独定案。

**验收**：拿一个真实二进制（如 app 的 Mach-O）跑三个工具，
返回的 arch / bits / 段数 / 三个哈希与 `file`、`shasum -a 256` 一致；strings 命中 URL 与路径样本。

### P1～P3

P1：`json.jq`、`bin.xref.search`、`bin.importexport.list`、crypto 特征扫描、`bin.diff.bin`；
P1.5：Planner 工具面（7 个）+ `allowHighRisk` 开关；
P2：Ghidra（MCP / CLI）、`bin.cfg.extract`、脱壳；
P3：动态沙箱（独立进程隔离 + 网络策略 + 并发池）、内存 dump、凭证类工具。

每条都要满足：**未接入的工具必须明确报"未接入"**，不允许返回空结果假装成功。

## 6. 安全红线

1. 只允许在**授权环境**使用；攻击类工具默认关闭，开启需要 profile 级开关 + 节点闸门 + 审计日志；
2. 所有高危调用记录：谁（agent / node）、什么（tool / payload 摘要）、何时、结果；
3. 一键 `dag.cancel` 必须能连带中断正在跑的子会话；
4. 网络策略按 Agent 隔离：逆向动态分析默认断网，报告类禁止出站；
5. 原始样本只读、hash 永存，任何派生文件都是新 artifact 并记录 `parent_id`。

## 7. 不做 / 待确认

**不做**：跨 DAG 制品共享；攻击类工具实现；样本沙箱（P3 前）；制品加密与远程同步。

**待确认**：

1. 制品默认配额（256 MiB / 2 GiB）是否合适——样本动辄几百 MB，可能要按 DAG 配；
2. `bin.meta.identify` 是否需要解析 PE 编译时间与 Rich header（要做的话得写资源目录遍历）；
3. Planner 工具化后，是否允许它**绕过 UI 弹窗**直接建图并开跑——这决定审批点放在哪。
