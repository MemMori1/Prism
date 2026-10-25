//! Agent 注册表：内置 presets + 用户自定义 profile。
//!
//! 定位
//! ----
//! 此前 Agent 只是 `native/subagent.rs` 里的两个枚举分支（`explore` / `general`），
//! 想知道"有哪些 Agent、各自能用什么工具"只能读代码。这里把它抬成**数据**：
//!
//! ```text
//! ~/.prism/agents/user/<id>.json     用户自定义（可改、可删）
//! 代码里的 builtin()                  内置 presets（升级即覆盖，不会被改坏）
//! ```
//!
//! 合并规则只有一条：**同 id 时 user 覆盖 builtin**，删掉 user 文件即还原内置。
//! 这样"定制"和"还原"都不需要额外的开关，也不需要备份原文件。
//!
//! Agent 是配置不是代码：这里只管读写与校验，真正拿它跑会话的是执行器
//! （`dag.executor.*`，下一里程碑）。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// id 最长多少字符（文件名就是 id，太长了在界面上也放不下）。
const ID_MAX: usize = 32;

/// profile 从哪来。UI 用它区分「内置」与「自定义」角标。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    /// 代码里的 preset。
    #[default]
    Builtin,
    /// `~/.prism/agents/user/` 下的文件。
    User,
}

/// 权限档位。与 dsh 的三档同名，语义对齐（便于执行器直接透传环境变量）。
///
/// 变体的先后顺序就是宽严顺序（`ReadOnly < WorkspaceWrite < FullAccess`），
/// [`Permission::at_most`] 靠它取交集——顺序不能随手调。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Permission {
    /// 只能看：写文件与跑命令一律拒绝。
    ReadOnly,
    /// 只能改工作目录（默认：最小可用且不吓人）。
    #[default]
    WorkspaceWrite,
    /// 全盘可写、从不询问。
    FullAccess,
}

impl Permission {
    pub fn as_str(self) -> &'static str {
        match self {
            Permission::ReadOnly => "read-only",
            Permission::WorkspaceWrite => "workspace-write",
            Permission::FullAccess => "danger-full-access",
        }
    }

    /// 取更严的一档：Agent 画像与节点 / 计划声明求交集。
    ///
    /// 节点只能收紧 Agent 的权限，不能借节点配置提权——只读 Agent 被派进
    /// 一个默认 `workspace-write` 的节点时，仍然只读。
    pub fn at_most(self, other: Permission) -> Permission {
        self.min(other)
    }
}

/// 节点默认在哪种工作区里跑（DAG 节点可以覆盖）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Workspace {
    /// 直接在项目目录跑。只读调研、单写者串行链。
    #[default]
    Shared,
    /// 独立 git worktree + 分支。并行写、需要 diff。
    Worktree,
    /// 空目录，产物只能通过 report / patch 带出。
    Scratch,
}

/// 工具策略：公用白名单 + 私有 + 显式拒绝。
///
/// `shared: None` 是"该 Agent 默认可见的全部公用工具"，不是"没有工具"——
/// 空数组才是"一个公用工具都不要"。这个区别必须保留：绝大多数 Agent 不该被
/// 逼着把内置工具名逐条抄一遍（工具表会变，抄下来就会腐坏）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSelection {
    #[serde(default)]
    pub shared: Option<Vec<String>>,
    #[serde(default)]
    pub private: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

/// 一个 Agent 的完整画像。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProfile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// 由读写路径决定，不信任文件里的值（会被覆盖）。
    #[serde(default)]
    pub origin: Origin,
    /// 这个 profile 盖掉了一个同名内置 preset（UI 显示「覆盖内置」）。
    #[serde(default)]
    pub overrides_builtin: bool,
    /// 执行器：`native` 或引擎 id（`dsh` / `codex` / `opencode` / `claude`）。
    #[serde(default = "default_executor")]
    pub executor: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub permission: Permission,
    #[serde(default)]
    pub workspace: Workspace,
    #[serde(default)]
    pub tools: ToolSelection,
    /// 引用 `~/.prism/mcp.json` 的 server id（执行器按需懒启动）。
    #[serde(default)]
    pub mcp: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// worktree 缺依赖时从主仓 symlink 过去的目录（如 `node_modules`）。
    #[serde(default)]
    pub link_dirs: Vec<String>,
    /// 该 Agent 的节点默认要人工确认。
    #[serde(default)]
    pub gate_default: bool,
}

fn default_executor() -> String {
    "native".to_string()
}

fn tools(shared: &[&str], private: &[&str], deny: &[&str]) -> ToolSelection {
    ToolSelection {
        shared: Some(shared.iter().map(|item| item.to_string()).collect()),
        private: private.iter().map(|item| item.to_string()).collect(),
        deny: deny.iter().map(|item| item.to_string()).collect(),
    }
}

fn preset(
    id: &str,
    name: &str,
    description: &str,
    executor: &str,
    permission: Permission,
    workspace: Workspace,
    system_prompt: &str,
    tags: &[&str],
    selection: ToolSelection,
) -> AgentProfile {
    AgentProfile {
        id: id.to_string(),
        name: name.to_string(),
        description: description.to_string(),
        origin: Origin::Builtin,
        overrides_builtin: false,
        executor: executor.to_string(),
        model: None,
        system_prompt: system_prompt.to_string(),
        permission,
        workspace,
        tools: selection,
        mcp: Vec::new(),
        tags: tags.iter().map(|item| item.to_string()).collect(),
        link_dirs: if workspace == Workspace::Worktree {
            vec!["node_modules".to_string()]
        } else {
            Vec::new()
        },
        gate_default: false,
    }
}

/// 内置 presets。顺序即 UI 展示顺序。
///
/// `explore` / `general` 与 `native/subagent.rs` 的 `AgentType` 一一对应：
/// 老会话里的 `task` 工具调用（`agent_type: "explore"`）解析结果不会因为
/// 这次改动而变。
pub fn builtin() -> Vec<AgentProfile> {
    let mut presets = vec![
        preset(
            "explore",
            "探索者",
            "只读调研：定位文件、梳理调用链，结论必须带「文件:行号」证据。",
            "native",
            Permission::ReadOnly,
            Workspace::Shared,
            "你负责只读调研。先搜再读，结论必须有「文件:行号」证据；不修改任何文件。",
            &["调研"],
            // `list_sessions` 是 IPC 命令不是工具，早先误写进白名单；
            // 白名单里挂着不存在的名字等于给用户一个假承诺，删掉。
            // `fetch` 给调研用：查文档 / CVE 比凭记忆编版本号靠谱，只读、有超时与上限。
            tools(&["read_file", "grep", "list_files", "fetch"], &[], &[]),
        ),
        preset(
            "general",
            "通用助手",
            "默认 Agent：可读改文件、跑命令，处理没有专属 Agent 的杂活。",
            "native",
            Permission::WorkspaceWrite,
            Workspace::Shared,
            "你是一个通用工程助手。先读再改，保持仓库现有风格。",
            &["通用"],
            tools(
                &[
                    "read_file",
                    "grep",
                    "list_files",
                    "write_file",
                    "edit_file",
                    "run_command",
                    "run_script",
                    "trash",
                    "fetch",
                ],
                &[],
                &[],
            ),
        ),
        preset(
            "frontend",
            "前端工程师",
            "前端实现：组件、样式、交互，保持现有设计令牌与间距体系。",
            "native",
            Permission::WorkspaceWrite,
            Workspace::Worktree,
            "你负责前端实现。改动保持仓库现有风格，不引入新组件库；先读再改。",
            &["前端", "实现"],
            tools(
                &[
                    "read_file",
                    "write_file",
                    "edit_file",
                    "run_command",
                    "run_script",
                    "trash",
                    "grep",
                    "fetch",
                ],
                &[],
                &[],
            ),
        ),
        preset(
            "backend",
            "后端工程师",
            "后端实现：Rust / IPC / 命令接线，落盘与事件语义对齐现有约定。",
            "native",
            Permission::WorkspaceWrite,
            Workspace::Worktree,
            "你负责后端实现。命令薄壳、逻辑进模块，事件信封与现有约定一致。",
            &["后端", "实现"],
            tools(
                &[
                    "read_file",
                    "write_file",
                    "edit_file",
                    "run_command",
                    "run_script",
                    "trash",
                    "grep",
                    "fetch",
                ],
                &[],
                &[],
            ),
        ),
        preset(
            "architect",
            "架构师",
            "设计接口、状态机与数据模型，产出契约与取舍说明，不写实现。",
            "native",
            Permission::ReadOnly,
            Workspace::Shared,
            "你负责架构设计。产出契约、状态机与取舍，不改文件。",
            &["设计"],
            tools(&["read_file", "grep", "list_files", "fetch"], &[], &[]),
        ),
        preset(
            "docs",
            "文档工程师",
            "文档与说明：README、变更说明、迁移指引，以实现为准。",
            "native",
            Permission::WorkspaceWrite,
            Workspace::Worktree,
            "你负责文档。以实际实现为准，不夸大、不臆造。",
            &["文档"],
            tools(
                &["read_file", "write_file", "edit_file", "trash", "grep", "fetch"],
                &[],
                &[],
            ),
        ),
        preset(
            "verifier",
            "验证者",
            "合并并验证：跑构建与测试，输出结论、耗时与冲突。",
            "native",
            Permission::WorkspaceWrite,
            Workspace::Worktree,
            "你负责验证。跑构建与测试，输出可复现的结论。",
            &["验证"],
            tools(
                &["read_file", "run_command", "run_script", "trash", "grep"],
                &[],
                &[],
            ),
        ),
        preset(
            "dsh-research",
            "DeepSeek 调研",
            "用 DeepSeek Harness 做只读调研，沙箱内跑，适合不可信任务。",
            "dsh",
            Permission::ReadOnly,
            Workspace::Shared,
            "只读调研，输出证据与结论。",
            &["调研", "沙箱"],
            tools(&["read_file", "grep", "list_files"], &[], &[]),
        ),
        preset(
            "dag-planner",
            "编排规划",
            "在运行中的 DAG 里改图：加删节点、接依赖、每次改完自动校验。",
            "native",
            Permission::WorkspaceWrite,
            Workspace::Shared,
            "你负责改这张图，不是重写这张图。\n\
             先 prism.dag.plan.read 看清现有节点与依赖，再动手；不要凭记忆改。\n\
             小步改：一次一个动作（加一个节点、接一条边），每次都用返回的 issues 确认没坏。\n\
             工具会在校验不过时整体回滚并告诉你原因——那不是失败，是让你看清问题再改一次。\n\
             能复用现有节点就别新建；节点的 task 里引用上游用 {{nodes.<id>.output}}，长报告用 {{artifacts.<id>.summary}}。\n\
             选 Agent 时按工种匹配：识别哈希字符串用 sample-prep，看导入表用 static-analysis，找算法用 crypto-analysis，复核结论用 sec-audit，出报告用 sec-assist。",
            &["编排", "规划"],
            tools(
                &[
                    "read_file",
                    "list_files",
                    "grep",
                    "bin.json.jq",
                    "prism.dag.plan.read",
                    "prism.dag.node.add",
                    "prism.dag.node.update",
                    "prism.dag.node.remove",
                    "prism.dag.edge.add",
                    "prism.dag.edge.remove",
                    "prism.dag.validate",
                ],
                &[],
                &["write_file", "edit_file"],
            ),
        ),
        // ---------------------------------------------------------- 安全研究线
        // 下面八个不是"给软工 Agent 挂上 bin.* 工具"，而是各自绑定了不同的
        // 方法论、工具组合与风险档：给「前端工程师」装上 bin.strings.extract，
        // 它只会把结果当文本读；逆向需要的是"先识别、再定优先级、每条结论带偏移"
        // 这套流程，那只能写在系统提示里。
        preset(
            "sample-prep",
            "样本预处理",
            "收样本第一步：识别格式与架构、算哈希、提字符串、给优先级。只读。",
            "native",
            Permission::ReadOnly,
            Workspace::Shared,
            "你负责样本预处理，只产出事实，不做行为判断。\n\
             顺序固定：bin.meta.identify（格式/架构/入口/编译时间）→ bin.hash.compute（三哈希，作为去重与情报比对的锚点）→ bin.pack.detect（有没有壳，直接决定下游能不能静态分析）→ bin.strings.extract（带可疑标记）。\n\
             如果样本是 APK，再用 bin.apk.sign 记录 v1 / v2 / v3 / v3.1 方案与签名证书指纹；它只解析结构，不验签——不要写「签名有效」。\n\
             bin.pack.detect 的 packed=false 才能说「可直接静态分析」；confidence 是 low 时必须写成「疑似」，并把 evidence 的偏移抄在结论里。\n\
             每条结论都要带偏移或字段名，例如「偏移 0x1a40 有 http://…」。解析不出来的字段写「未知」，不要猜。\n\
             最后给一个 1-5 的优先级与一句理由（有网络特征/有反调试/有已知 C2 域名 → 高）。\n\
             产出写成 JSON 正文（执行器会自动落盘成 key=report 的制品），供下游节点直接取用；只读档调不了 prism.artifact.put，不用试。",
            &["安全", "预处理"],
            tools(
                &[
                    "read_file",
                    "list_files",
                    "bin.meta.identify",
                    "bin.hash.compute",
                    "bin.pack.detect",
                    "bin.apk.sign",
                    "bin.strings.extract",
                    "bin.json.jq",
                    "prism.artifact.get",
                ],
                &[],
                &[],
            ),
        ),
        preset(
            "unpacker",
            "脱壳",
            "判断壳类型并脱壳，产物落新制品并记血缘；原始样本只读。",
            "native",
            Permission::WorkspaceWrite,
            Workspace::Scratch,
            "你负责脱壳。原始样本永远只读：任何产物都写成新文件 + 新制品，并记 parent 指向原样本。\n\
             先跑 bin.pack.detect 定壳：看 packers 的 name 与 confidence、evidence 的偏移，再看 signals（入口是否落在可写可执行节、导入表是否为空、是否有 virtualSize 远大于 rawSize 的节）。\n\
             没有壳就直说「不像加壳」，不要为了交差硬脱；confidence 是 low 时先说「疑似」再说明理由。\n\
             有壳才动手：UPX 走 `bin_unpack_upx`（它自己带 `upx -t` 校验，不硬解；后端没装时这个工具不会出现在清单里）；VMProtect / Themida / ConfuserEx 本机没有工具，直接报「需要 Ghidra + 沙箱（P2）」，不要自造步骤。\n\
             bin.pack.detect 的 hint 是方向，不是命令：照抄之前先确认本机有没有那个程序。\n\
             不要用 run_command 调 upx / r2 / ghidra：它们要么有专用工具，要么还没接；拼 shell 会绕开探测、超时与产物血缘。\n\
             run_command 只用于确定且没有专用工具的辅助命令，绝不执行样本本身。\n\
             脱完必须复核：bin.meta.identify 看入口与段是否正常，bin.pack.detect 确认壳特征已经消失，bin.diff 给出与原样本的差异摘要。",
            &["安全", "脱壳"],
            tools(
                &[
                    "read_file",
                    "list_files",
                    "write_file",
                    "run_command",
                    "bin.meta.identify",
                    "bin.hash.compute",
                    "bin.pack.detect",
                    "bin.unpack.upx",
                    "bin.strings.extract",
                    "bin.diff",
                    "prism.artifact.get",
                    "prism.artifact.put",
                ],
                &[],
                &["edit_file"],
            ),
        ),
        preset(
            "static-analysis",
            "静态分析",
            "从导入表与字符串入手定位可疑行为，结论带「函数 @ 偏移」。只读。",
            "native",
            Permission::ReadOnly,
            Workspace::Shared,
            "你负责静态分析，只读。\n\
             入口是 bin.importexport.list：先看 riskHints，再逐个确认高危 API 的真实用途——imported 了不等于调用了。\n\
             定位用 bin.xref.search：用字符串或字节特征反查引用点，拿到 offset 与 va。\n\
             需要函数边界时先看工具清单：装了 Ghidra 用 bin_ghidra_analyze（带基本块），只有 radare2 用 bin_radare2_scan（只有边界）。\n\
             APK / DEX 不走这两条：bin.dex.disasm 列类并按方法反汇编（能读到 jadx 跳过的方法），bin.dex.xref 给跨类调用边。\n\
             要伪 C 用 bin_ghidra_decompile_func，传 va 或函数名；它给的是 Ghidra 的原文，不要二次编造。两个后端都没装时它们不会出现在清单里；不要用 run_command 调 r2 / analyzeHeadless。\n\
             补充用 bin.strings.extract 找 C2、路径、注册表键、互斥体名。\n\
             每条结论写成「行为 ← 证据」：例如「写注册表自启 ← RegSetValueExA @ 0x4012a0，键名 Software\\Microsoft\\Windows\\CurrentVersion\\Run」。\n\
             严格区分「看到」（有证据）与「推断」（无证据），后者必须显式标注。\n\
             没有伪 C 工具时不要描述伪代码，也不要编函数名。",
            &["安全", "静态分析"],
            tools(
                &[
                    "read_file",
                    "grep",
                    "list_files",
                    "bin.meta.identify",
                    "bin.strings.extract",
                    "bin.xref.search",
                    "bin.dex.disasm",
                    "bin.dex.xref",
                    "bin.importexport.list",
                    "bin.radare2.scan",
                    "bin.ghidra.analyze",
                    "bin.ghidra.decompile_func",
                    "bin.json.jq",
                    "prism.artifact.get",
                ],
                &[],
                &[],
            ),
        ),
        preset(
            "crypto-analysis",
            "密码分析",
            "扫描密码学常量、识别算法与密钥线索，按置信度分级。只读。",
            "native",
            Permission::ReadOnly,
            Workspace::Shared,
            "你负责密码分析，只读。\n\
             先用 bin.crypto.scan 扫常量，再按 confidence 分级处理：high 可直接认定算法，medium 要结合上下文，low 只能写「疑似」。\n\
             RC4 的 0..255 递增表误报很高：没有其它证据时不要写成「用了 RC4」。\n\
             有已知明文特征时用 bin.crypto.xor_brute 反推密钥：默认 1..8 字节，命中多把时取位置最集中的那把；confidence=low 的是未验证候选，不能直接下结论。单字节快查仍可用 bin.crypto.scan 的 xorProbe。\n\
             遇到 PEM / DER / CERT.RSA / CERT.EC，用 bin.crypto.asn1.parse 拿 issuer、有效期、算法与指纹；私钥只报元信息，绝不还原私钥内容。\n\
             找算法调用点用 bin.xref.search 反查常量所在地址周围的引用；APK / DEX 用 bin.dex.xref 看调用边、bin.dex.disasm 读方法体。\n\
             不给出「无法解密的密文」这种结论；解不出来就写清缺什么（密钥、IV、算法参数）。",
            &["安全", "密码"],
            tools(
                &[
                    "read_file",
                    "grep",
                    "bin.meta.identify",
                    "bin.strings.extract",
                    "bin.crypto.scan",
                    "bin.crypto.asn1.parse",
                    "bin.crypto.xor_brute",
                    "bin.xref.search",
                    "bin.dex.disasm",
                    "bin.dex.xref",
                    "bin.json.jq",
                    "prism.artifact.get",
                ],
                &[],
                &[],
            ),
        ),
        preset(
            "dynamic-sandbox",
            "动态沙箱",
            "在受控环境里跑样本并回收行为；本机尚无真实沙箱，执行前须人工确认。",
            "native",
            Permission::WorkspaceWrite,
            Workspace::Scratch,
            "你负责动态行为观察。**本机当前没有真实隔离沙箱**：run_command 跑样本会影响宿主，所以：\n\
             1. 默认只做只读准备（哈希、快照、依赖检查），把「要跑什么、预期看到什么」写清楚；\n\
             2. 只有在用户明确要求、且本条消息里承认风险时才 run_command；一次只跑一个动作，跑完立刻停；\n\
             3. 执行前先 bin.hash.compute 记录样本哈希，执行后 bin.diff 对比文件系统或产物的变化；\n\
             4. 结果里必须写明「未隔离执行」这个前提，不要让人误以为拿到了干净的沙箱报告。\n\
             Ghidra 与真沙箱是 P2/P3 的事，不要假装它们在。",
            &["安全", "动态"],
            tools(
                &[
                    "read_file",
                    "run_command",
                    "bin.meta.identify",
                    "bin.hash.compute",
                    "bin.diff",
                    "prism.artifact.get",
                    "prism.artifact.put",
                ],
                &[],
                &["edit_file", "write_file"],
            ),
        ),
        preset(
            "control-flow",
            "控制流",
            "沿引用链还原调用关系与分发结构，标出直接调用与间接跳转。只读。",
            "native",
            Permission::ReadOnly,
            Workspace::Shared,
            "你负责控制流梳理，只读。\n\
             用 bin.xref.search 把「字符串/常量 → 引用地址 → 所在函数」串成链，每一步都记 offset 与 va。\n\
             用 bin.importexport.list 判断调用是直接（IAT 有名）还是间接（GetProcAddress 动态取址）——后者才是混淆的迹象。\n\
             输出用缩进树或边列表描述调用关系，不要画 ASCII 艺术图。\n\
             机器码样本本机没有反汇编器：只能给到「引用地址」这一级，不要编造基本块、跳转表或函数边界，那需要 Ghidra（P2）。\n\
             DEX（APK / dex）不在此列：bin.dex.disasm 列类、按方法反汇编，bin.dex.xref 给跨类调用边（invoke 目标名齐全）——先用它们把链走通，再谈结论。",
            &["安全", "控制流"],
            tools(
                &[
                    "read_file",
                    "grep",
                    "bin.meta.identify",
                    "bin.xref.search",
                    "bin.dex.disasm",
                    "bin.dex.xref",
                    "bin.importexport.list",
                    "bin.strings.extract",
                    "bin.json.jq",
                    "prism.artifact.get",
                ],
                &[],
                &[],
            ),
        ),
        preset(
            "sec-audit",
            "结论审计",
            "复核上游的否定性与关键结论：换方法重跑，给出成立/推翻/无法复核。只读。",
            "native",
            Permission::ReadOnly,
            Workspace::Shared,
            "你负责结论审计，不负责分析。\n\
             上游已经给出结论；你的任务是挑错：把每一条「否定性 / 排他性 / 关键性」主张找出来，换一种方法重跑，判定它成立不成立。\n\
             判定只有三档：refuted（找到反证，必须给「文件:行号」或工具原始输出，并写清影响）；confirmed（用与上游不同的方法复现）；unverified（拿不到证据，写清缺什么）。没有第四档，也不许写「疑似成立」。\n\
             必须复核四类：①「0 命中 / 未发现 / 不存在 / 无」——默认怀疑，先怀疑搜索方法再怀疑结论；②「未定位 / 在失败类 / 未确认」——用 bin.dex.disasm 先列类名再按方法反汇编；③「仅 / 只有 / 全部」这类排他性主张——专门找漏网的；④数字（大小 / 哈希 / 版本 / 条目数）——重算。\n\
             复核要换方法：拆词、加大小写、换语料根（sources / resources / 原始 dex 字符串）、换工具（grep ↔ bin.dex.xref ↔ bin.strings.extract）。复用上游同一条命令不算复核。\n\
             APK 的「已签名 / 签名有效 / 证书是谁」用 bin.apk.sign 复核：它能证明签名结构是否存在、证书自报字段是什么，但不能证明密码学有效；上游若写「签名有效」，只能改成「存在 v2 签名，未验签」。\n\
             每条主张最多 3 次工具调用；查不到就判 unverified 并写清缺什么，不要烧预算硬猜。\n\
             读全文，不读摘要：上游摘要是截断过的，用 prism.artifact.get 取原文再下判断。\n\
             不做新分析：不要顺手研究上游没提的东西——你的产出是判定，不是第二份报告。\n\
             输出：第一段三行（审了 N 条 / 推翻 X / 确认 Y / 无法复核 Z，加一句最严重的一条）；然后逐条写「编号 | 原文摘录 | 判定 | 方法 | 证据 | 影响（是否改定级）」；最后一行写未覆盖的主张类型与原因。这段正文会被自动落盘成制品，下游按你的节点 id 引用。",
            &["安全", "审计"],
            tools(
                &[
                    "read_file",
                    "list_files",
                    "grep",
                    "bin.json.jq",
                    "bin.dex.disasm",
                    "bin.dex.xref",
                    "bin.strings.extract",
                    "bin.meta.identify",
                    "bin.hash.compute",
                    "bin.apk.sign",
                    "bin.jadx.decompile",
                    "prism.artifact.get",
                ],
                &[],
                &["write_file", "edit_file", "run_command"],
            ),
        ),
        preset(
            "sec-assist",
            "安全辅助",
            "把上游结论整理成报告：以审计为准，缺的一律标未验证。",
            "native",
            Permission::WorkspaceWrite,
            Workspace::Shared,
            "你负责出报告，不做新的分析。\n\
             输入是上游节点的制品；用 prism.artifact.get 取全文或 bin.json.jq 取需要的字段，不要凭记忆复述。\n\
             报告结构固定：结论摘要（3 条以内）→ 证据表（行为 / 证据 / 位置 / 置信度）→ 未验证项 → 建议下一步。\n\
             禁止补写上游没有的证据；上游说「疑似」的地方，报告里也必须写「疑似」。\n\
             若上游有结论审计节点（sec-audit，产出按它的节点 id 引用）：refuted 的主张必须按审计证据改写并注明「审计 A1 推翻」；confirmed 可保留但引用审计编号；unverified 写成「未复核（缺 X）」，不得据此上调风险等级。审计与上游冲突时以审计为准——除非审计拿不出「文件:行号」。\n\
             用 write_file 落 Markdown，同时 prism.artifact.put 存一份（key=report）给下游。",
            &["安全", "报告"],
            tools(
                &[
                    "read_file",
                    "list_files",
                    "grep",
                    "write_file",
                    "edit_file",
                    "bin.json.jq",
                    "prism.artifact.get",
                    "prism.artifact.put",
                ],
                &[],
                &[],
            ),
        ),
    ];
    // 八个工种里只有它会碰宿主（真的跑命令），所以默认过人工闸门。
    // 其余全是只读或只写工作区，没必要每次都点一下。
    if let Some(profile) = presets
        .iter_mut()
        .find(|item| item.id == "dynamic-sandbox")
    {
        profile.gate_default = true;
    }
    presets
}

/// 用户 profile 目录（需要时创建）。
pub fn user_dir() -> Option<PathBuf> {
    let dir = crate::engines::opencode::search::prism_dir()?.join("agents/user");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// 校验并规范化 id：小写、数字、连字符；必须字母数字开头；不允许路径分隔符。
///
/// id 会直接当文件名用，这条校验同时是**路径穿越防线**——`../x` 之类的输入
/// 在这里被拒掉，不需要在写入前再拼一次白名单。
fn normalize_id(raw: &str) -> Result<String, String> {
    let id = raw.trim().to_ascii_lowercase();
    if id.is_empty() {
        return Err("id 不能为空".to_string());
    }
    if id.chars().count() > ID_MAX {
        return Err(format!("id 最长 {ID_MAX} 个字符"));
    }
    if id.chars().count() < 2 {
        return Err("id 至少 2 个字符".to_string());
    }
    let mut chars = id.chars();
    let first = chars.next().unwrap_or(' ');
    if !first.is_ascii_alphanumeric() {
        return Err("id 必须以字母或数字开头".to_string());
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_') {
        return Err("id 只能包含字母、数字、连字符与下划线".to_string());
    }
    Ok(id)
}

/// 去空、去重（保序）。
fn tidy(list: &mut Vec<String>) {
    let mut seen: Vec<String> = Vec::with_capacity(list.len());
    list.retain_mut(|item| {
        let trimmed = item.trim().to_string();
        if trimmed.is_empty() || seen.contains(&trimmed) {
            return false;
        }
        seen.push(trimmed.clone());
        *item = trimmed;
        true
    });
}

/// 规范化后写入：id / 名称 / 各列表都收一遍。
fn normalize(mut profile: AgentProfile) -> Result<AgentProfile, String> {
    profile.id = normalize_id(&profile.id)?;
    profile.name = profile.name.trim().to_string();
    if profile.name.is_empty() {
        return Err("名称不能为空".to_string());
    }
    profile.description = profile.description.trim().to_string();
    profile.executor = profile.executor.trim().to_string();
    if profile.executor.is_empty() {
        profile.executor = default_executor();
    }
    profile.model = profile
        .model
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty());
    tidy(&mut profile.mcp);
    tidy(&mut profile.tags);
    tidy(&mut profile.link_dirs);
    tidy(&mut profile.tools.private);
    tidy(&mut profile.tools.deny);
    if let Some(shared) = profile.tools.shared.as_mut() {
        tidy(shared);
    }
    // 明确拒绝的工具不该同时出现在允许列表里：deny 优先，留着只会让
    // 「模型看到的清单」和配置对不上。
    let denied = profile.tools.deny.clone();
    if let Some(shared) = profile.tools.shared.as_mut() {
        shared.retain(|name| !denied.contains(name));
    }
    profile
        .tools
        .private
        .retain(|name| !denied.contains(name));
    Ok(profile)
}

fn read_profile(path: &Path) -> Result<AgentProfile, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("读取 {} 失败：{error}", path.display()))?;
    serde_json::from_str::<AgentProfile>(&text)
        .map_err(|error| format!("解析 {} 失败：{error}", path.display()))
}

fn write_profile(dir: &Path, profile: &AgentProfile) -> Result<(), String> {
    let path = dir.join(format!("{}.json", profile.id));
    let body = serde_json::to_string_pretty(profile)
        .map_err(|error| format!("序列化 Agent 失败：{error}"))?;
    // 原子写：先写临时文件再 rename，崩在中间不会留下半个 JSON。
    let temp = dir.join(format!(".{}.json.tmp", profile.id));
    std::fs::write(&temp, body)
        .map_err(|error| format!("写入 {} 失败：{error}", temp.display()))?;
    std::fs::rename(&temp, &path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        format!("保存 {} 失败：{error}", path.display())
    })
}

/// 合并内置与用户 profile（同 id 时 user 覆盖 builtin）。
///
/// 坏文件跳过并打一行 stderr：一个手改坏的 JSON 不该让整张 Agent 列表消失。
pub fn list_at(dir: &Path) -> Vec<AgentProfile> {
    let mut out = builtin();
    let mut extras: Vec<AgentProfile> = Vec::new();

    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect();
    paths.sort();

    for path in paths {
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        match read_profile(&path) {
            Ok(mut profile) => {
                // 文件名是 id 的唯一真相：手改过 id 字段也不该造出两份。
                profile.id = stem.to_string();
                profile.origin = Origin::User;
                if let Some(slot) = out.iter_mut().find(|item| item.id == profile.id) {
                    profile.overrides_builtin = true;
                    *slot = profile;
                } else {
                    profile.overrides_builtin = false;
                    extras.push(profile);
                }
            }
            Err(error) => eprintln!("[prism] 跳过 Agent {stem}：{error}"),
        }
    }

    extras.sort_by(|a, b| a.name.cmp(&b.name));
    out.extend(extras);
    out
}

/// 全部 Agent（内置 + 自定义，含覆盖）。
pub fn list() -> Vec<AgentProfile> {
    match user_dir() {
        Some(dir) => list_at(&dir),
        None => builtin(),
    }
}

/// 保存（新建或覆盖）。同 id 的内置 preset 会被"覆盖"，而不是改名。
pub fn save_at(dir: &Path, profile: AgentProfile) -> Result<AgentProfile, String> {
    let mut profile = normalize(profile)?;
    profile.origin = Origin::User;
    profile.overrides_builtin = builtin().iter().any(|item| item.id == profile.id);
    write_profile(dir, &profile)?;
    Ok(profile)
}

pub fn save(profile: AgentProfile) -> Result<AgentProfile, String> {
    let dir = user_dir().ok_or_else(|| "找不到 ~/.prism 目录".to_string())?;
    save_at(&dir, profile)
}

/// 删除自定义 profile。删掉的若是内置覆盖，效果等于"还原内置"。
pub fn delete_at(dir: &Path, id: &str) -> Result<(), String> {
    let id = normalize_id(id)?;
    let path = dir.join(format!("{id}.json"));
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if builtin().iter().any(|item| item.id == id) {
                Err(format!("「{id}」是内置 Agent，没有可删除的自定义覆盖（它本来就在）"))
            } else {
                Err(format!("没有找到 Agent「{id}」"))
            }
        }
        Err(error) => Err(format!("删除 {} 失败：{error}", path.display())),
    }
}

pub fn delete(id: &str) -> Result<(), String> {
    let dir = user_dir().ok_or_else(|| "找不到 ~/.prism 目录".to_string())?;
    delete_at(&dir, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "prism-agents-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn custom(id: &str) -> AgentProfile {
        AgentProfile {
            id: id.to_string(),
            name: "审查员".to_string(),
            description: String::new(),
            origin: Origin::Builtin,
            overrides_builtin: false,
            executor: "native".to_string(),
            model: None,
            system_prompt: String::new(),
            permission: Permission::ReadOnly,
            workspace: Workspace::Shared,
            tools: ToolSelection::default(),
            mcp: Vec::new(),
            tags: Vec::new(),
            link_dirs: Vec::new(),
            gate_default: false,
        }
    }

    #[test]
    fn builtin_covers_the_documented_presets() {
        let ids: Vec<String> = builtin().into_iter().map(|profile| profile.id).collect();
        for expected in [
            "explore",
            "general",
            "frontend",
            "backend",
            "architect",
            "docs",
            "verifier",
            "dsh-research",
            // 安全研究线八个工种
            "sample-prep",
            "unpacker",
            "static-analysis",
            "crypto-analysis",
            "dynamic-sandbox",
            "control-flow",
            "sec-audit",
            "sec-assist",
        ] {
            assert!(ids.contains(&expected.to_string()), "缺少内置 Agent：{expected}");
        }
    }

    /// 工种的价值在"绑了什么"，不在名字：每个安全 Agent 都要有专属系统提示
    /// 和至少一个领域工具，否则它就只是个改了名的通用助手。
    #[test]
    fn security_presets_bind_tools_and_methodology() {
        let all = builtin();
        let security = [
            "sample-prep",
            "unpacker",
            "static-analysis",
            "crypto-analysis",
            "dynamic-sandbox",
            "control-flow",
            "sec-audit",
            "sec-assist",
        ];
        for id in security {
            let profile = all.iter().find(|item| item.id == id).unwrap();
            assert!(
                profile.system_prompt.chars().count() > 80,
                "{id} 的系统提示太短，写不出方法论"
            );
            let shared = profile.tools.shared.as_ref().expect("安全工种要显式列工具");
            assert!(
                shared.iter().any(|name| name.starts_with("bin.")
                    || name.starts_with("prism.artifact.")),
                "{id} 没有任何领域工具：{shared:?}"
            );
            assert!(
                profile.tags.contains(&"安全".to_string()),
                "{id} 缺少「安全」标签"
            );
        }
    }

    /// 样本预处理要能读 APK 签名结构，但提示词必须禁止它把"结构存在"说成"验签通过"。
    #[test]
    fn sample_prep_reads_apk_signature_structure_only() {
        let all = builtin();
        let prep = all.iter().find(|item| item.id == "sample-prep").unwrap();
        let shared = prep.tools.shared.as_ref().expect("预处理要显式列工具");
        assert!(
            shared.contains(&"bin.apk.sign".to_string()),
            "预处理缺 bin.apk.sign：{shared:?}"
        );
        assert!(
            prep.system_prompt.contains("不验签"),
            "提示词必须写清只解析结构：{}",
            prep.system_prompt
        );
        assert!(
            prep.system_prompt.contains("不要写「签名有效」"),
            "提示词必须禁止把结构存在当成验签结论：{}",
            prep.system_prompt
        );
    }

    /// 审计工种必须能读到 jadx 跳过的方法。
    ///
    /// 这条是工种存在的理由：aerial-guard 那次 45 个方法 `Method dump skipped`，
    /// 报告只能写「未定位」，而 `bin.dex.disasm` 本来就在仓库里——不给它白名单，
    /// 审计就退化成把上游结论再读一遍。
    #[test]
    fn sec_audit_can_reach_undedecompiled_methods() {
        let all = builtin();
        let audit = all.iter().find(|item| item.id == "sec-audit").unwrap();
        let shared = audit.tools.shared.as_ref().expect("审计要显式列工具");
        for tool in [
            "bin.dex.disasm",
            "bin.dex.xref",
            "bin.apk.sign",
            "prism.artifact.get",
        ] {
            assert!(
                shared.contains(&tool.to_string()),
                "审计缺 {tool}，反编译失败的方法就没人复核：{shared:?}"
            );
        }
        assert_eq!(audit.permission, Permission::ReadOnly, "审计不该有写权限");
        // 三档判定是提示词的骨架，缺一个模型就会退回「疑似成立」。
        for verdict in ["refuted", "confirmed", "unverified"] {
            assert!(
                audit.system_prompt.contains(verdict),
                "审计提示词缺判定档 {verdict}"
            );
        }
    }

    /// 反编译失败的方法要能靠 DEX 反汇编补证：三个分析工种都得有这两件套。
    #[test]
    fn analysis_presets_can_disassemble_dex() {
        let all = builtin();
        for id in ["static-analysis", "control-flow", "crypto-analysis"] {
            let profile = all.iter().find(|item| item.id == id).expect(id);
            let shared = profile.tools.shared.as_ref().expect("分析工种要显式列工具");
            for tool in ["bin.dex.disasm", "bin.dex.xref"] {
                assert!(
                    shared.contains(&tool.to_string()),
                    "{id} 缺 {tool}：APK 的调用链只能停在「引用地址」这一级"
                );
            }
        }
    }

    /// 节点配置只能收紧 Agent 的权限，不能提权。
    #[test]
    fn node_permission_cannot_exceed_the_agent() {
        use Permission::*;
        assert_eq!(WorkspaceWrite.at_most(ReadOnly), ReadOnly);
        assert_eq!(ReadOnly.at_most(WorkspaceWrite), ReadOnly);
        assert_eq!(FullAccess.at_most(WorkspaceWrite), WorkspaceWrite);
        assert_eq!(WorkspaceWrite.at_most(FullAccess), WorkspaceWrite);
    }

    /// 只有会碰宿主的工种默认过闸门——闸门遍地开就没有信息量了。
    #[test]
    fn only_dynamic_sandbox_gates_by_default() {
        let all = builtin();
        let gated: Vec<String> = all
            .iter()
            .filter(|profile| profile.gate_default)
            .map(|profile| profile.id.clone())
            .collect();
        assert_eq!(gated, vec!["dynamic-sandbox".to_string()]);
    }

    /// 只读工种必须真的只读：申请了 write_file / run_command 的不能标只读。
    #[test]
    fn readonly_security_agents_request_no_write_tools() {
        let all = builtin();
        for profile in all.iter().filter(|item| item.permission == Permission::ReadOnly) {
            let shared = profile.tools.shared.clone().unwrap_or_default();
            for forbidden in ["write_file", "edit_file", "run_command"] {
                assert!(
                    !shared.contains(&forbidden.to_string()),
                    "{} 是只读档，却申请了 {forbidden}",
                    profile.id
                );
            }
        }
    }

    /// 安全线工种不给**出站**工具：样本目录里的内容是不可信的，模型读到一段
    /// 带 URL 的字符串就能把本机信息带出去。要查情报走人工，或将来接
    /// `recon.*` 的专用通道（那条路能按目的域白名单收窄）。
    #[test]
    fn security_presets_have_no_outbound_tools() {
        let all = builtin();
        for profile in all
            .iter()
            .filter(|item| item.tags.contains(&"安全".to_string()))
        {
            let shared = profile.tools.shared.clone().unwrap_or_default();
            assert!(
                !shared.iter().any(|name| name == "fetch"),
                "{} 拿到了出站工具 fetch",
                profile.id
            );
        }
    }

    /// 工种里引用的工具名必须真实存在，否则模型看到的白名单会静默变空。
    #[test]
    fn security_presets_only_reference_known_tools() {
        let known: Vec<String> = crate::engines::native::tools::builtin_tools()
            .iter()
            .map(|tool| tool.name.to_string())
            .collect();
        for profile in builtin() {
            for name in profile.tools.shared.clone().unwrap_or_default() {
                assert!(
                    known.contains(&name),
                    "{} 引用了不存在的工具：{name}",
                    profile.id
                );
            }
        }
    }

    /// `explore` / `general` 必须和 native 子代理的老枚举对得上，否则老会话
    /// 里的 `task` 调用会突然找不到对应 Agent。
    #[test]
    fn explore_and_general_match_subagent_defaults() {
        let all = builtin();
        let explore = all.iter().find(|item| item.id == "explore").unwrap();
        let general = all.iter().find(|item| item.id == "general").unwrap();
        assert_eq!(explore.permission, Permission::ReadOnly);
        assert_eq!(general.permission, Permission::WorkspaceWrite);
    }

    #[test]
    fn rejects_ids_that_could_escape_the_directory() {
        assert!(normalize_id("../etc/passwd").is_err());
        assert!(normalize_id("a/b").is_err());
        assert!(normalize_id("").is_err());
        assert!(normalize_id("x").is_err());
        assert!(normalize_id("-x").is_err());
        assert!(normalize_id("中文").is_err());
        assert_eq!(normalize_id("  Code-Reviewer  ").unwrap(), "code-reviewer");
    }

    #[test]
    fn deny_wins_over_allow() {
        let mut profile = custom("reviewer");
        profile.tools = ToolSelection {
            shared: Some(vec!["read_file".into(), "edit_file".into()]),
            private: vec!["edit_file".into()],
            deny: vec!["edit_file".into()],
        };
        let saved = normalize(profile).unwrap();
        assert_eq!(saved.tools.shared.unwrap(), vec!["read_file"]);
        assert!(saved.tools.private.is_empty());
    }

    #[test]
    fn user_file_overrides_builtin_and_delete_restores_it() {
        let dir = temp_dir("override");
        assert_eq!(list_at(&dir).len(), builtin().len());

        let mut profile = custom("explore");
        profile.name = "我的探索者".to_string();
        save_at(&dir, profile).unwrap();

        let merged = list_at(&dir);
        let explore = merged.iter().find(|item| item.id == "explore").unwrap();
        assert_eq!(explore.name, "我的探索者");
        assert_eq!(explore.origin, Origin::User);
        assert!(explore.overrides_builtin);
        // 覆盖不是新增：总数不变。
        assert_eq!(merged.len(), builtin().len());

        delete_at(&dir, "explore").unwrap();
        let restored = list_at(&dir);
        let explore = restored.iter().find(|item| item.id == "explore").unwrap();
        assert_eq!(explore.name, "探索者");
        assert_eq!(explore.origin, Origin::Builtin);
        assert!(!explore.overrides_builtin);
    }

    #[test]
    fn builtin_cannot_be_deleted_when_not_overridden() {
        let dir = temp_dir("delete-builtin");
        let error = delete_at(&dir, "verifier").unwrap_err();
        assert!(error.contains("内置"), "{error}");
        assert!(delete_at(&dir, "nope").unwrap_err().contains("没有找到"));
    }

    #[test]
    fn broken_file_is_skipped_without_losing_the_rest() {
        let dir = temp_dir("broken");
        save_at(&dir, custom("good")).unwrap();
        std::fs::write(dir.join("broken.json"), "{ not json").unwrap();

        let ids: Vec<String> = list_at(&dir).into_iter().map(|item| item.id).collect();
        assert!(ids.contains(&"good".to_string()));
        assert!(!ids.contains(&"broken".to_string()));
    }

    #[test]
    fn filename_is_the_source_of_truth_for_id() {
        let dir = temp_dir("filename");
        // 文件里写着别的 id，仍以文件名为准——否则一个文件能造出两份 Agent。
        std::fs::write(
            dir.join("real.json"),
            r#"{"id":"fake","name":"手改过","executor":"native"}"#,
        )
        .unwrap();
        let ids: Vec<String> = list_at(&dir).into_iter().map(|item| item.id).collect();
        assert!(ids.contains(&"real".to_string()));
        assert!(!ids.contains(&"fake".to_string()));
    }
}
