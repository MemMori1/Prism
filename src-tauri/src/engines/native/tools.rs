//! 原生引擎的内置工具。
//!
//! 六个就够覆盖日常：读、列、搜、写、改、跑命令。
//! 所有路径先做词法归一 + 前缀校验，禁止越出会话工作目录；命令有超时和输出上限。

use serde_json::{json, Value};
use std::borrow::Cow;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::Manager;

use super::store::StoredTokens;
use crate::tasks::{SpawnRequest, TaskInfo, TasksState};

/// 单次工具输出的字符上限（超出截断，避免一轮就把上下文撑爆）。
const MAX_OUTPUT_CHARS: usize = 20_000;
/// 读文件默认行数。
const READ_LINES: usize = 2000;
/// 列目录的最大条目数。
const LIST_ENTRIES: usize = 300;
/// 搜索命中上限。
const GREP_HITS: usize = 120;
/// 搜索时跳过的大文件（字节）。
const GREP_MAX_BYTES: u64 = 1_500_000;
/// 跑命令时的轮询间隔：停止和超时都按这个粒度看。
const RUN_POLL: Duration = Duration::from_millis(40);
/// 单路管道最多留多少字节：读的人不能停手，但也不必无限往内存里攒
/// （展示时本来就要截断，这里只要留得比 MAX_OUTPUT_CHARS 富余即可）。
const PIPE_KEEP_BYTES: usize = 512 * 1024;
/// 遍历时永远跳过的目录。
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".venv",
    "__pycache__",
    ".turbo",
    ".cache",
    "vendor",
];
/// 遍历时的文件数上限：到顶就在结论里说明，不能装成「都看过了」。
const WALK_MAX_FILES: usize = 4000;
/// 遍历的最大深度。按真实仓库的典型层级给：src-tauri/src/engines/native/*.rs
/// 已经在第 4 层，原来限 3 层会把一多半源码挡在外面（实测漏搜过）。
const WALK_MAX_DEPTH: usize = 6;

/// 遍历被上限截断的情况。
#[derive(Debug, Default, Clone, Copy)]
struct WalkCut {
    files: bool,
    depth: bool,
}

impl WalkCut {
    fn any(&self) -> bool {
        self.files || self.depth
    }

    /// 一句人话：这次只扫了一部分，结论要打折看。
    fn note(&self) -> Option<String> {
        if !self.any() {
            return None;
        }
        let mut reasons = Vec::new();
        if self.files {
            reasons.push(format!("文件数到顶（{WALK_MAX_FILES} 个）"));
        }
        if self.depth {
            reasons.push(format!("目录到顶（{WALK_MAX_DEPTH} 层）"));
        }
        Some(format!(
            "注意：这次扫描被截断了——{}，范围没扫全；把 path 指到更窄的目录再试。",
            reasons.join("、")
        ))
    }
}

#[derive(Debug, Clone)]
pub struct ToolOutcome {
    pub text: String,
    /// 失败时为错误说明（界面上工具卡标红）。
    pub error: Option<String>,
}

impl ToolOutcome {
    pub(crate) fn ok(text: impl Into<String>) -> Self {
        ToolOutcome {
            text: text.into(),
            error: None,
        }
    }

    pub fn fail(text: impl Into<String>) -> Self {
        let text = text.into();
        ToolOutcome {
            error: Some(text.clone()),
            text,
        }
    }
}

// ---------------------------------------------------------------- 注册表类型

/// 工具来源：决定信任级别与失效行为。
///
/// 目前只有 `Builtin`（编译进二进制、完全信任）。后两个变体是给
/// "模型自写工具复用"和"MCP / 插件工具"预留的位——它们的共同点是
/// **不可完全信任**，接进来时必须过沙箱与审批，不能和内置工具
/// 混在同一信任级别里。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolOrigin {
    /// 开发期写死、编译进二进制。
    Builtin,
    /// 模型运行时生成、校验通过后入库复用（尚未启用）。
    #[allow(dead_code)]
    RuntimeGenerated,
    /// 外部插件 / MCP server 提供：由插件在运行时注册，可撤销。
    Plugin,
}

impl ToolOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            ToolOrigin::Builtin => "builtin",
            ToolOrigin::RuntimeGenerated => "generated",
            ToolOrigin::Plugin => "plugin",
        }
    }
}

/// 权限等级：决定"能不能直接跑"和"子代理能不能拿到"。
///
/// 越界写入、任意命令这类更危险的动作没有单列一档，而是由审批闸门
/// （`approval::action_of`）按工具动作升级处理——那一层才是真正拦人的地方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    /// 无副作用，可直接执行。
    ReadOnly,
    /// 会写会话工作目录（写文件 / 改文件 / 跑命令 / 停任务）。
    WriteWorkspace,
}

impl Permission {
    /// 在 `max` 这一档权限内是否放行（子代理白名单用它）。
    fn within(self, max: Permission) -> bool {
        match max {
            Permission::ReadOnly => self == Permission::ReadOnly,
            Permission::WriteWorkspace => true,
        }
    }

    /// 给人和模型看的档位名（与 `agents::Permission` 的写法保持一致）。
    pub fn as_str(self) -> &'static str {
        match self {
            Permission::ReadOnly => "read-only",
            Permission::WriteWorkspace => "workspace-write",
        }
    }
}

/// 风险等级。`permission` 管的是"能不能写文件"，风险管的是"这东西伤不伤人"，
/// 两者正交：`read_file` 与 `bin.strings.extract` 都是只读，但后者面对的是
/// 恶意样本，值得单独标出来。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    Read,
    Write,
    /// 会执行任意代码 / 命令。
    Exec,
    /// 主动攻击类（POC 执行、爆破、提权）。**默认不发给任何 Agent**。
    /// 本期没有任何工具标这一档：位置先留着，接 POC 验证时才有地方写。
    #[allow(dead_code)]
    Attack,
}

impl Risk {
    pub fn as_str(self) -> &'static str {
        match self {
            Risk::Read => "read",
            Risk::Write => "write",
            Risk::Exec => "exec",
            Risk::Attack => "attack",
        }
    }
}

/// 网络策略。本期只做声明与展示，真正的隔离在 P2/P3 的沙箱里落。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    /// 不发包。
    None,
    /// 可以出站（抓网页、查 CVE）。
    Outbound,
    /// 必须断网（动态分析沙箱）。本期只有声明，真正的断网在 P3 的沙箱里做。
    #[allow(dead_code)]
    Isolated,
}

impl Network {
    pub fn as_str(self) -> &'static str {
        match self {
            Network::None => "none",
            Network::Outbound => "outbound",
            Network::Isolated => "isolated",
        }
    }
}

/// 工具跑在哪套沙箱里。
///
/// 两套沙箱**不能复用**：`Code` 是给模型算脚本的，`Malware` 是跑恶意样本的
/// 重型隔离环境。合成一套，迟早会出现"样本在码沙箱里跑"的事故。
///
/// 本期没有任何工具真的跑在这两套沙箱里（`run_command` 没有隔离），
/// 这里只是把标签位留出来——接 P2/P3 时不用改 Agent profile 的结构。
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sandbox {
    Code,
    Malware,
}

impl Sandbox {
    pub fn as_str(self) -> &'static str {
        match self {
            Sandbox::Code => "code",
            Sandbox::Malware => "malware",
        }
    }
}

/// 工具的可用作用域。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolScope {
    /// 哪儿都能用（聊天、子代理、DAG 节点）。
    Any,
    /// 只在 DAG 节点里出现：需要执行器注入的 `dag` 作用域（制品类工具）。
    Dag,
}

/// 一条工具的标签集合（主题外的"元信息"）。
#[derive(Debug, Clone, Copy)]
pub struct ToolMeta {
    /// 领域标签：`bin:static` / `bin:dyn-sandbox` / `dag:planner` / `artifact`。
    pub tags: &'static [&'static str],
    pub risk: Risk,
    pub network: Network,
    pub sandbox: Option<Sandbox>,
    pub scope: ToolScope,
}

impl Default for ToolMeta {
    fn default() -> Self {
        ToolMeta {
            tags: &[],
            risk: Risk::Read,
            network: Network::None,
            sandbox: None,
            scope: ToolScope::Any,
        }
    }
}

/// 工具开关集合。
///
/// 只记"被显式改过"的工具；没记录的按**登记项自带的默认值**走。内置工具
/// 默认开，MCP / 插件工具默认关（除非配置里显式标了 trusted）——这个区分是
/// 刻意的：外部工具不该因为"装了就顺手能用"而在用户不知情下拿到执行权。
#[derive(Debug, Clone, Default)]
pub struct EnabledTools {
    overrides: std::collections::HashMap<String, bool>,
    /// 是否连"默认关"的外部工具一起放行。只有测试和显式信任场景用得到。
    allow_untrusted: bool,
}

impl EnabledTools {
    /// 全开：测试与"完全信任"场景用（含外部工具）。
    pub fn all() -> Self {
        EnabledTools {
            overrides: std::collections::HashMap::new(),
            allow_untrusted: true,
        }
    }

    /// 从配置里的显式记录构造。
    pub fn from_map(overrides: std::collections::HashMap<String, bool>) -> Self {
        EnabledTools {
            overrides,
            allow_untrusted: false,
        }
    }

    /// 读当前配置里的开关。
    pub fn from_config() -> Self {
        EnabledTools::from_map(super::config::load().tools)
    }

    /// 这个工具此刻是否可用：显式记录优先，否则看登记项自带的默认。
    pub fn is_on(&self, def: &ToolDef) -> bool {
        self.overrides
            .get(def.name.as_ref())
            .copied()
            .unwrap_or(def.enabled_by_default || self.allow_untrusted)
    }
}

/// 一次工具调用的上下文。
///
/// 普通工具只用 `directory` + `should_stop`；`task_status` 还要 `app` /
/// `session_id`；子代理（`task`）还要 `call_id` / 模型 / 权限档。
/// 全部收进一个结构，是为了让**所有工具共享同一个签名**——有了统一签名，
/// 一张表才装得下它们。
pub struct ToolContext<'a> {
    /// 无 Tauri 上下文时为空（单测直接跑工具走这条），此时后台任务不可用。
    pub app: Option<&'a tauri::AppHandle>,
    pub session_id: &'a str,
    pub directory: &'a str,
    /// 本次调用的 id（子代理事件按它归属到工具卡）。
    pub call_id: &'a str,
    /// 当前模型服务（只有子代理用得上，普通工具不碰）。
    pub provider: Option<&'a super::config::ProviderConfig>,
    pub model: Option<&'a str>,
    /// 权限档的原始值（read-only / ask / full），子代理据此构造
    /// `approval::Mode`——这里刻意存字符串而不是 `Mode`，免得工具层反过来
    /// 依赖审批层（审批层要查这张表，方向不能反）。
    pub permission: Option<&'a str>,
    /// 当前生效的工具开关：执行前兜底校验，不依赖模型"听话"。
    pub enabled: &'a EnabledTools,
    /// DAG 作用域：由执行器为每个节点注入，模型改不了 `dag_id`。
    /// 聊天与子代理为 `None`，此时 [`ToolScope::Dag`] 的工具根本不在 schema 里。
    pub dag: Option<&'a crate::dag::artifact::DagScope>,
    pub should_stop: &'a dyn Fn() -> bool,
}

/// 工具执行结果。
///
/// `meta` 与 `tokens` 原本只有 `task`（子代理）会带，所以旧的分发处不得不
/// 为它开一个 `call.name == "task"` 的特判。把这两项提升成结果的常规部分后，
/// 特判就消失了——这是"用一个签名装下所有工具"的直接收益。
pub struct ToolResult {
    pub outcome: ToolOutcome,
    /// 附加给前端的进度元数据（子代理的步骤轨迹）。
    pub meta: Option<Value>,
    /// 折叠进会话总量的 token（子代理烧的）。
    pub tokens: Option<StoredTokens>,
}

impl ToolResult {
    /// 普通工具：只有文本结果。
    pub(crate) fn plain(outcome: ToolOutcome) -> Self {
        ToolResult {
            outcome,
            meta: None,
            tokens: None,
        }
    }

    pub fn fail(text: impl Into<String>) -> Self {
        ToolResult::plain(ToolOutcome::fail(text))
    }

    /// 带元数据与 token 的结果（子代理回填走这条）。
    pub fn with_parts(
        outcome: ToolOutcome,
        meta: Option<Value>,
        tokens: Option<StoredTokens>,
    ) -> Self {
        ToolResult {
            outcome,
            meta,
            tokens,
        }
    }
}

/// 工具执行体：统一签名，内置工具因此能直接存成函数指针。
pub type ToolRun = fn(&ToolContext<'_>, &Value) -> ToolResult;

/// 动态工具的执行体：要捕获客户端句柄（MCP 连接），函数指针装不下。
pub type BoxedRun = dyn for<'a> Fn(&ToolContext<'a>, &Value) -> ToolResult + Send + Sync;

/// schema 的来源。
///
/// 内置工具用生成器（只在真要发给模型时 `json!` 一遍，省得启动就构造一堆
/// `Value`）；外部工具拿到的是服务器给的原样 schema，没有"生成"这一步。
pub enum ToolSchema {
    Build(fn() -> Value),
    Fixed(Value),
}

impl ToolSchema {
    fn render(&self) -> Value {
        match self {
            ToolSchema::Build(build) => build(),
            ToolSchema::Fixed(value) => value.clone(),
        }
    }
}

/// 执行体的两种形态。
pub enum ToolImpl {
    Fn(ToolRun),
    Boxed(Arc<BoxedRun>),
}

/// 一条工具登记项：声明 + 权限 + 审批信息 + 执行体，全在一处。
///
/// 加一个内置工具 = 往 `builtin_tools()` 加一条；加一类外部工具 = 插件调
/// [`ToolRegistry::register`] 登记，撤的时候 [`ToolRegistry::release`]。
/// schema、模型可见清单、分发、审批卡片、子代理白名单全部从登记项推导，
/// 不再"改一处漏三处"。
pub struct ToolDef {
    /// 稳定工具名。内置的是静态串；外部工具带服务器命名空间（`srv__tool`），
    /// 所以用 `Cow` 装。
    pub name: Cow<'static, str>,
    /// schema 生成器。
    pub schema: ToolSchema,
    pub origin: ToolOrigin,
    pub permission: Permission,
    /// 审批动作（None = 无需授权，如只读工具）。
    pub action: Option<&'static str>,
    /// 权限卡片里"动了什么"取哪个入参（None = 不展示资源）。
    pub resource_key: Option<&'static str>,
    /// 权限卡片标题（`action` 为 Some 时才有意义）。
    pub summary: &'static str,
    /// 设置页里给**人**看的一句简介。
    ///
    /// 和 `summary`（审批标题）、schema 里给模型看的长描述都不是一回事：
    /// 它只占据一个小格子的第二行，超过十来个字就会被省略号吃掉。
    pub blurb: Cow<'static, str>,
    /// 能否被委派给子代理（扁平编排：`task` 自己永远不行）。
    pub delegable: bool,
    /// 没被开关覆盖时的默认状态。
    pub enabled_by_default: bool,
    /// 领域标签 / 风险 / 网络 / 沙箱 / 作用域（见 [`ToolMeta`]）。
    pub meta: ToolMeta,
    pub run: ToolImpl,
}

impl ToolDef {
    /// 发给模型的声明。
    ///
    /// 名字在这里统一换成 [`wire_name`](Self::wire_name)：内部名可以带点分层
    /// （`bin.meta.identify`），但 OpenAI / Anthropic / Gemini 的函数名都只接受
    /// `[A-Za-z0-9_-]`，点号会让整个请求 400，模型一次都轮不到。
    pub fn schema_value(&self) -> Value {
        let mut value = self.schema.render();
        if let Some(function) = value.get_mut("function").and_then(Value::as_object_mut) {
            function.insert("name".to_string(), json!(self.wire_name()));
        }
        value
    }

    /// 模型侧看到的工具名：非法字符一律换成下划线。
    pub fn wire_name(&self) -> String {
        wire_name(&self.name)
    }

    pub fn call(&self, ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
        match &self.run {
            ToolImpl::Fn(run) => run(ctx, args),
            ToolImpl::Boxed(run) => run(ctx, args),
        }
    }
}

/// 把内部工具名压成 provider 接受的形状。
///
/// 允许的字符集来自 OpenAI 的 `^[a-zA-Z0-9_-]+$`（Anthropic 与 Gemini 同样
/// 只收这个范围）。点号是唯一的越界来源，所以 `bin.meta.identify` 变成
/// `bin_meta_identify` 这种一眼还能读回来的形式。
pub fn wire_name(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

// ---------------------------------------------------------------- 工具声明

fn read_file_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "read_file",
            "description": "读取工作目录里的一个文本文件，带行号。",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "相对工作目录的路径" },
                    "offset": { "type": "integer", "description": "起始行（1 开始），默认 1" },
                    "limit": { "type": "integer", "description": "最多读多少行，默认 2000" }
                },
                "required": ["path"]
            }
        }
    })
}

fn list_files_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "list_files",
            "description": "列出目录下的文件，最多下探六层。",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "相对路径，默认工作目录根" },
                    "pattern": { "type": "string", "description": "简单的通配过滤，如 *.rs" }
                }
            }
        }
    })
}

fn grep_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "grep",
            "description": "在文件内容里搜子串，返回 文件:行号:内容。",
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "要搜的文本" },
                    "path": { "type": "string", "description": "搜索起点，默认工作目录根" },
                    "ignore_case": { "type": "boolean", "description": "忽略大小写" }
                },
                "required": ["pattern"]
            }
        }
    })
}

fn write_file_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "write_file",
            "description": "写文件（新建或整体覆盖）。",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }
        }
    })
}

fn edit_file_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "edit_file",
            "description": "把文件里的 old_string 精确替换成 new_string；old_string 必须唯一，或显式 replace_all。",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_string": { "type": "string", "description": "要替换的原文（含缩进）" },
                    "new_string": { "type": "string" },
                    "replace_all": { "type": "boolean" }
                },
                "required": ["path", "old_string", "new_string"]
            }
        }
    })
}

fn run_command_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "run_command",
            "description": "在工作目录里跑一条 shell 命令，返回退出码和输出。长命令（构建 / 全量测试 / dev server / 预计超过 ~30 秒）把 background 设成 true：立即返回任务 id，进程进「后台任务」坞继续跑，你不需要干等，之后用 task_status 看进度。",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout_ms": { "type": "integer", "description": "前台执行的超时毫秒，默认 120000（background=true 时不生效）" },
                    "background": { "type": "boolean", "description": "true = 转后台：立即返回，输出进任务坞 / 终端页，用 task_status 查" }
                },
                "required": ["command"]
            }
        }
    })
}

fn task_status_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "task_status",
            "description": "查看本会话的后台任务（run_command background=true 起的，以及用户新建的终端任务）。不传 id：列出全部；传 id：返回该任务最近输出；stop=true：停止它。",
            "parameters": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "任务 id；省略则列出全部" },
                    "lines": { "type": "integer", "description": "看多少行尾部输出，默认 120，最多 400" },
                    "stop": { "type": "boolean", "description": "true = 停止这个任务（需要 id）" }
                }
            }
        }
    })
}

fn tools_list_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "tools_list",
            "description": "列出此刻真正能调用的工具（已按权限档、工作区作用域和设置里的开关过滤），以及被挡住的原因。不确定自己有什么能力、或想找某个专用工具时先调它，不要凭印象猜工具名。",
            "parameters": {
                "type": "object",
                "properties": {
                    "tag": {
                        "type": "string",
                        "description": "只看带某个标签的工具，如 bin:static / bin:dyn-sandbox / dag:planner / artifact"
                    },
                    "include_blocked": {
                        "type": "boolean",
                        "description": "是否列出被挡住的工具及原因，默认 true"
                    }
                }
            }
        }
    })
}

// ---------------------------------------------------------------- 注册表

/// 内置工具的登记项。懒构建一次，之后所有查询共享同一批 `Arc`。
///
/// 顺序即模型看到的顺序：只读在前，写类在后，`task`（子代理）最后。
pub fn builtin_tools() -> &'static [Arc<ToolDef>] {
    static BUILTINS: OnceLock<Vec<Arc<ToolDef>>> = OnceLock::new();
    BUILTINS.get_or_init(|| {
        let mut list = vec![
            builtin(
                "read_file",
                "读取文件",
                read_file_schema,
                Permission::ReadOnly,
                None,
                None,
                "",
                true,
                run_read_file,
            ),
            builtin(
                "list_files",
                "列出目录",
                list_files_schema,
                Permission::ReadOnly,
                None,
                None,
                "",
                true,
                run_list_files,
            ),
            builtin(
                "grep",
                "按内容搜索",
                grep_schema,
                Permission::ReadOnly,
                None,
                None,
                "",
                true,
                run_grep,
            ),
            builtin(
                "write_file",
                "写入文件",
                write_file_schema,
                Permission::WriteWorkspace,
                Some("edit"),
                Some("path"),
                "写文件需要授权",
                true,
                run_write_file,
            ),
            builtin(
                "edit_file",
                "局部改文件",
                edit_file_schema,
                Permission::WriteWorkspace,
                Some("edit"),
                Some("path"),
                "修改文件需要授权",
                true,
                run_edit_file,
            ),
            builtin_full(
                "run_command",
                "执行命令",
                run_command_schema,
                Permission::WriteWorkspace,
                Some("run"),
                Some("command"),
                "运行命令需要授权",
                true,
                // 会执行任意命令：风险档单独标 exec，免得和"写文件"混在一起。
                ToolMeta {
                    tags: &["exec"],
                    risk: Risk::Exec,
                    network: Network::None,
                    sandbox: None,
                    scope: ToolScope::Any,
                },
                run_run_command,
            ),
            builtin(
                "task_status",
                "查看后台任务",
                task_status_schema,
                Permission::WriteWorkspace,
                None,
                None,
                "",
                true,
                run_task_status,
            ),
            // 自省：让模型查"我现在有什么工具"，而不是凭印象猜名字。
            builtin(
                "tools.list",
                "查询可用工具",
                tools_list_schema,
                Permission::ReadOnly,
                None,
                None,
                "",
                true,
                run_tools_list,
            ),
            // 子代理：声明与执行都在 subagent 模块，这里只做登记。
            // `delegable: false` 是扁平编排的保证——子代理开不出孙代理。
            builtin(
                "task",
                "委派给子代理",
                super::subagent::spec,
                Permission::WriteWorkspace,
                None,
                None,
                "",
                false,
                super::subagent::run_as_tool,
            ),
        ];
        // 制品工具（只在 DAG 作用域里出现）与安全研究线的本机工具。
        list.extend(super::artifact_tools::tools());
        list.extend(super::forensics::tools());
        list.extend(super::analysis::tools());
        list.extend(super::dag_tools::tools());
        list
    })
}

#[allow(clippy::too_many_arguments)]
fn builtin(
    name: &'static str,
    blurb: &'static str,
    schema: fn() -> Value,
    permission: Permission,
    action: Option<&'static str>,
    resource_key: Option<&'static str>,
    summary: &'static str,
    delegable: bool,
    run: ToolRun,
) -> Arc<ToolDef> {
    // 老工具零改动：风险等级按权限档推，其它标签留空。
    let meta = ToolMeta {
        risk: match permission {
            Permission::ReadOnly => Risk::Read,
            Permission::WriteWorkspace => Risk::Write,
        },
        ..ToolMeta::default()
    };
    builtin_full(
        name,
        blurb,
        schema,
        permission,
        action,
        resource_key,
        summary,
        delegable,
        meta,
        run,
    )
}

/// 带标签的登记。新工具用它，老工具走上面的 `builtin`。
#[allow(clippy::too_many_arguments)]
pub fn builtin_full(
    name: &'static str,
    blurb: &'static str,
    schema: fn() -> Value,
    permission: Permission,
    action: Option<&'static str>,
    resource_key: Option<&'static str>,
    summary: &'static str,
    delegable: bool,
    meta: ToolMeta,
    run: ToolRun,
) -> Arc<ToolDef> {
    Arc::new(ToolDef {
        name: Cow::Borrowed(name),
        schema: ToolSchema::Build(schema),
        origin: ToolOrigin::Builtin,
        permission,
        action,
        resource_key,
        summary,
        blurb: Cow::Borrowed(blurb),
        delegable,
        enabled_by_default: true,
        meta,
        run: ToolImpl::Fn(run),
    })
}

/// 运行时工具注册表：只装**外部**工具（插件 / MCP），内置工具另有静态表。
///
/// 分开存不是洁癖，是为了让"撤销"有明确边界：撤销外部工具绝不能碰到内置的。
/// 注册表是进程级单例——工具本来就属于整个宿主，不属于某个会话。
pub struct ToolRegistry {
    owned: RwLock<Vec<OwnedTool>>,
}

struct OwnedTool {
    /// 谁注册的（插件 id / MCP server 名）。撤销按它成批删除。
    owner: String,
    def: Arc<ToolDef>,
}

impl ToolRegistry {
    pub fn global() -> &'static ToolRegistry {
        static REGISTRY: OnceLock<ToolRegistry> = OnceLock::new();
        REGISTRY.get_or_init(|| ToolRegistry {
            owned: RwLock::new(Vec::new()),
        })
    }

    /// 登记一个外部工具。重名（含与内置撞名）一律拒绝：模型只认名字，
    /// 允许覆盖就等于允许插件顶掉 `write_file`。
    pub fn register(&self, owner: &str, def: Arc<ToolDef>) -> Result<(), String> {
        let name = def.name.to_string();
        if builtin_tools().iter().any(|builtin| builtin.name == def.name) {
            return Err(format!("工具名 {name} 与内置工具重名，拒绝注册"));
        }
        let mut owned = self.write();
        if let Some(existing) = owned.iter().find(|entry| entry.def.name == def.name) {
            return Err(format!("工具名 {name} 已被 {} 注册", existing.owner));
        }
        owned.push(OwnedTool {
            owner: owner.to_string(),
            def,
        });
        Ok(())
    }

    /// 撤销某个所有者登记的全部工具，返回撤掉的条数。
    ///
    /// 插件关停、MCP 服务器断开时调。撤掉的工具立刻从 schema 里消失，
    /// 模型下一次拿到的清单里就没有它了。
    pub fn release(&self, owner: &str) -> usize {
        let mut owned = self.write();
        let before = owned.len();
        owned.retain(|entry| entry.owner != owner);
        before - owned.len()
    }

    /// 当前登记的外部工具（注册顺序）。
    pub fn dynamic(&self) -> Vec<Arc<ToolDef>> {
        self.read().iter().map(|entry| entry.def.clone()).collect()
    }

    /// 外部工具的所有者（供状态展示）。
    pub fn owner_of(&self, name: &str) -> Option<String> {
        self.read()
            .iter()
            .find(|entry| entry.def.name == name)
            .map(|entry| entry.owner.clone())
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Vec<OwnedTool>> {
        // 中毒的锁里数据仍然是一致的（只是某个读者 panic 过），照常用。
        self.owned
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Vec<OwnedTool>> {
        self.owned
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 按名字取登记项：先内置，后外部。
pub fn find(name: &str) -> Option<Arc<ToolDef>> {
    // 模型回传的是 wire 名（`bin_meta_identify`），代码里写的是内部名
    // （`bin.meta.identify`）。两种都要认，否则工具声明得通、调用却报
    // 「没有这个工具」。
    let wanted = wire_name(name);
    let hit = |tool: &Arc<ToolDef>| tool.name == name || wire_name(&tool.name) == wanted;
    if let Some(def) = builtin_tools().iter().find(|tool| hit(tool)) {
        return Some(def.clone());
    }
    ToolRegistry::global()
        .dynamic()
        .into_iter()
        .find(|tool| hit(tool))
}

/// 全部登记项（内置在前，外部在后）。
fn all_tools() -> Vec<Arc<ToolDef>> {
    let mut tools: Vec<Arc<ToolDef>> = builtin_tools().to_vec();
    tools.extend(ToolRegistry::global().dynamic());
    tools
}

/// 按「开关 + 权限 + 作用域（+ 是否可委派）」筛出此刻真正可用的登记项。
///
/// [`specs`]、[`specs_for`] 和自省工具 `tools.list` 都走这里：过滤逻辑只写
/// 一份，"发给模型的清单"和"模型问到的清单"才不会各说各话。
pub fn visible(
    enabled: &EnabledTools,
    max: Permission,
    dag: bool,
    delegable_only: bool,
) -> Vec<Arc<ToolDef>> {
    all_tools()
        .into_iter()
        .filter(|tool| {
            (!delegable_only || tool.delegable)
                && tool.permission.within(max)
                && enabled.is_on(tool)
                && (dag || tool.meta.scope == ToolScope::Any)
        })
        .collect()
}

/// 发给模型的工具声明（受当前开关与作用域约束）。
///
/// 关掉的工具不进 schema：模型看不到就不会调，也省一份 schema 的 token。
/// `scope = Dag` 的工具在这里一律不发——聊天里没有 DAG 作用域，发了也只会失败。
pub fn specs(enabled: &EnabledTools) -> Vec<Value> {
    // `WriteWorkspace` 是本层最高档（`within` 对它恒为真），等价于"不按权限筛"。
    visible(enabled, Permission::WriteWorkspace, false, false)
        .into_iter()
        .map(|tool| tool.schema_value())
        .collect()
}

/// 按权限等级筛出的声明（子代理 / DAG 节点）：权限档 + 开关 + 作用域三重过滤。
///
/// 取代了原来按工具名硬编码的 `AgentType::tool_names()`：只读子代理天然
/// 拿不到写类工具，而新增的只读工具会自动进入探索子代理的能力集。
///
/// `dag = true` 表示有 DAG 作用域，制品类工具才进清单。
pub fn specs_for(enabled: &EnabledTools, max: Permission, dag: bool) -> Vec<Value> {
    visible(enabled, max, dag, true)
        .into_iter()
        .map(|tool| tool.schema_value())
        .collect()
}

/// 解析入参并执行一次工具调用。
pub fn execute(ctx: &ToolContext<'_>, name: &str, arguments: &str) -> ToolResult {
    let args: Value = if arguments.trim().is_empty() {
        json!({})
    } else {
        match serde_json::from_str(arguments) {
            Ok(value) => value,
            Err(error) => return ToolResult::fail(format!("入参不是合法 JSON：{error}")),
        }
    };
    run_with_args(ctx, name, &args)
}

/// 已解析好入参的执行入口（子代理内部复用，避免二次解析）。
pub fn run_with_args(ctx: &ToolContext<'_>, name: &str, args: &Value) -> ToolResult {
    let Some(tool) = find(name) else {
        return ToolResult::fail(format!("没有这个工具：{name}"));
    };
    // 开关的第二道防线：schema 里已经去掉了，但模型可能从历史里翻出旧调用，
    // 或者干脆幻觉一个被禁的工具名——不能指望它"看不见就不调"。
    if !ctx.enabled.is_on(&tool) {
        return ToolResult::fail(format!("工具 {name} 已被关闭，如需使用请在设置里打开"));
    }
    // 作用域的第三道防线：schema 里没发，但历史里可能留着旧调用。
    if tool.meta.scope == ToolScope::Dag && ctx.dag.is_none() {
        return ToolResult::fail(format!("{name} 只在 DAG 节点里可用（需要节点作用域）"));
    }
    tool.call(ctx, args)
}

// ---------------------------------------------------------------- 执行体适配
//
// 各工具的真实实现都收 (root, args) 这组参数；这里把它们套进注册表要求的
// 统一签名。每一层都薄得只有一次转接，但正是它让"一张表装下所有工具"成立
// ——包括原来需要特判的 `task`。

fn run_read_file(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    ToolResult::plain(read_file(Path::new(ctx.directory), args))
}

fn run_list_files(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    ToolResult::plain(list_files(Path::new(ctx.directory), args))
}

fn run_grep(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    ToolResult::plain(grep(Path::new(ctx.directory), args))
}

fn run_write_file(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    ToolResult::plain(write_file(Path::new(ctx.directory), args))
}

fn run_edit_file(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    ToolResult::plain(edit_file(Path::new(ctx.directory), args))
}

fn run_run_command(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let runtime = ctx.app.map(|app| ToolRuntime {
        app,
        session_id: ctx.session_id,
    });
    ToolResult::plain(run_command(
        Path::new(ctx.directory),
        args,
        ctx.should_stop,
        runtime.as_ref(),
    ))
}

fn run_task_status(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let runtime = ctx.app.map(|app| ToolRuntime {
        app,
        session_id: ctx.session_id,
    });
    ToolResult::plain(task_status(args, runtime.as_ref()))
}

fn run_tools_list(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    ToolResult::plain(tools_list(ctx, args))
}

/// 自省：这一轮模型究竟能调用什么。
///
/// 与 [`visible`] 同源，所以"自报的清单"和"真发出去的 schema"不会漂——这
/// 正是它存在的意义：模型不该靠猜自己有没有某个工具。
fn tools_list(ctx: &ToolContext<'_>, args: &Value) -> ToolOutcome {
    let tag = args
        .get("tag")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let include_blocked = args
        .get("include_blocked")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    // 上下文里存的是审批档字符串（read-only / ask / full）。映射回工具权限上限，
    // 与 DAG runner 的 cap 保持一致：只有 read-only 会挡住写类工具。
    let cap = match ctx.permission {
        Some("read-only") => Permission::ReadOnly,
        _ => Permission::WriteWorkspace,
    };
    let dag = ctx.dag.is_some();

    let mut lines: Vec<String> = Vec::new();
    let mut blocked: Vec<String> = Vec::new();
    for tool in all_tools() {
        if let Some(tag) = tag {
            if !tool
                .meta
                .tags
                .iter()
                .any(|item| item.eq_ignore_ascii_case(tag))
            {
                continue;
            }
        }
        let reason = if !ctx.enabled.is_on(&tool) {
            Some("已在设置里关闭".to_string())
        } else if tool.meta.scope == ToolScope::Dag && !dag {
            Some("只在 DAG 节点里可用".to_string())
        } else if !tool.permission.within(cap) {
            Some(format!(
                "需要 {} 档权限（当前 {}）",
                tool.permission.as_str(),
                cap.as_str()
            ))
        } else {
            None
        };
        match reason {
            Some(reason) => blocked.push(format!("- {}：{reason}", tool.wire_name())),
            None => lines.push(describe_tool(&tool)),
        }
    }

    let filter = tag.map(|tag| format!("，标签={tag}")).unwrap_or_default();
    let mut text = if lines.is_empty() {
        format!(
            "当前没有可用工具（权限档 {}，作用域 {}{filter}）。",
            cap.as_str(),
            if dag { "DAG 节点" } else { "普通会话" }
        )
    } else {
        format!(
            "当前可用 {} 个工具（权限档 {}，作用域 {}{filter}）：\n{}",
            lines.len(),
            cap.as_str(),
            if dag { "DAG 节点" } else { "普通会话" },
            lines.join("\n")
        )
    };
    if include_blocked && !blocked.is_empty() {
        text.push_str(&format!(
            "\n\n被挡住 {} 个（这轮不在工具清单里，硬调会失败）：\n{}",
            blocked.len(),
            blocked.join("\n")
        ));
    }
    ToolOutcome::ok(text)
}

/// 一条工具的自我说明：模型侧名字、干什么、要什么参数、有什么门槛。
fn describe_tool(tool: &ToolDef) -> String {
    let wire = tool.wire_name();
    let mut line = format!("- {wire}");
    if wire != tool.name.as_ref() {
        line.push_str(&format!("（内部名 {}）", tool.name));
    }
    if !tool.blurb.is_empty() {
        line.push_str(&format!("：{}", tool.blurb));
    }
    let params = params_of(tool);
    if !params.is_empty() {
        line.push_str(&format!("｜参数：{params}（* = 必填）"));
    }
    let mut notes: Vec<String> = Vec::new();
    if tool.permission != Permission::ReadOnly {
        notes.push(format!("需 {} 档", tool.permission.as_str()));
    }
    if !tool.meta.tags.is_empty() {
        notes.push(tool.meta.tags.join("/"));
    }
    if tool.meta.scope == ToolScope::Dag {
        notes.push("仅 DAG".to_string());
    }
    if !tool.delegable {
        notes.push("子代理不可用".to_string());
    }
    if !notes.is_empty() {
        line.push_str(&format!("〔{}〕", notes.join("，")));
    }
    line
}

/// 参数名摘要（必填加 `*`）。只列名字不列 JSON Schema：模型要的是"能不能
/// 喂得上"，完整 schema 它本来就有。
fn params_of(tool: &ToolDef) -> String {
    let schema = tool.schema.render();
    let Some(params) = schema.pointer("/function/parameters") else {
        return String::new();
    };
    let Some(props) = params.get("properties").and_then(Value::as_object) else {
        return String::new();
    };
    let required: Vec<&str> = params
        .get("required")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    props
        .keys()
        .map(|key| {
            if required.contains(&key.as_str()) {
                format!("{key}*")
            } else {
                key.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// 工具执行时的运行时上下文：后台任务要 AppHandle 才能进注册表 / 发事件。
pub struct ToolRuntime<'a> {
    pub app: &'a tauri::AppHandle,
    /// 任务归属：任务坞里标「本会话」，task_status 只列这个会话的。
    pub session_id: &'a str,
}

/// 无运行时上下文执行（单测走这条）：后台任务不可用。
///
/// 保留这个薄壳是因为工具实现的纯逻辑（读文件、搜内容、路径越界拦截）
/// 本就不需要 `AppHandle`，单测要能直接跑。
/// 带一个「该停手了吗」的探针：跑命令时每轮都问一次，命中就连同命令拉起的
/// 子进程一起收掉（停止按钮不该等命令自己跑完）。
#[cfg_attr(not(test), allow(dead_code))]
pub fn execute_with_stop(
    directory: &str,
    name: &str,
    arguments: &str,
    should_stop: &dyn Fn() -> bool,
) -> ToolOutcome {
    let enabled = EnabledTools::all();
    let ctx = ToolContext {
        app: None,
        session_id: "",
        directory,
        call_id: "",
        provider: None,
        model: None,
        permission: None,
        enabled: &enabled,
        dag: None,
        should_stop,
    };
    execute(&ctx, name, arguments).outcome
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

fn int_arg(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(Value::as_u64)
}

fn bool_arg(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// 归一化路径并确认没跑出工作目录。
fn resolve(root: &Path, raw: &str) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(root.to_path_buf());
    }
    let candidate = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        root.join(raw)
    };
    // 词法归一：处理 `..` / `.`，不做 IO（文件可能还不存在）。
    let mut normalized = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    let root_abs = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let base = normalized
        .ancestors()
        .find(|path| path.exists())
        .and_then(|path| path.canonicalize().ok());
    let inside = match base {
        Some(base) => base.starts_with(&root_abs) || base == root_abs,
        None => normalized.starts_with(&root_abs),
    };
    if !inside {
        return Err(format!("路径越出工作目录：{raw}"));
    }
    Ok(normalized)
}

/// 把相对路径解析到工作目录内（给兄弟模块用：制品、逆向工具都要读样本）。
pub fn resolve_path(root: &Path, raw: &str) -> Result<PathBuf, String> {
    resolve(root, raw)
}

fn read_file(root: &Path, args: &Value) -> ToolOutcome {    let Some(path) = str_arg(args, "path") else {
        return ToolOutcome::fail("缺少 path");
    };
    let target = match resolve(root, path) {
        Ok(target) => target,
        Err(error) => return ToolOutcome::fail(error),
    };
    let Ok(text) = std::fs::read_to_string(&target) else {
        return ToolOutcome::fail(format!("读不到 {}", target.display()));
    };
    let offset = int_arg(args, "offset").unwrap_or(1).max(1) as usize;
    let limit = int_arg(args, "limit")
        .unwrap_or(READ_LINES as u64)
        .clamp(1, 5000) as usize;
    let mut out = String::new();
    for (index, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
        out.push_str(&format!("{:>5}→{}\n", index + 1, line));
    }
    if out.is_empty() {
        return ToolOutcome::ok("（该范围没有内容）");
    }
    ToolOutcome::ok(clamp(&out))
}

fn list_files(root: &Path, args: &Value) -> ToolOutcome {
    let start = match str_arg(args, "path").map(|path| resolve(root, path)) {
        Some(Ok(path)) => path,
        Some(Err(error)) => return ToolOutcome::fail(error),
        None => root.to_path_buf(),
    };
    let pattern = str_arg(args, "pattern").map(str::to_string);
    let mut files = Vec::new();
    let cut = walk(&start, 0, &mut files);
    let note = cut
        .note()
        .map(|note| format!("{note}\n"))
        .unwrap_or_default();
    let mut out = String::new();
    let mut shown = 0;
    for file in files {
        let text = file.display().to_string();
        if let Some(pattern) = &pattern {
            let name = file
                .file_name()
                .map(|v| v.to_string_lossy().to_string())
                .unwrap_or_default();
            if !wildcard_match(pattern, &name) {
                continue;
            }
        }
        if shown >= LIST_ENTRIES {
            out.push_str("…（还有更多，已截断）\n");
            break;
        }
        out.push_str(&format!("{text}\n"));
        shown += 1;
    }
    if out.is_empty() {
        return ToolOutcome::ok(format!("{note}（没有匹配的文件）"));
    }
    ToolOutcome::ok(clamp(&format!("{note}{out}")))
}

fn grep(root: &Path, args: &Value) -> ToolOutcome {
    let Some(pattern) = str_arg(args, "pattern") else {
        return ToolOutcome::fail("缺少 pattern");
    };
    let start = match str_arg(args, "path").map(|path| resolve(root, path)) {
        Some(Ok(path)) => path,
        Some(Err(error)) => return ToolOutcome::fail(error),
        None => root.to_path_buf(),
    };
    let ignore_case = bool_arg(args, "ignore_case");
    let needle = if ignore_case {
        pattern.to_lowercase()
    } else {
        pattern.to_string()
    };
    let mut files = Vec::new();
    let cut = walk(&start, 0, &mut files);
    // 截断说明放在最前面：clamp 只保头，放后面会被截掉。
    let note = cut
        .note()
        .map(|note| format!("{note}\n"))
        .unwrap_or_default();
    let mut out = String::new();
    let mut hits = 0;
    for file in files {
        let Ok(meta) = std::fs::metadata(&file) else {
            continue;
        };
        if meta.len() > GREP_MAX_BYTES {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        if text.contains('\u{0}') {
            continue;
        }
        for (index, line) in text.lines().enumerate() {
            let haystack = if ignore_case {
                line.to_lowercase()
            } else {
                line.to_string()
            };
            if !haystack.contains(&needle) {
                continue;
            }
            out.push_str(&format!(
                "{}:{}:{}\n",
                file.display(),
                index + 1,
                line.trim_end()
            ));
            hits += 1;
            if hits >= GREP_HITS {
                out.push_str("…（命中过多，已截断）\n");
                return ToolOutcome::ok(clamp(&format!("{note}{out}")));
            }
        }
    }
    if out.is_empty() {
        // 扫不全的时候「没有命中」是错误结论：必须把话说明白。
        return ToolOutcome::ok(format!("{note}（没有命中）"));
    }
    ToolOutcome::ok(clamp(&format!("{note}{out}")))
}

fn write_file(root: &Path, args: &Value) -> ToolOutcome {
    let Some(path) = str_arg(args, "path") else {
        return ToolOutcome::fail("缺少 path");
    };
    let Some(content) = args.get("content").and_then(Value::as_str) else {
        return ToolOutcome::fail("缺少 content");
    };
    let target = match resolve(root, path) {
        Ok(target) => target,
        Err(error) => return ToolOutcome::fail(error),
    };
    if let Some(parent) = target.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            return ToolOutcome::fail(format!("创建目录失败：{error}"));
        }
    }
    match std::fs::write(&target, content) {
        Ok(()) => ToolOutcome::ok(format!(
            "已写入 {}（{} 行）",
            target.display(),
            content.lines().count()
        )),
        Err(error) => ToolOutcome::fail(format!("写入失败：{error}")),
    }
}

/// 文件与 old_string 的行尾对不上时，给出「照文件行尾换一遍」的版本。
///
/// 只处理两种明确的错配：文件 CRLF 而 old_string 只有 \n（read_file 之后
/// 照着抄的常态），以及反过来文件 LF 而 old_string 带着 \r。换过之后 new_string
/// 也一起换，免得改完一段就混出行尾来。
fn line_ending_variant(text: &str, old: &str, new: &str) -> Option<(String, String)> {
    let file_crlf = text.contains("\r\n");
    if file_crlf && old.contains('\n') && !old.contains('\r') {
        return Some((to_crlf(old), to_crlf(new)));
    }
    if !file_crlf && old.contains("\r\n") {
        return Some((old.replace("\r\n", "\n"), new.replace("\r\n", "\n")));
    }
    None
}

fn to_crlf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

fn edit_file(root: &Path, args: &Value) -> ToolOutcome {
    let Some(path) = str_arg(args, "path") else {
        return ToolOutcome::fail("缺少 path");
    };
    let Some(old) = args.get("old_string").and_then(Value::as_str) else {
        return ToolOutcome::fail("缺少 old_string");
    };
    let Some(new) = args.get("new_string").and_then(Value::as_str) else {
        return ToolOutcome::fail("缺少 new_string");
    };
    if old.is_empty() {
        return ToolOutcome::fail("old_string 不能为空（整体重写请用 write_file）");
    }
    let target = match resolve(root, path) {
        Ok(target) => target,
        Err(error) => return ToolOutcome::fail(error),
    };
    let Ok(text) = std::fs::read_to_string(&target) else {
        return ToolOutcome::fail(format!("读不到 {}", target.display()));
    };
    // read_file 只给行内容（看不到 \r），模型照着抄回来的 old_string 在 CRLF
    // 文件里就永远匹配不上——按文件行尾换一遍再试。
    let variant = line_ending_variant(&text, old, new);
    let (old, new) = match &variant {
        Some((variant_old, variant_new)) => (variant_old.as_str(), variant_new.as_str()),
        None => (old, new),
    };
    let count = text.matches(old).count();
    if count == 0 {
        // 行尾对不上是常见坑，失败时把话说明白：别让模型反复试错。
        let hint = if variant.is_some() {
            "；这个文件是 CRLF 行尾（read_file 里看不到 \\r），已按 \\r\\n 试过一次仍不匹配"
        } else {
            ""
        };
        return ToolOutcome::fail(format!(
            "没有找到与 old_string 完全一致的片段{hint}：请带上完整缩进与上下文行再试"
        ));
    }
    let replace_all = bool_arg(args, "replace_all");
    if count > 1 && !replace_all {
        return ToolOutcome::fail(format!(
            "old_string 出现了 {count} 次，请补足上下文让它唯一，或传 replace_all=true"
        ));
    }
    let updated = if replace_all {
        text.replace(old, new)
    } else {
        text.replacen(old, new, 1)
    };
    match std::fs::write(&target, &updated) {
        Ok(()) => ToolOutcome::ok(format!(
            "已替换 {} 处：{}",
            if replace_all { count } else { 1 },
            target.display()
        )),
        Err(error) => ToolOutcome::fail(format!("写入失败：{error}")),
    }
}

fn run_command(
    root: &Path,
    args: &Value,
    should_stop: &dyn Fn() -> bool,
    runtime: Option<&ToolRuntime<'_>>,
) -> ToolOutcome {
    let Some(command) = str_arg(args, "command") else {
        return ToolOutcome::fail("缺少 command");
    };
    // 后台模式：进 TasksState（和用户在终端页起的任务是同一条通道），立即返回。
    // 长命令不再占住这一轮，也不需要 120s 超时；进度由 task_status 查。
    if bool_arg(args, "background") {
        return spawn_background_task(runtime, root, command);
    }
    let timeout = Duration::from_millis(
        int_arg(args, "timeout_ms")
            .unwrap_or(120_000)
            .clamp(1_000, 600_000),
    );
    let mut process = if cfg!(windows) {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(command);
        cmd
    } else {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd
    };
    process
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // 子进程自己领一个进程组：`sh -c` 拉起的孙子进程（cargo / rustc / npm …）
    // 只杀 sh 是带不走的，成组才能一起收掉。
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        process.process_group(0);
    }
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => return ToolOutcome::fail(format!("启动命令失败：{error}")),
    };
    // 两路管道都要有人同时读：写满一管（macOS 64KB）之后子进程会永远卡在 write 上，
    // 于是「命令话多」被误判成「命令超时」——cargo build 就是常客。
    let stdout_reader = child.stdout.take().map(spawn_pipe_reader);
    let stderr_reader = child.stderr.take().map(spawn_pipe_reader);

    enum Ending {
        Done,
        Stopped,
        TimedOut,
    }
    let started = Instant::now();
    let ending = loop {
        match child.try_wait() {
            Ok(Some(_)) => break Ending::Done,
            Ok(None) => {}
            Err(error) => {
                kill_tree(&mut child);
                return ToolOutcome::fail(format!("等待命令失败：{error}"));
            }
        }
        if should_stop() {
            kill_tree(&mut child);
            break Ending::Stopped;
        }
        if started.elapsed() > timeout {
            kill_tree(&mut child);
            break Ending::TimedOut;
        }
        std::thread::sleep(RUN_POLL);
    };
    // 杀过也要 wait：收掉僵尸，也让两个读线程读干净后退出。
    let status = child.wait().ok();
    let stdout = stdout_reader.map(join_pipe_reader).unwrap_or_default();
    let stderr = stderr_reader.map(join_pipe_reader).unwrap_or_default();

    let code = status.and_then(|value| value.code()).unwrap_or(-1);
    let mut text = match ending {
        Ending::Done => format!("exit={code}\n"),
        Ending::Stopped => format!("已被用户停止，命令已终止：{command}\nexit={code}\n"),
        Ending::TimedOut => format!(
            "命令超时（{} ms）已终止：{command}\nexit={code}\n",
            timeout.as_millis()
        ),
    };
    let stdout = String::from_utf8_lossy(&stdout);
    let stderr = String::from_utf8_lossy(&stderr);
    if !stdout.trim().is_empty() {
        text.push_str("--- stdout ---\n");
        text.push_str(stdout.trim_end());
        text.push('\n');
    }
    if !stderr.trim().is_empty() {
        text.push_str("--- stderr ---\n");
        text.push_str(stderr.trim_end());
        text.push('\n');
    }
    let text = clamp(&text);
    let ok = matches!(ending, Ending::Done) && status.map(|value| value.success()).unwrap_or(false);
    if ok {
        ToolOutcome::ok(text)
    } else {
        ToolOutcome::fail(text)
    }
}

/// `run_command background=true`：起一个 agent 任务，立刻把 id 交还给模型。
fn spawn_background_task(
    runtime: Option<&ToolRuntime<'_>>,
    root: &Path,
    command: &str,
) -> ToolOutcome {
    let Some(runtime) = runtime else {
        return ToolOutcome::fail("当前上下文不支持后台任务（background 只能在本机会话里用）");
    };
    let Some(tasks) = runtime.app.try_state::<TasksState>() else {
        return ToolOutcome::fail(
            "后台任务插件未启用：background 用不了。改成前台执行，或让用户先启用 tasks 插件。",
        );
    };
    match tasks.spawn(
        runtime.app.clone(),
        SpawnRequest {
            cmd: command.to_string(),
            cwd: root.to_string_lossy().into_owned(),
            session_id: Some(runtime.session_id.to_string()),
            title: None,
            background: true,
            origin: "agent".to_string(),
        },
    ) {
        Ok(info) => ToolOutcome::ok(format!(
            "已在后台启动：{command}\n任务 id：{id}\n输出会实时进「后台任务」坞 / 文件侧栏的「终端」页；用 task_status（id: {id}）看进度，stop=true 停止。",
            id = info.id
        )),
        Err(error) => ToolOutcome::fail(format!("启动后台任务失败：{error}")),
    }
}

/// `task_status`：列本会话的任务 / 看某条尾部输出 / 停止。
fn task_status(args: &Value, runtime: Option<&ToolRuntime<'_>>) -> ToolOutcome {
    let Some(runtime) = runtime else {
        return ToolOutcome::fail("当前上下文不能查后台任务");
    };
    let Some(tasks) = runtime.app.try_state::<TasksState>() else {
        return ToolOutcome::fail("后台任务插件未启用，查不到任何任务");
    };
    // 只认这个会话起的任务：别的会话的命令不该从工具里漏出来。
    let mine = |task: &TaskInfo| task.session_id.as_deref() == Some(runtime.session_id);

    if bool_arg(args, "stop") {
        let Some(id) = str_arg(args, "id") else {
            return ToolOutcome::fail("stop=true 需要同时给 id");
        };
        return match tasks.stop(id) {
            Ok(()) => ToolOutcome::ok(format!("已停止任务 {id}")),
            Err(error) => ToolOutcome::fail(error),
        };
    }

    if let Some(id) = str_arg(args, "id") {
        let Some(info) = tasks
            .list()
            .into_iter()
            .find(|task| task.id == id && mine(task))
        else {
            return ToolOutcome::fail(format!("找不到任务 {id}（只能看本会话起的任务）"));
        };
        let lines = int_arg(args, "lines").unwrap_or(120).clamp(1, 400) as usize;
        let start = info.tail.len().saturating_sub(lines);
        let body = info.tail[start..].join("\n");
        let body = if body.trim().is_empty() {
            "（还没有输出）".to_string()
        } else {
            body
        };
        return ToolOutcome::ok(clamp(&format!(
            "任务 {}\n命令：{}\n状态：{}{}\n目录：{}\n--- 最近 {lines} 行输出 ---\n{body}",
            info.id,
            info.cmd,
            state_label(&info.state),
            exit_suffix(&info),
            info.cwd
        )));
    }

    let list: Vec<TaskInfo> = tasks.list().into_iter().filter(mine).collect();
    if list.is_empty() {
        return ToolOutcome::ok("这个会话还没有后台任务。");
    }
    let mut text = String::from("本会话的后台任务：\n");
    for task in list {
        text.push_str(&format!(
            "- {} · {} · {}{}\n  命令：{}\n",
            task.id,
            state_label(&task.state),
            elapsed_label(&task),
            exit_suffix(&task),
            task.cmd
        ));
    }
    ToolOutcome::ok(clamp(&text))
}

fn state_label(state: &str) -> &'static str {
    match state {
        "running" => "运行中",
        "done" => "已完成",
        "failed" => "失败",
        "stopped" => "已停止",
        _ => "未知",
    }
}

fn elapsed_label(task: &TaskInfo) -> String {
    let end = task.ended_at.unwrap_or_else(now_ms);
    let seconds = ((end - task.started_at).max(0) / 1000) as u64;
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

fn exit_suffix(task: &TaskInfo) -> String {
    match task.state.as_str() {
        "running" => String::new(),
        "stopped" => "（已停止）".to_string(),
        _ => match task.exit_code {
            Some(code) => format!("（exit {code}）"),
            None => String::new(),
        },
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
}

/// 一路管道交给一个线程读到结束：读的人不读，子进程就会卡在 write 上。
fn spawn_pipe_reader(pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || drain_pipe(pipe))
}

fn join_pipe_reader(handle: std::thread::JoinHandle<Vec<u8>>) -> Vec<u8> {
    handle.join().unwrap_or_default()
}

/// 读到结束；最多留 PIPE_KEEP_BYTES——展示时本来就要截断，多余的只是占内存。
fn drain_pipe(mut pipe: impl Read) -> Vec<u8> {
    let mut kept: Vec<u8> = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                let room = PIPE_KEEP_BYTES.saturating_sub(kept.len());
                if room == 0 {
                    continue;
                }
                let take = count.min(room);
                kept.extend_from_slice(&buffer[..take]);
            }
            Err(_) => break,
        }
    }
    kept
}

/// 收掉整组子进程：先 SIGTERM 给个收尾机会，赖着不走再 SIGKILL。
fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        // spawn 时设了 process_group(0)：组号＝子进程 pid，负数表示「整组」。
        let group = child.id() as libc::pid_t;
        unsafe {
            libc::kill(-group, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            // 已经退出就不再发信号：僵尸被收掉后 pid 可能被系统复用。
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

/// 递归收集文件（有深度与跳过规则，不跟符号链接）。
/// 返回值说明这次遍历有没有被上限截断——截断过就必须在结论里讲清楚。
fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) -> WalkCut {
    let mut cut = WalkCut::default();
    if depth > WALK_MAX_DEPTH {
        cut.depth = true;
        return cut;
    }
    if out.len() >= WALK_MAX_FILES {
        cut.files = true;
        return cut;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return cut;
    };
    let mut items: Vec<_> = entries.filter_map(Result::ok).collect();
    items.sort_by_key(|entry| entry.file_name());
    for entry in items {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            let child = walk(&path, depth + 1, out);
            cut.files |= child.files;
            cut.depth |= child.depth;
        } else if file_type.is_file() {
            if out.len() >= WALK_MAX_FILES {
                cut.files = true;
                break;
            }
            out.push(path);
        }
    }
    cut
}

/// 极简通配：只支持 `*` 与 `?`。
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while ti < text.len() {
        if pi < pattern.len() && (pattern[pi] == '?' || pattern[pi] == text[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pattern.len() && pattern[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(star_index) = star {
            pi = star_index + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == '*' {
        pi += 1;
    }
    pi == pattern.len()
}

fn clamp(text: &str) -> String {
    if text.chars().count() <= MAX_OUTPUT_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX_OUTPUT_CHARS).collect();
    format!("{head}\n…（输出过长，已截断）")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// 大多数用例不关心停止：固定传 false。
    fn execute(root: &str, name: &str, arguments: &str) -> ToolOutcome {
        execute_with_stop(root, name, arguments, &|| false)
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("prism-native-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("建临时目录");
        path
    }

    #[test]
    fn specs_cover_builtin_tools() {
        // 注册表现在连 `task`（子代理）一起给出——原来它是 agent 层单独 push 的，
        // 模型看到的清单没变，但"事实来源"只有一个。
        //
        // 只比前 9 项：内置工具永远排在前面，后面可能跟着插件在别的用例里
        // 注册进来的外部工具（注册表是进程级的，测试并行跑）。
        let names: Vec<String> = specs(&EnabledTools::all())
            .iter()
            .filter_map(|spec| spec["function"]["name"].as_str().map(str::to_string))
            .take(9)
            .collect();
        assert_eq!(
            names,
            vec![
                "read_file",
                "list_files",
                "grep",
                "write_file",
                "edit_file",
                "run_command",
                "task_status",
                "tools_list",
                "task"
            ]
        );
    }

    /// 自省工具要说真话：报出来的清单必须和 `specs` 的口径一致。
    fn list_with(permission: Option<&str>, arguments: &str) -> String {
        let enabled = EnabledTools::all();
        let never = || false;
        let ctx = ToolContext {
            app: None,
            session_id: "",
            directory: "/tmp",
            call_id: "",
            provider: None,
            model: None,
            permission,
            enabled: &enabled,
            dag: None,
            should_stop: &never,
        };
        let outcome = super::execute(&ctx, "tools.list", arguments).outcome;
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        outcome.text
    }

    #[test]
    fn tools_list_reports_only_what_can_run_now() {
        let text = list_with(Some("workspace-write"), "{}");
        assert!(text.contains("当前可用"), "{text}");
        assert!(text.contains("- read_file"), "只读工具应当在清单里：{text}");
        assert!(text.contains("- tools_list"), "自省工具自己也要能被看到：{text}");
        // 非 DAG 会话：制品工具没作用域，必须明确报"用不了"而不是假装能调。
        assert!(text.contains("只在 DAG 节点里可用"), "{text}");
    }

    #[test]
    fn tools_list_hides_write_tools_in_read_only() {
        let text = list_with(Some("read-only"), "{}");
        assert!(
            text.contains("被挡住") && text.contains("write_file：需要 workspace-write 档权限"),
            "{text}"
        );
    }

    #[test]
    fn tools_list_filters_by_tag() {
        let text = list_with(Some("workspace-write"), r#"{"tag":"exec"}"#);
        assert!(text.contains("- run_command"), "{text}");
        assert!(!text.contains("- read_file"), "tag 过滤不该漏出无关工具：{text}");
    }

    /// provider 的函数名只接受 `[A-Za-z0-9_-]`。
    ///
    /// 带点号的名字会让整个请求 400——工具一个都发不出去，模型连一句
    /// "我没有这个工具"都说不了。逆向任务就是在这里挂掉的。
    #[test]
    fn every_tool_name_is_accepted_by_providers() {
        let mut seen = std::collections::HashSet::new();
        for tool in builtin_tools() {
            let wire = tool.wire_name();
            assert!(!wire.is_empty() && wire.len() <= 64, "{} 的 wire 名超长", tool.name);
            assert!(
                wire
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'),
                "{} → {wire} 含非法字符",
                tool.name
            );
            assert!(seen.insert(wire.clone()), "wire 名重复：{wire}");
            assert_eq!(
                tool.schema_value()["function"]["name"],
                json!(wire),
                "{} 的 schema 名没跟着换",
                tool.name
            );
        }
    }

    /// 模型回传的是 wire 名，执行层得认回来，否则"声明得通、调用不存在"。
    #[test]
    fn wire_names_resolve_back_to_dotted_tools() {
        let tool = find("bin.meta.identify").expect("内置工具应该存在");
        assert_eq!(tool.wire_name(), "bin_meta_identify");
        assert_eq!(
            find("bin_meta_identify").map(|hit| hit.name.to_string()),
            Some("bin.meta.identify".to_string())
        );
        assert!(find("bin_meta_identify_extra").is_none(), "不该模糊匹配");
    }

    /// 关掉的工具要从 schema 里消失（省 token，也不诱导模型调用）。
    #[test]
    fn disabled_tool_disappears_from_specs_and_is_rejected() {
        let mut off = std::collections::HashMap::new();
        off.insert("run_command".to_string(), false);
        let enabled = EnabledTools::from_map(off);

        let all = specs(&enabled);
        let names: Vec<&str> = all
            .iter()
            .filter_map(|spec| spec["function"]["name"].as_str())
            .collect();
        assert!(
            !names.contains(&"run_command"),
            "关掉的工具不该进 schema：{names:?}"
        );
        assert!(names.contains(&"read_file"), "没动的内置工具应当保持开启");

        // 执行侧兜底：模型硬调被禁工具也要被拦下。
        // 用 `super::` 是因为测试模块里有同名的本地辅助函数。
        let ctx = ToolContext {
            app: None,
            session_id: "",
            directory: ".",
            call_id: "",
            provider: None,
            model: None,
            permission: None,
            enabled: &enabled,
            dag: None,
            should_stop: &|| false,
        };
        let result = super::execute(&ctx, "run_command", r#"{"command":"echo hi"}"#);
        assert!(result.outcome.error.is_some(), "被禁工具不该真的执行");
        assert!(
            result.outcome.text.contains("已被关闭"),
            "{}",
            result.outcome.text
        );
    }

    #[test]
    fn read_write_round_trip_has_line_numbers() {
        let dir = temp_dir("rw");
        let root = dir.to_string_lossy().to_string();
        write_file(
            &dir,
            &json!({ "path": "a.txt", "content": "第一行\n第二行\n" }),
        );
        let outcome = execute(&root, "read_file", r#"{"path":"a.txt"}"#);
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        assert!(outcome.text.contains("1→第一行"), "{}", outcome.text);
        assert!(outcome.text.contains("2→第二行"), "{}", outcome.text);
    }

    #[test]
    fn edit_requires_unique_match() {
        let dir = temp_dir("edit");
        let root = dir.to_string_lossy().to_string();
        execute(
            &root,
            "write_file",
            r#"{"path":"b.txt","content":"x\nx\n"}"#,
        );
        let outcome = execute(
            &root,
            "edit_file",
            r#"{"path":"b.txt","old_string":"x","new_string":"y"}"#,
        );
        assert!(outcome.error.is_some());
        assert!(outcome.text.contains("2 次"), "{}", outcome.text);

        let outcome = execute(
            &root,
            "edit_file",
            r#"{"path":"b.txt","old_string":"x","new_string":"y","replace_all":true}"#,
        );
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "y\ny\n");
    }

    /// CRLF 文件：read_file 里看不到 \r，模型照抄回来的 old_string 也必须能改。
    #[test]
    fn edit_file_accepts_lf_old_string_in_crlf_file() {
        let dir = temp_dir("crlf");
        let root = dir.to_string_lossy().to_string();
        write_file(
            &dir,
            &json!({ "path": "c.txt", "content": "alpha\r\nbeta\r\ngamma\r\n" }),
        );
        // 读出来是这个样子：看不到 \r，模型也只能照着这样子抄。
        let read = execute(&root, "read_file", r#"{"path":"c.txt"}"#);
        assert!(read.text.contains("1→alpha"), "{}", read.text);

        let outcome = execute(
            &root,
            "edit_file",
            r#"{"path":"c.txt","old_string":"alpha\nbeta","new_string":"one\ntwo"}"#,
        );
        assert!(outcome.error.is_none(), "{}", outcome.text);
        // 改完还是 CRLF：不能改一段就混出行尾来。
        assert_eq!(
            fs::read_to_string(dir.join("c.txt")).unwrap(),
            "one\r\ntwo\r\ngamma\r\n"
        );
    }

    /// 反过来也要认：文件是 LF，old_string 却带着 \r。
    #[test]
    fn edit_file_accepts_crlf_old_string_in_lf_file() {
        let dir = temp_dir("lf");
        let root = dir.to_string_lossy().to_string();
        write_file(
            &dir,
            &json!({ "path": "d.txt", "content": "alpha\nbeta\n" }),
        );
        let outcome = execute(
            &root,
            "edit_file",
            r#"{"path":"d.txt","old_string":"alpha\r\nbeta","new_string":"one\r\ntwo"}"#,
        );
        assert!(outcome.error.is_none(), "{}", outcome.text);
        assert_eq!(fs::read_to_string(dir.join("d.txt")).unwrap(), "one\ntwo\n");
    }

    /// 真的找不到时要把行尾这件事说出来，别让模型反复试错。
    #[test]
    fn edit_file_failure_mentions_crlf() {
        let dir = temp_dir("crlfmiss");
        let root = dir.to_string_lossy().to_string();
        write_file(
            &dir,
            &json!({ "path": "e.txt", "content": "alpha\r\nbeta\r\n" }),
        );
        let outcome = execute(
            &root,
            "edit_file",
            r#"{"path":"e.txt","old_string":"alpha\ngamma","new_string":"x"}"#,
        );
        assert!(outcome.error.is_some());
        assert!(outcome.text.contains("CRLF"), "{}", outcome.text);
    }

    #[test]
    fn paths_outside_workspace_are_rejected() {
        let dir = temp_dir("guard");
        let root = dir.to_string_lossy().to_string();
        let outcome = execute(&root, "read_file", r#"{"path":"../../etc/hosts"}"#);
        assert!(outcome.error.is_some());
        assert!(outcome.text.contains("越出"), "{}", outcome.text);
    }

    fn sample_task(state: &str, exit_code: Option<i32>) -> TaskInfo {
        TaskInfo {
            id: "t1".to_string(),
            cmd: "pnpm build".to_string(),
            cwd: "/tmp".to_string(),
            state: state.to_string(),
            started_at: 1_000,
            ended_at: if state == "running" {
                None
            } else {
                Some(73_000)
            },
            exit_code,
            session_id: Some("s1".to_string()),
            title: None,
            background: true,
            origin: "agent".to_string(),
            tail: vec![],
        }
    }

    #[test]
    fn task_status_labels_are_readable() {
        assert_eq!(state_label("running"), "运行中");
        assert_eq!(state_label("done"), "已完成");
        assert_eq!(state_label("failed"), "失败");
        assert_eq!(state_label("stopped"), "已停止");

        assert_eq!(exit_suffix(&sample_task("running", None)), "");
        assert_eq!(exit_suffix(&sample_task("done", Some(0))), "（exit 0）");
        assert_eq!(
            exit_suffix(&sample_task("failed", Some(128))),
            "（exit 128）"
        );
        assert_eq!(exit_suffix(&sample_task("stopped", None)), "（已停止）");

        assert_eq!(elapsed_label(&sample_task("done", Some(0))), "01:12");
        // 跑着的按 now 算，别把秒边界写死。
        let mut running = sample_task("running", None);
        running.started_at = now_ms() - 72_000;
        let label = elapsed_label(&running);
        assert!(label == "01:12" || label == "01:13", "{label}");
    }

    /// 后台模式必须在有运行时上下文时才能用：单测里没有，要明确报错而不是悄悄前台跑。
    #[test]
    fn background_command_needs_runtime() {
        let outcome = execute(
            &std::env::temp_dir().to_string_lossy(),
            "run_command",
            r#"{"command":"echo hi","background":true}"#,
        );
        assert!(outcome.error.is_some());
        assert!(outcome.error.unwrap().contains("不支持后台任务"));
    }

    #[test]
    fn task_status_needs_runtime() {
        let outcome = execute(&std::env::temp_dir().to_string_lossy(), "task_status", "{}");
        assert!(outcome.error.is_some());
    }

    /// 声明里必须有 background 开关和 task_status，否则模型根本不知道这条通道。
    #[test]
    fn specs_expose_background_and_task_status() {
        let specs = specs(&EnabledTools::all());
        let run = specs
            .iter()
            .find(|spec| spec["function"]["name"] == "run_command")
            .expect("run_command 声明");
        assert!(run["function"]["parameters"]["properties"]["background"].is_object());
        assert!(specs
            .iter()
            .any(|spec| spec["function"]["name"] == "task_status"));
    }

    #[test]
    fn run_command_reports_exit_code() {
        let dir = temp_dir("cmd");
        let root = dir.to_string_lossy().to_string();
        let outcome = execute(&root, "run_command", r#"{"command":"echo hi"}"#);
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        assert!(outcome.text.contains("exit=0"), "{}", outcome.text);
        assert!(outcome.text.contains("hi"), "{}", outcome.text);
    }

    /// 输出超过管道缓冲（macOS 64KB）不能假性超时：读的人必须同时在读。
    #[test]
    fn run_command_large_output_is_not_a_timeout() {
        let dir = temp_dir("cmdout");
        let root = dir.to_string_lossy().to_string();
        let outcome = execute(
            &root,
            "run_command",
            r#"{"command":"yes x | head -c 300000","timeout_ms":10000}"#,
        );
        assert!(outcome.error.is_none(), "{}", outcome.text);
        assert!(outcome.text.contains("exit=0"), "{}", outcome.text);
    }

    /// 超大输出也不能把内存吃干：展示本来就要截断，读的时候留前一段就够。
    #[test]
    fn run_command_huge_output_is_capped() {
        let dir = temp_dir("cmdhuge");
        let root = dir.to_string_lossy().to_string();
        let started = Instant::now();
        let outcome = execute(
            &root,
            "run_command",
            r#"{"command":"yes x | head -c 4000000","timeout_ms":20000}"#,
        );
        assert!(outcome.error.is_none(), "{}", outcome.text);
        assert!(outcome.text.contains("已截断"), "{}", outcome.text.len());
        assert!(started.elapsed() < Duration::from_secs(20), "不该等到超时");
    }

    #[test]
    fn run_command_timeout_kills_slow_command() {
        let dir = temp_dir("cmdtimeout");
        let root = dir.to_string_lossy().to_string();
        let started = Instant::now();
        let outcome = execute(
            &root,
            "run_command",
            r#"{"command":"sleep 30","timeout_ms":1000}"#,
        );
        assert!(outcome.error.is_some());
        assert!(outcome.text.contains("命令超时"), "{}", outcome.text);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "超时要及时收手"
        );
    }

    /// 停止按钮要能打断正在跑的命令（不是等命令自己跑完）。
    #[test]
    fn run_command_stop_kills_running_command() {
        let dir = temp_dir("cmdstop");
        let root = dir.to_string_lossy().to_string();
        let flag = Arc::new(AtomicBool::new(false));
        let flip = Arc::clone(&flag);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(250));
            flip.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        let outcome = execute_with_stop(
            &root,
            "run_command",
            r#"{"command":"sleep 30","timeout_ms":60000}"#,
            &|| flag.load(Ordering::SeqCst),
        );
        assert!(outcome.error.is_some());
        assert!(outcome.text.contains("已被用户停止"), "{}", outcome.text);
        assert!(started.elapsed() < Duration::from_secs(5), "停止要立刻生效");
    }

    /// 命令拉起的孙子进程要一起收掉：只杀 sh 的话后台的活还会继续干。
    #[test]
    fn run_command_stop_takes_down_child_processes() {
        let dir = temp_dir("cmdtree");
        let root = dir.to_string_lossy().to_string();
        let flag = Arc::new(AtomicBool::new(false));
        let flip = Arc::clone(&flag);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            flip.store(true, Ordering::SeqCst);
        });
        // 后台那支 1.5 秒后会写下 marker；它要是活着，说明只杀了 sh。
        let outcome = execute_with_stop(
            &root,
            "run_command",
            r#"{"command":"(sleep 1.5; touch marker) & sleep 30","timeout_ms":60000}"#,
            &|| flag.load(Ordering::SeqCst),
        );
        assert!(outcome.text.contains("已被用户停止"), "{}", outcome.text);
        std::thread::sleep(Duration::from_millis(1800));
        assert!(!dir.join("marker").exists(), "孙子进程没被收掉");
    }

    /// 真实仓库的形状：文件在第 4 层（src-tauri/src/engines/native/*.rs）。
    /// 原来深度限 3 层，从根上搜不到、还回一句「没有命中」。
    #[test]
    fn grep_reaches_deep_repo_layout() {
        let dir = temp_dir("deepgrep");
        let root = dir.to_string_lossy().to_string();
        fs::create_dir_all(dir.join("src-tauri/src/engines/native")).unwrap();
        fs::write(dir.join("src-tauri/src/engines/native/hit.txt"), "needle\n").unwrap();
        let outcome = execute(&root, "grep", r#"{"pattern":"needle"}"#);
        assert!(outcome.text.contains("hit.txt"), "{}", outcome.text);
        assert!(
            !outcome.text.contains("截断"),
            "层数够，不该有截断提示：{}",
            outcome.text
        );
    }

    /// 真超上限时结论里必须说出来：不能回一句「没有命中」了事。
    #[test]
    fn grep_says_when_scan_was_cut() {
        let dir = temp_dir("cutgrep");
        let root = dir.to_string_lossy().to_string();
        fs::create_dir_all(dir.join("a/b/c/d/e/f/g")).unwrap();
        fs::write(dir.join("a/b/c/d/e/f/g/needle.txt"), "needle\n").unwrap();
        let outcome = execute(&root, "grep", r#"{"pattern":"needle"}"#);
        assert!(outcome.text.contains("没有命中"), "{}", outcome.text);
        assert!(outcome.text.contains("截断"), "{}", outcome.text);
        assert!(
            outcome.text.contains("path"),
            "要说清怎么缩小范围：{}",
            outcome.text
        );
    }

    /// 文件数到顶也一样：只能保证「扫过的那部分」。
    #[test]
    fn grep_says_when_file_cap_hit() {
        let dir = temp_dir("capfiles");
        let root = dir.to_string_lossy().to_string();
        for index in 0..(WALK_MAX_FILES + 5) {
            fs::write(dir.join(format!("f{index:05}.txt")), "x\n").unwrap();
        }
        let outcome = execute(&root, "grep", r#"{"pattern":"needle-nowhere"}"#);
        assert!(outcome.text.contains("没有命中"), "{}", outcome.text);
        assert!(outcome.text.contains("文件数到顶"), "{}", outcome.text);

        let listed = execute(&root, "list_files", "{}");
        assert!(listed.text.contains("文件数到顶"), "{}", listed.text);
    }

    #[test]
    fn wildcard_matches_simple_patterns() {
        assert!(wildcard_match("*.rs", "main.rs"));
        assert!(!wildcard_match("*.rs", "main.ts"));
        assert!(wildcard_match("a?c", "abc"));
        assert!(wildcard_match("*", "anything"));
    }

    // ------------------------------------------------------------ 外部工具

    /// 造一个外部工具：跑起来只回 `echo:<入参>`，足够验证"注册 → 可查 → 可跑 → 可撤"。
    fn plugin_tool(name: &str, trusted: bool) -> Arc<ToolDef> {
        let run: Arc<BoxedRun> = Arc::new(|_ctx, args| {
            ToolResult::plain(ToolOutcome::ok(format!("echo:{args}")))
        });
        Arc::new(ToolDef {
            name: Cow::Owned(name.to_string()),
            schema: ToolSchema::Fixed(json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": "外部工具",
                    "parameters": { "type": "object", "properties": {} }
                }
            })),
            origin: ToolOrigin::Plugin,
            permission: Permission::WriteWorkspace,
            action: Some("mcp"),
            resource_key: None,
            summary: "调用外部工具需要授权",
            blurb: Cow::Borrowed("测试用外部工具"),
            delegable: false,
            enabled_by_default: trusted,
            meta: ToolMeta::default(),
            run: ToolImpl::Boxed(run),
        })
    }

    /// 注册后能被查到、能列出、能执行；撤销后三样一起消失。
    #[test]
    fn external_tool_registers_runs_and_releases() {
        let name = "test__echo_cycle";
        let owner = "test.cycle";
        let registry = ToolRegistry::global();
        registry.register(owner, plugin_tool(name, true)).expect("注册");

        assert!(find(name).is_some(), "注册后应当能按名字查到");
        assert!(specs(&EnabledTools::all())
            .iter()
            .any(|spec| spec["function"]["name"] == name));
        assert_eq!(registry.owner_of(name).as_deref(), Some(owner));

        let enabled = EnabledTools::all();
        let ctx = ToolContext {
            app: None,
            session_id: "",
            directory: ".",
            call_id: "",
            provider: None,
            model: None,
            permission: None,
            enabled: &enabled,
            dag: None,
            should_stop: &|| false,
        };
        let result = run_with_args(&ctx, name, &json!({ "text": "hi" }));
        assert!(result.outcome.error.is_none(), "{:?}", result.outcome.error);
        assert!(result.outcome.text.starts_with("echo:"), "{}", result.outcome.text);

        assert_eq!(registry.release(owner), 1);
        assert!(find(name).is_none(), "撤销后不该还查得到");
        assert!(registry.owner_of(name).is_none());
    }

    /// 外部工具不能顶掉内置工具的名字——模型只认名字，能覆盖就等于能劫持。
    #[test]
    fn external_tool_cannot_shadow_builtin() {
        let error = ToolRegistry::global()
            .register("test.shadow", plugin_tool("write_file", true))
            .unwrap_err();
        assert!(error.contains("重名"), "{error}");
    }

    /// 同名第二个注册者被拒，且不影响第一个。
    #[test]
    fn external_tool_duplicate_is_rejected() {
        let name = "test__echo_dup";
        let registry = ToolRegistry::global();
        registry.register("test.dup_a", plugin_tool(name, true)).expect("第一个");
        let error = registry
            .register("test.dup_b", plugin_tool(name, true))
            .unwrap_err();
        assert!(error.contains("已被 test.dup_a 注册"), "{error}");
        assert_eq!(registry.owner_of(name).as_deref(), Some("test.dup_a"));
        registry.release("test.dup_a");
    }

    /// 默认信任级别决定默认开关：没标 trusted 的外部工具不进模型清单，
    /// 但仍能被显式打开。
    #[test]
    fn untrusted_external_tool_is_off_by_default() {
        let name = "test__echo_untrusted";
        let registry = ToolRegistry::global();
        registry.register("test.untrusted", plugin_tool(name, false)).expect("注册");

        // 生产路径：默认配置（没有这条工具的显式记录）。
        let configured = EnabledTools::from_config();
        assert!(!configured.is_on(&find(name).expect("查得到")));
        assert!(!specs(&configured)
            .iter()
            .any(|spec| spec["function"]["name"] == name));

        // 显式打开之后就能用。
        let mut overrides = std::collections::HashMap::new();
        overrides.insert(name.to_string(), true);
        let opened = EnabledTools::from_map(overrides);
        assert!(opened.is_on(&find(name).expect("查得到")));

        registry.release("test.untrusted");
    }

    /// 被禁的外部工具连直接调用都要拦住（注册表撤销与开关是两道独立的门）。
    #[test]
    fn disabled_external_tool_refuses_to_run() {
        let name = "test__echo_disabled";
        let registry = ToolRegistry::global();
        registry.register("test.disabled", plugin_tool(name, true)).expect("注册");

        let mut overrides = std::collections::HashMap::new();
        overrides.insert(name.to_string(), false);
        let enabled = EnabledTools::from_map(overrides);
        let ctx = ToolContext {
            app: None,
            session_id: "",
            directory: ".",
            call_id: "",
            provider: None,
            model: None,
            permission: None,
            enabled: &enabled,
            dag: None,
            should_stop: &|| false,
        };
        let result = run_with_args(&ctx, name, &json!({}));
        assert!(result.outcome.error.unwrap().contains("已被关闭"));

        registry.release("test.disabled");
    }
}
