//! 节点执行器：把「一个 Agent + 一段任务」跑成「一段报告」。
//!
//! 这一层是 DAG 的**唯一**出口——调度器不关心底下是哪个引擎，只拿一个
//! [`NodeRunner`]。插件化的意义在这里落地：加一个引擎执行器 = 实现这个 trait，
//! 调度、重试、闸门、并发全都复用。
//!
//! 无人值守与审批
//! --------------
//! DAG 的节点是**没人看着**跑的，所以「逐次询问」在这里没有意义：没人点，
//! 线程就永远挂着。因此执行器把计划里写的权限档当作**事前同意**：
//!
//! - `read-only`：只读工具集，写类工具连 schema 都不发；
//! - `workspace-write` / `danger-full-access`：放开写类工具。
//!
//! 注意原生引擎没有内核级沙箱，这两档在**能力**上相同，区别是"用户点了几次
//! 确认"。要真正隔离得靠外部引擎（dsh 的 sandbox）——那由各自执行器负责。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::agents::{AgentProfile, Permission, ToolSelection};
use crate::engines::native::tools::Permission as ToolPermission;
use crate::engines::native::overflow::{clip_tool_result, compress_messages, trim_messages};
use crate::engines::native::{config, provider, tools};

/// 单次执行的停止信号：全局取消 + 本节点超时。
///
/// 两件事合成一个对象，是因为执行路径上只该有一个 `should_stop` 回调；
/// 分成两个闭包，工具层只会拿到其中一个，迟早漏掉"超时"或"取消"。
#[derive(Clone)]
pub struct StopHandle {
    cancelled: Arc<AtomicBool>,
    timed_out: Arc<AtomicBool>,
    deadline: Option<Instant>,
}

impl StopHandle {
    pub fn new(cancelled: Arc<AtomicBool>, timeout_secs: Option<u64>) -> StopHandle {
        StopHandle {
            cancelled,
            timed_out: Arc::new(AtomicBool::new(false)),
            deadline: timeout_secs
                .filter(|secs| *secs > 0)
                .map(|secs| Instant::now() + Duration::from_secs(secs)),
        }
    }

    /// 该停了吗。超时只标一次。
    pub fn hit(&self) -> bool {
        if self.cancelled.load(Ordering::SeqCst) {
            return true;
        }
        match self.deadline {
            Some(deadline) if Instant::now() >= deadline => {
                self.timed_out.store(true, Ordering::SeqCst);
                true
            }
            _ => false,
        }
    }

    pub fn timed_out(&self) -> bool {
        self.timed_out.load(Ordering::SeqCst)
    }

    /// 停止原因（给节点错误文案用）。
    pub fn reason(&self) -> &'static str {
        if self.timed_out() {
            "节点超时"
        } else {
            "运行被取消"
        }
    }
}

/// 一次节点执行的全部输入。
pub struct NodeRequest {
    pub run_id: String,
    /// 所属计划 id：制品作用域要它（同 DAG 可读、跨 DAG 拒绝）。
    pub dag_id: String,
    pub node_id: String,
    pub profile: AgentProfile,
    /// 已替换过占位符的任务正文。
    pub prompt: String,
    /// 节点实际的工作目录（共享目录 / worktree / 临时目录）。
    pub directory: String,
    /// 落地后的权限档（节点覆盖 → 计划默认）。
    pub permission: Permission,
    /// 落地后的工具策略（节点覆盖 → Agent profile）。
    pub selection: ToolSelection,
    pub stop: StopHandle,
    /// 正文增量：给运行面板做直播。
    pub on_text: Arc<dyn Fn(&str) + Send + Sync>,
    /// 新落的一步工具调用（`read_file src/x.rs`）：面板上"现在在干什么"。
    pub on_step: Arc<dyn Fn(&str) + Send + Sync>,
    /// 本轮 token 用量：跑长节点时数字要动，否则看起来像卡住了。
    pub on_usage: Arc<dyn Fn(i64, i64, i64) + Send + Sync>,
    /// 进入第几轮（1 起）+ 轮次上限。模型久不吐字时，面板靠它说明"还在第几轮"。
    pub on_turn: Arc<dyn Fn(u32, u32) + Send + Sync>,
}

/// 一次节点执行的产出。
#[derive(Debug, Default, Clone)]
pub struct NodeOutput {
    pub text: String,
    pub tokens_in: i64,
    pub tokens_out: i64,
    /// tokens_in 里命中前缀缓存的部分（报给运行面板算命中率）。
    pub tokens_cached: i64,
    pub turns: u32,
    pub steps: Vec<String>,
    /// 这一节点里**失败过的工具调用**。节点仍可能成功（模型换了方法或绕开），
    /// 但下游必须知道"这条线索没查成"，否则会把缺口当成"查过且没有问题"。
    /// 执行器把这些条目传给直接 / 间接下游。
    pub failures: Vec<String>,
}

/// 失败清单的上限：太长的清单既花提示词预算，也没人看。
pub const FAILURES_MAX: usize = 12;
/// 单条失败说明保留多少字符。
pub const FAILURE_TEXT_LIMIT: usize = 240;

/// 记一条工具失败。
///
/// 同一个工具反复失败（模型对同一个坏路径重试）只留第一次并记次数；
/// 条目数到上限后不再展开，最后一行改成"还有 N 条已省略"——事实不丢，
/// 但别让一份清单吃掉下游的提示词预算。
pub fn record_failure(output: &mut NodeOutput, tool: &str, error: &str) {
    let reason: String = error
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(FAILURE_TEXT_LIMIT)
        .collect();
    let prefix = format!("{tool}：");
    if let Some(existing) = output.failures.iter_mut().find(|item| item.starts_with(&prefix)) {
        let count = repeat_count(existing) + 1;
        let base = truncate_repeat(existing);
        *existing = format!("{base}（重复 {count} 次）");
        return;
    }

    // 留一格给"省略了 N 条"：清单有上限，但缺口数量本身也是信息。
    let body_limit = FAILURES_MAX.saturating_sub(1);
    if output.failures.len() < body_limit {
        output.failures.push(format!("{prefix}{reason}"));
        return;
    }
    let omitted = output
        .failures
        .iter()
        .last()
        .and_then(|last| omitted_count(last))
        .unwrap_or(0)
        + 1;
    let summary = format!("…另有 {omitted} 条工具失败未展开（清单上限 {body_limit} 条）");
    if output.failures.len() == body_limit {
        output.failures.push(summary);
    } else if let Some(last) = output.failures.last_mut() {
        *last = summary;
    }
}

fn repeat_count(line: &str) -> u32 {
    const MARK: &str = "（重复 ";
    line.rfind(MARK)
        .and_then(|at| {
            line[at + MARK.len()..]
                .split_whitespace()
                .next()?
                .parse::<u32>()
                .ok()
        })
        .unwrap_or(1)
}

fn truncate_repeat(line: &str) -> &str {
    match line.rfind("（重复 ") {
        Some(at) => &line[..at],
        None => line,
    }
}

fn omitted_count(line: &str) -> Option<u32> {
    let rest = line.strip_prefix("…另有 ")?;
    rest.split(' ').next()?.parse::<u32>().ok()
}

/// 节点执行器插件。
pub trait NodeRunner: Send + Sync {
    /// 执行器 id，与 Agent profile 的 `executor` 字段对应。
    fn id(&self) -> &'static str;

    fn run(&self, request: NodeRequest) -> Result<NodeOutput, String>;
}

// ---------------------------------------------------------------- 注册表

/// 执行器注册表：`executor id -> runner`。
///
/// 为什么是进程级全局而不是 Tauri 托管状态：执行器在**调度线程**里按节点查，
/// 而调度线程拿不到 `AppHandle` 之外的上下文（也不该拿）。注册只发生在装配期
/// 一次，之后只读——这个形状用 `OnceLock` 比往每个调用点传一遍 `State` 干净。
///
/// 插件语义：关掉 `dag.executor.native` 之后这里就是空的，`dag_run` 会直接报
/// 「没有装执行器插件」，而不是假装跑了。
static REGISTRY: std::sync::OnceLock<Mutex<HashMap<&'static str, Arc<dyn NodeRunner>>>> =
    std::sync::OnceLock::new();

fn registry() -> &'static Mutex<HashMap<&'static str, Arc<dyn NodeRunner>>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 登记一个执行器。重复登记以最后一次为准（热重载时不会被旧实例挡住）。
pub fn register(runner: Arc<dyn NodeRunner>) {
    if let Ok(mut map) = registry().lock() {
        map.insert(runner.id(), runner);
    }
}

/// 注销（插件关停时用）。
pub fn unregister(id: &'static str) {
    if let Ok(mut map) = registry().lock() {
        map.remove(id);
    }
}

pub fn get(id: &str) -> Option<Arc<dyn NodeRunner>> {
    registry().lock().ok()?.get(id).cloned()
}

/// 已装的执行器 id（给报错文案与设置页用）。
pub fn installed() -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = registry()
        .lock()
        .map(|map| map.keys().copied().collect())
        .unwrap_or_default();
    ids.sort_unstable();
    ids
}

// ---------------------------------------------------------------- 占位符

/// 单个 `{{nodes.<id>.output}}` 最多塞多少字符。
///
/// 上游报告动辄上百 KB，原样拼进下游提示词会直接撞上下文上限（还花钱）。
/// 截断并留一句说明 + 制品引用，比让请求 400 好得多。
///
/// 为什么是 8 KB 而不是 2 KB：读完之后要"再改"的下游节点很常见，一份 5 KB
/// 的调研结论砍掉六成，模型就开始猜了。这个阈值的职责是拦住百 KB 级倾倒。
pub const PROMPT_INLINE_LIMIT: usize = 8_000;

/// 一次提示词里**所有**占位符内联的总预算。
///
/// 单槽上限拦不住扇入：一个「汇总」节点引用 8 份上游报告，8 × 8 KB = 64 KB，
/// 128k 窗口的模型光提示词就吃掉两万多 token，再加上系统提示与工具 schema，
/// 第一轮请求直接 400。而这个节点没人看着，救不回来就是整条链断在这里。
///
/// 所以预算按**槽位数**均分：上游越多，每份分到的越少，总量恒定。
/// 24 KB ≈ 8k token，留给模型自己的历史与输出足够。
pub const PROMPT_BUDGET: usize = 24_000;

/// 单个占位符的下限。
///
/// 均分不能无下限：20 个上游时 24000/20 = 1200，再砍就是标题加一句"略"，
/// 下游只能靠猜。宁可总量略超预算，也要保证每份有一点可用的信息。
const PROMPT_FLOOR: usize = 1_200;

/// 提示词超过这个长度就在运行面板上报一行。
///
/// 上下文超限现在完全不可见——节点跑挂了只看到一句 400。让人先看见
/// "这一轮的提示词有多大"，才知道该往哪儿调。
pub const PROMPT_NOTICE: usize = 12_000;

/// 数一遍任务里有多少个占位符槽位。
///
/// 为什么按**出现次数**而不是按节点去重：同一个节点被引用三次，提示词里就是
/// 三份正文——token 会真的重复三遍。去重计数会让总预算凭空失效。
///
/// 没闭合的占位符不计：它会被原样输出，不产生内容。
pub(crate) fn count_slots(task: &str) -> (usize, usize) {
    const NODES: &str = "{{nodes.";
    const ARTIFACTS: &str = "{{artifacts.";
    let mut nodes = 0usize;
    let mut artifacts = 0usize;
    let mut rest = task;
    loop {
        let nodes_at = rest.find(NODES);
        let artifacts_at = rest.find(ARTIFACTS);
        let (start, is_node) = match (nodes_at, artifacts_at) {
            (None, None) => break,
            (Some(at), None) => (at, true),
            (None, Some(at)) => (at, false),
            (Some(a), Some(b)) => {
                if a <= b {
                    (a, true)
                } else {
                    (b, false)
                }
            }
        };
        let prefix = if is_node { NODES } else { ARTIFACTS };
        let after = &rest[start + prefix.len()..];
        let Some(close) = after.find("}}") else { break };
        if is_node {
            nodes += 1;
        } else {
            artifacts += 1;
        }
        rest = &after[close + 2..];
    }
    (nodes, artifacts)
}

/// 每个槽位能分到的内联字数。
///
/// 固定总预算均分，夹在「下限」与「单词上限」之间：
/// - 1 个上游 → 8000（拿满单槽上限，行为与加预算前一致）；
/// - 3 个 → 8000；(24000/3)
/// - 8 个 → 3000；
/// - 20 个 → 命中下限 1200（总量 24k 出头，可以接受）。
fn slot_budget(slots: usize) -> usize {
    if slots == 0 {
        return PROMPT_INLINE_LIMIT;
    }
    (PROMPT_BUDGET / slots).clamp(PROMPT_FLOOR, PROMPT_INLINE_LIMIT)
}

/// 把 `{{nodes.<id>.output}}` / `{{artifacts.<别名>.<字段>}}` 换成实际内容。
///
/// `outputs` 是上游正文（键为节点 id），`artifacts` 是本次可用的制品视图
/// （键为别名；隐式别名就是上游节点 id）。
pub fn resolve_task(
    task: &str,
    outputs: &std::collections::HashMap<String, String>,
    artifacts: &std::collections::HashMap<String, super::artifact::ArtifactView>,
) -> String {
    const NODES: &str = "{{nodes.";
    const ARTIFACTS: &str = "{{artifacts.";

    // 先数一遍槽位再动手：预算要均分，一边替换一边算就只能"先到先得"，
    // 排在后面的上游被饿死。
    let (node_slots, artifact_slots) = count_slots(task);
    let per_node = slot_budget(node_slots + artifact_slots);
    // 制品本来给的就是摘要，不该比一个正文槽位还大。
    let per_artifact = per_node.min(super::artifact::SUMMARY_LIMIT);

    let mut out = String::with_capacity(task.len());
    let mut rest = task;
    loop {
        let nodes_at = rest.find(NODES);
        let artifacts_at = rest.find(ARTIFACTS);
        let (start, prefix) = match (nodes_at, artifacts_at) {
            (None, None) => break,
            (Some(at), None) => (at, NODES),
            (None, Some(at)) => (at, ARTIFACTS),
            (Some(a), Some(b)) => {
                if a <= b {
                    (a, NODES)
                } else {
                    (b, ARTIFACTS)
                }
            }
        };
        out.push_str(&rest[..start]);
        let after = &rest[start + prefix.len()..];
        let Some(close) = after.find("}}") else {
            // 没闭合：原样保留，让用户看见自己写错了。
            out.push_str(&rest[start..]);
            return out;
        };
        let inner = &after[..close];
        if prefix == NODES {
            let id = inner.split('.').next().unwrap_or("").trim();
            out.push_str(&node_text(id, outputs, artifacts, per_node));
        } else {
            let mut parts = inner.split('.');
            let alias = parts.next().unwrap_or("").trim();
            let field = parts.next().map(str::trim).unwrap_or("summary");
            out.push_str(&artifact_text(
                alias,
                if field.is_empty() { "summary" } else { field },
                artifacts,
                per_artifact,
            ));
        }
        rest = &after[close + 2..];
    }
    out.push_str(rest);
    out
}

/// 一个上游节点的正文：超长截断，并在有制品时给出引用。
fn node_text(
    id: &str,
    outputs: &std::collections::HashMap<String, String>,
    artifacts: &std::collections::HashMap<String, super::artifact::ArtifactView>,
    limit: usize,
) -> String {
    let Some(text) = outputs.get(id) else {
        return format!("（节点 {id} 没有产出）");
    };
    if text.trim().is_empty() {
        return format!("（节点 {id} 没有留下正文）");
    }
    let artifact = artifacts.get(id).filter(|view| !view.id.is_empty());
    if text.chars().count() <= limit {
        return text.clone();
    }
    let mut out: String = text.chars().take(limit).collect();
    match artifact {
        Some(view) => out.push_str(&format!(
            "\n\n（上游报告过长，以上为前 {limit} 字。完整内容见制品 {}，\
             需要细节就调用 prism.artifact.get 取）",
            view.id
        )),
        None => out.push_str("\n\n（上游报告过长，以上为节选）"),
    }
    out
}

/// 一个制品占位符：`id` / `summary` / `meta`。
fn artifact_text(
    alias: &str,
    field: &str,
    artifacts: &std::collections::HashMap<String, super::artifact::ArtifactView>,
    summary_limit: usize,
) -> String {
    let Some(view) = artifacts.get(alias) else {
        return format!("（未声明的制品 {alias}）");
    };
    match field {
        "id" => {
            if view.id.is_empty() {
                "（制品不可用：上游没有落盘成功，只能用摘要）".to_string()
            } else {
                view.id.clone()
            }
        }
        "meta" => view.meta_json(),
        "summary" => {
            if view.summary.trim().is_empty() {
                return "（制品没有正文）".to_string();
            }
            // 摘要本身已经在 2048 上下，但扇入时会叠。按预算再削一道，
            // 削了就说清楚，别让模型以为那就是全文。
            let total = view.summary.chars().count();
            if total <= summary_limit {
                let mut out = view.summary.clone();
                if view.truncated {
                    out.push_str(&format!(
                        "\n\n（摘要截到 {} 字；全文见制品 {}）",
                        super::artifact::SUMMARY_LIMIT,
                        if view.id.is_empty() { "（不可用）" } else { &view.id }
                    ));
                }
                return out;
            }
            let mut out: String = view.summary.chars().take(summary_limit).collect();
            out.push_str(&format!(
                "\n\n（摘要过长，以上为前 {summary_limit} 字；全文见制品 {}）",
                if view.id.is_empty() { "（不可用）" } else { &view.id }
            ));
            out
        }
        other => format!("（制品占位符不支持 .{other}）"),
    }
}

// ---------------------------------------------------------------- 原生执行器

/// 用 Prism 原生引擎（provider + 内置工具）跑一个节点。
///
/// 这是编排的**默认**执行器（插件 `dag.executor.native` 注册的），不是外部引擎
/// ——只有 Agent profile 的 `executor` 被改成 codex / dsh / opencode，节点才走
/// [`super::engine_runner::EngineRunner`]。
///
/// 下面的回合循环是 `engines::native::agent` 主循环的手写复刻：节点不需要会话
/// 持久化、审批槽位和事件流，所以没去复用那一个。代价是**上下文治理是两份**，
/// 见 `engines::native::overflow` 文件头的「两套循环，一处定义」。
pub struct NativeRunner {
    /// 单测里没有 Tauri 上下文，工具层的 `app` 允许为空。
    app: Option<tauri::AppHandle>,
}

impl NativeRunner {
    pub fn new(app: Option<tauri::AppHandle>) -> Self {
        NativeRunner { app }
    }
}

impl NodeRunner for NativeRunner {
    fn id(&self) -> &'static str {
        "native"
    }

    fn run(&self, request: NodeRequest) -> Result<NodeOutput, String> {
        let cfg = config::load();
        let (provider_cfg, model) = pick_model(&cfg, request.profile.model.as_deref())?;
        let enabled = tools::EnabledTools::from_config();
        let cap = match request.permission {
            Permission::ReadOnly => ToolPermission::ReadOnly,
            _ => ToolPermission::WriteWorkspace,
        };
        let specs = filter_specs(tools::specs_for(&enabled, cap, true), &request.selection);
        // 制品工具的边界：作用域由执行器注入，模型改不了 dag_id，
        // 所以"读别的 DAG 的产物"在参数层面就不成立。
        let scope = super::artifact::DagScope {
            dag_id: request.dag_id.clone(),
            node_id: request.node_id.clone(),
        };

        let mut messages: Vec<Value> = vec![
            serde_json::json!({ "role": "system", "content": system_prompt(&request) }),
            serde_json::json!({ "role": "user", "content": request.prompt }),
        ];
        let mut output = NodeOutput::default();
        // 上游报告多的时候提示词会很大：先让面板上看得见这个数字。
        // 上下文超限现在只表现为一句 400，没人知道起因是"这一轮光提示词就很大"。
        let prompt_chars = request.prompt.chars().count();
        if prompt_chars > PROMPT_NOTICE {
            let step = format!("提示词 {prompt_chars} 字（上游报告已按预算均分截断）");
            (request.on_step)(&step);
            output.steps.push(step);
        }
        let max_turns = cfg.max_turns();
        let stop = request.stop.clone();
        let should_stop = move || stop.hit();
        /// 上下文超限的救援上限：瘦身是机械的，两次还装不下就是真装不下。
        ///
        /// 比聊天路径的 `agent::RESCUE_LIMIT`（3）少一次，因为救援手段不同：
        /// 聊天可以叫模型做摘要（`compact::rescue`），裁剪有信息损失但至少能
        /// 收敛到窗口内；这里纯机械裁剪，头一次救不回来，第二次多半也一样。
        /// 宁可直接失败让用户看见原因，也不要把同一个大提示词重发三遍再烧钱。
        const RESCUE_LIMIT: u32 = 2;
        // 轮次自己数：救援重试不烧轮次——它消耗的是被拒的那次请求。
        let mut turn = 0u32;
        let mut rescues = 0u32;
        while turn < max_turns {
            if should_stop() {
                return Err(stop_error(&request.stop, output));
            }
            // 最后一轮不给工具：逼它把已知信息写成报告。否则"跑满轮次"的节点
            // 会带着一串半成品工具结果结束，下游什么也接不到。
            let final_turn = turn + 1 == max_turns;
            // 先报轮次再发请求：请求要等几秒到几分钟，这段时间面板至少要有个
            // "第 N/M 轮"在走，而不是干等一句"运行中"。
            (request.on_turn)(turn + 1, max_turns);
            let turn_specs: &[Value] = &specs;
            if let Some(hint) = turn_nudge(turn, max_turns) {
                messages.push(serde_json::json!({
                    "role": "user",
                    "content": hint,
                }));
            }
            let mut on_text = |delta: &str| (request.on_text)(delta);
            let mut on_reasoning = |_delta: &str| {};
            // 强收轮不再把 tools 清空——tools 数组在前缀缓存里，删掉它等于
            // 让整个会话最大的一次请求全量重算。掩码：tools 照发，锁死
            // tool_choice = none，前缀逐字节不变。
            let chat_result = if final_turn {
                provider::chat_text_only(
                    &provider_cfg,
                    &model,
                    &messages,
                    turn_specs,
                    &mut on_text,
                    &mut on_reasoning,
                    &should_stop,
                )
            } else {
                provider::chat(
                    &provider_cfg,
                    &model,
                    &messages,
                    turn_specs,
                    &mut on_text,
                    &mut on_reasoning,
                    &should_stop,
                )
            };
            let chat = match chat_result {
                Ok(chat) => chat,
                Err(error) => {
                    if request.stop.hit() {
                        return Err(stop_error(&request.stop, output));
                    }
                    let text = error.to_string();
                    // 模型拒收（上下文超限）：先压缩，压不动再机械裁剪。
                    // 节点没人看着，不能等人敲 /compact，得自己救活自己。
                    if rescues < RESCUE_LIMIT && provider::is_context_overflow(&text) {
                        // 第一次优先压缩：多花一次模型调用，保住前面十几轮工具
                        // 调用攒出来的结论——节点重跑一遍要十几分钟。压不动
                        // （没有中段、摘要调用也失败）才退回裁剪，那是纯丢。
                        let recovered = if rescues == 0 {
                            match compress_messages(
                                &mut messages,
                                &provider_cfg,
                                &model,
                                &should_stop,
                            ) {
                                Ok(compressed) => {
                                    let step =
                                        format!("上下文超限：{compressed} 条历史已压成摘要，继续干");
                                    (request.on_step)(&step);
                                    if output.steps.len() < 24 {
                                        output.steps.push(step);
                                    }
                                    true
                                }
                                Err(_) => trim_messages(&mut messages),
                            }
                        } else {
                            trim_messages(&mut messages)
                        };
                        if recovered {
                            rescues += 1;
                            continue;
                        }
                    }
                    if provider::is_context_overflow(&text) {
                        // 压缩和裁剪都用尽还是装不下。错误里要交代"已经试过什么"
                        // 和"它到底干到哪一步"——否则用户只看到一句 maximum
                        // context length，会以为节点整个白跑了。
                        let trail = output
                            .steps
                            .iter()
                            .rev()
                            .take(5)
                            .rev()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(" → ");
                        let mut message = format!(
                            "{text}\n（已自动压缩与裁剪仍装不下：这个节点的提示词或工具输出超出\
                             模型窗口。把扇入拆成两级汇总、让上游结论更短，或换上下文更大的模型。）"
                        );
                        if !trail.is_empty() {
                            message.push_str(&format!("\n失败前最后几步：{trail}"));
                        }
                        return Err(message);
                    }
                    return Err(text);
                }
            };
            if let Some(usage) = chat.usage {
                output.tokens_in += usage.input;
                output.tokens_out += usage.output;
                output.tokens_cached += usage.cached;
                // 逐轮报出去：一个节点可能要跑十轮，等结束才显示数字就是十分钟空白。
                (request.on_usage)(usage.input, usage.output, usage.cached);
            }
            output.turns = turn + 1;

            if chat.tool_calls.is_empty() {
                output.text = chat.text.trim().to_string();
                if output.text.is_empty() {
                    return Err("模型没有返回正文（这一节点没有产出）".to_string());
                }
                return Ok(output);
            }

            let mut assistant = serde_json::json!({ "role": "assistant", "content": chat.text });
            assistant["tool_calls"] = Value::Array(
                chat.tool_calls
                    .iter()
                    .map(|call| {
                        serde_json::json!({
                            "id": call.id,
                            "type": "function",
                            "function": { "name": call.name, "arguments": call.arguments }
                        })
                    })
                    .collect(),
            );
            messages.push(assistant);

            for call in &chat.tool_calls {
                if should_stop() {
                    return Err(stop_error(&request.stop, output));
                }
                let args: Value =
                    serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
                // 权限档与白名单都在 `run_with_args` 里统一拦（聊天 / 编排 /
                // 子代理共用一处出口）。这里不再特判——特判过的那一版只挡了
                // 「有审批动作」的工具，白名单外的工具照样能调。
                let result = tools::run_with_args(
                    &tools::ToolContext {
                        app: self.app.as_ref(),
                        session_id: &format!("dag:{}:{}", request.run_id, request.node_id),
                        directory: &request.directory,
                        call_id: &call.id,
                        provider: Some(&provider_cfg),
                        model: Some(&model),
                        permission: Some(request.permission.as_str()),
                        selection: Some(&request.selection),
                        enabled: &enabled,
                        dag: Some(&scope),
                        should_stop: &should_stop,
                    },
                    &call.name,
                    &args,
                )
                .outcome;
                if let Some(error) = &result.error {
                    // 失败也要让下游看见：模型可能绕过去了，但证据缺口还在。
                    record_failure(&mut output, &call.name, error);
                }
                if output.steps.len() < 24 {
                    let step = format!(
                        "{}{}",
                        call.name,
                        summarize_args(&args)
                            .map(|text| format!(" {text}"))
                            .unwrap_or_default()
                    );
                    // 先推给面板再入队：这一步已经在跑了，别等结果回来才让人看见。
                    (request.on_step)(&step);
                    output.steps.push(step);
                }
                messages.push(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": call.id,
                    // 每一轮都重发整份历史，一个不设限的大输出乘以轮数就能把
                    // 节点拖爆——进历史前限流，头尾各留一段。
                    "content": clip_tool_result(&result.text),
                }));
            }
            turn += 1;
        }

        // 跑到这里说明轮次用尽仍没收敛。有正文就当归档交出去（比整场失败有用），
        // 但标清"是截断的"。
        if output.text.trim().is_empty() {
            // 没正文等于下游什么都拿不到。把最后几步带上：用户能看出是卡在
            // 哪一类操作上，而不是笼统地"跑满轮次"，下次知道该改哪。
            let trail = output
                .steps
                .iter()
                .rev()
                .take(5)
                .rev()
                .cloned()
                .collect::<Vec<_>>()
                .join(" → ");
            return Err(if trail.is_empty() {
                format!("跑满 {max_turns} 轮仍没有给出结论")
            } else {
                format!("跑满 {max_turns} 轮仍没有给出结论；最后几步：{trail}")
            });
        }
        Ok(output)
    }
}

/// 每一轮该给模型什么提醒：倒数第二轮交代"正文会被保存"，最后一轮强收。
///
/// **不要催它调 `prism_artifact.put`**：执行器在节点结束时已经把 `output.text`
/// 落成主制品（`RunHandle::save_artifact` → `put_text`），实测一个 DAG 里 53 个
/// 制品全在，模型一次工具都没调也照样有 `report`。催它反而多烧一轮去调一个
/// 不必要的东西，而且它可能只把摘要写进制品、把完整正文丢掉。
///
/// 倒数第二轮该说的是"把结论写完整"——难点从来不是没人存，是最后几轮被强收时
/// 正文写得太潦草。
fn turn_nudge(turn: u32, max_turns: u32) -> Option<&'static str> {
    if turn + 1 >= max_turns {
        Some("轮次要用完了：不要再调用工具，直接把结论写成报告。")
    } else if turn + 2 == max_turns {
        Some(
            "还剩两轮：把结论写完整——最后这段正文会被自动落盘成制品，\
             下游节点读到的就是它。",
        )
    } else {
        None
    }
}

fn stop_error(stop: &StopHandle, output: NodeOutput) -> String {
    let mut text = format!("{}（已跑 {} 轮）", stop.reason(), output.turns);
    if !output.text.trim().is_empty() {
        text.push_str("，保留了部分正文");
    }
    text
}

/// 系统提示：只用 profile 写的那段 + 一条运行环境事实。
///
/// 不在这里塞"你是一个 AI"这类话：那是模型自己的事，写进去只花 token。
fn system_prompt(request: &NodeRequest) -> String {
    let mut text = if request.profile.system_prompt.trim().is_empty() {
        format!("你是「{}」这个 Agent 的执行者。", request.profile.name)
    } else {
        request.profile.system_prompt.trim().to_string()
    };
    text.push_str("\n\n运行环境：");
    text.push_str(&format!("工作目录 {}；", request.directory));
    text.push_str(&format!("权限档「{}」。", permission_label(request.permission)));
    text.push_str(
        "你正在无人值守地执行 DAG 里的一个节点：没人会回你的问题，需要的信息只能靠工具自己查。",
    );
    text.push_str("最后一条正文就是交付物（会作为上游结论传给下游节点），写结论而不是过程流水账。");
    // 表格里的竖线必须转义：报告里写 `setServiceInfo(flags | 32)` 时没转义，
    // 那一行被当成四列，整张证据表从中间裂开——内容是对的，渲染全废。
    text.push_str(
        "用 markdown 表格时，单元格里的竖线要写成 `\\|`（如 `flags \\| 32`），\
         否则会被当成列分隔符，整行错位。",
    );
    text
}

fn permission_label(permission: Permission) -> &'static str {
    match permission {
        Permission::ReadOnly => "只读",
        Permission::WorkspaceWrite => "工作区可写",
        Permission::FullAccess => "完全访问",
    }
}

/// 按 Agent 的工具策略筛 schema。
///
/// 判定本身在 [`tools::selection_allows`]：schema 裁剪与调用闸门必须共用同一
/// 份规则，否则「模型看得见」和「实际跑得动」会各说各话。
/// `shared: None` 表示"该档权限下的全部公用工具"，不是"没有工具"——这个
/// 区别在 `agents::ToolSelection` 那里已经讲清，这里只是照着执行。
fn filter_specs(specs: Vec<Value>, selection: &ToolSelection) -> Vec<Value> {
    specs
        .into_iter()
        .filter(|spec| {
            let name = spec
                .get("function")
                .and_then(|value| value.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            tools::selection_allows(selection, name)
        })
        .collect()
}

fn summarize_args(args: &Value) -> Option<String> {
    let object = args.as_object()?;
    // `purpose`：跑脚本时「它要干什么」比正文短得多，更适合当卡片摘要。
    for key in ["path", "file", "pattern", "command", "purpose", "query", "url"] {
        if let Some(text) = object.get(key).and_then(Value::as_str) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.chars().take(60).collect());
            }
        }
    }
    None
}

/// 选模型：profile 指定的模型优先，其次设置里的默认。
///
/// `model` 允许写成 `provider/model` 或裸 model id——和 opencode 的写法一致，
/// 作者不必记住内部的两级结构。
fn pick_model(
    cfg: &config::NativeConfig,
    wanted: Option<&str>,
) -> Result<(config::ProviderConfig, String), String> {
    if let Some(wanted) = wanted.map(str::trim).filter(|value| !value.is_empty()) {
        if let Some((provider_id, model_id)) = wanted.split_once('/') {
            if let Some(provider) = cfg.find(provider_id) {
                if provider.models.iter().any(|item| item.id == model_id) {
                    return Ok((provider.clone(), model_id.to_string()));
                }
            }
        }
        if let Some(provider) = cfg
            .providers
            .iter()
            .find(|provider| provider.models.iter().any(|item| item.id == wanted))
        {
            return Ok((provider.clone(), wanted.to_string()));
        }
    }
    let (provider, model) = cfg
        .selection()
        .ok_or_else(|| "原生引擎还没配置：打开 设置 → 模型管理，添加一个模型服务".to_string())?;
    Ok((provider.clone(), model.id.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::artifact::ArtifactView;
    use std::collections::HashMap;

    /// 倒数第二轮的提醒只能是"写完整"，不能催它调工具。
    ///
    /// 执行器在节点结束时已经自动落盘了（实测 53 个制品全在，模型零工具调用也
    /// 有 `report`）。催 `prism_artifact.put` 等于让它多烧一轮去做一件已经做过的
    /// 事，还可能把完整正文换成一段摘要。
    #[test]
    fn nudge_asks_for_complete_text_not_for_a_tool_call() {
        let max = 12;
        assert!(turn_nudge(0, max).is_none());
        assert!(turn_nudge(9, max).is_none());

        let save = turn_nudge(10, max).expect("倒数第二轮应有提醒");
        assert!(save.contains("自动落盘"), "要交代正文会被保存：{save}");
        assert!(
            !save.contains("prism_artifact_put"),
            "不该催它调工具，执行器已经存过了：{save}"
        );

        let close = turn_nudge(11, max).expect("最后一轮应强收");
        assert!(close.contains("不要再调用工具"), "{close}");
    }

    /// 轮次很少时不能出现"提醒落盘的那轮同时也是强收轮"。
    #[test]
    fn nudge_degrades_gracefully_on_short_budgets() {
        // 1 轮：只有强收，不提落盘（提了也没用）。
        assert!(turn_nudge(0, 1).expect("应强收").contains("不要再调用工具"));
        // 2 轮：第一轮交代"写完整"，第二轮强收。
        assert!(turn_nudge(0, 2).expect("应有提醒").contains("自动落盘"));
        assert!(turn_nudge(1, 2).expect("应强收").contains("不要再调用工具"));
    }

    fn outputs(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(id, text)| (id.to_string(), text.to_string()))
            .collect()
    }

    fn views(items: &[(&str, &str, &str)]) -> HashMap<String, ArtifactView> {
        items
            .iter()
            .map(|(alias, id, text)| {
                (
                    alias.to_string(),
                    ArtifactView {
                        id: id.to_string(),
                        key: "report".to_string(),
                        kind: "text".to_string(),
                        size: text.len() as u64,
                        sha256: "abc".to_string(),
                        summary: text.chars().take(2048).collect(),
                        truncated: text.chars().count() > 2048,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn substitutes_upstream_reports() {
        let task = "基于这些结论继续：\n{{nodes.research.output}}";
        let text = resolve_task(
            task,
            &outputs(&[("research", "找到了 3 处调用")]),
            &HashMap::new(),
        );
        assert!(text.contains("找到了 3 处调用"));
        assert!(!text.contains("{{"));
    }

    #[test]
    fn missing_node_does_not_leave_a_hole() {
        let text = resolve_task("[{{nodes.ghost.output}}]", &outputs(&[]), &HashMap::new());
        assert!(text.contains("没有产出"), "{text}");
    }

    #[test]
    fn long_reports_are_truncated_and_point_at_the_artifact() {
        let long = "啊".repeat(PROMPT_INLINE_LIMIT + 100);
        let artifacts = views(&[("a", "art-1", &long)]);
        let text = resolve_task("{{nodes.a.output}}", &outputs(&[("a", &long)]), &artifacts);
        assert!(text.contains("上游报告过长"));
        assert!(text.contains("art-1"), "截断时必须给出制品引用：{text}");
        assert!(text.chars().count() < PROMPT_INLINE_LIMIT + 300);
    }

    /// 扇入：一个「汇总」节点引用 10 份上游报告。
    ///
    /// 加总预算之前这里是 10 × 8 KB = 80 KB 的提示词，128k 窗口的模型
    /// 光提示词就吃两万多 token，第一轮直接 400——而且节点没人看着，救不回来。
    #[test]
    fn fan_in_stays_within_the_total_prompt_budget() {
        let long = "哈".repeat(PROMPT_INLINE_LIMIT * 2);
        let pairs: Vec<(String, String)> = (0..10)
            .map(|index| (format!("n{index}"), long.clone()))
            .collect();
        let task = (0..10)
            .map(|index| format!("第 {index} 份：{{{{nodes.n{index}.output}}}}"))
            .collect::<Vec<_>>()
            .join("\n");
        let outputs: HashMap<String, String> = pairs.into_iter().collect();

        let text = resolve_task(&task, &outputs, &HashMap::new());
        // 每份分到 2400（24000/10），加说明文字与模板骨架，留一点余量。
        assert!(
            text.chars().count() < PROMPT_BUDGET + 2_000,
            "扇入提示词必须落回总预算：{} 字",
            text.chars().count()
        );
        // 每份都还在（不是「前面的吃光、后面的饿死」）。
        for index in 0..10 {
            assert!(text.contains(&format!("第 {index} 份：")), "第 {index} 份被丢了");
        }
        assert_eq!(text.matches("上游报告过长").count(), 10, "每份都该标注被截断");
    }

    /// 一个上游时行为不变：拿满 8 KB 单槽上限。
    #[test]
    fn a_single_upstream_keeps_the_full_slot_limit() {
        let long = "嗯".repeat(PROMPT_INLINE_LIMIT * 3);
        let text = resolve_task(
            "{{nodes.only.output}}",
            &outputs(&[("only", &long)]),
            &HashMap::new(),
        );
        let body = text.split("\n\n（上游报告过长").next().unwrap();
        assert_eq!(body.chars().count(), PROMPT_INLINE_LIMIT);
    }

    /// 同一节点被引用两次就是两份正文（token 真的重复两遍），预算得按出现次数算。
    #[test]
    fn repeated_references_are_counted_per_occurrence() {
        assert_eq!(count_slots("{{nodes.a.output}}"), (1, 0));
        assert_eq!(
            count_slots("{{nodes.a.output}} + {{nodes.a.output}}"),
            (2, 0)
        );
        assert_eq!(count_slots("{{artifacts.x.summary}}"), (0, 1));
        assert_eq!(
            count_slots("{{nodes.a.output}}{{artifacts.b.id}}"),
            (1, 1)
        );
        // 没闭合的不算槽位：它原样输出，不产生内容。
        assert_eq!(count_slots("{{nodes.a.output"), (0, 0));
    }

    /// 槽位越多单个越小，但绝不低于下限，也不超过单槽上限。
    #[test]
    fn slot_budget_favours_fairness_with_a_floor() {
        assert_eq!(slot_budget(0), PROMPT_INLINE_LIMIT);
        assert_eq!(slot_budget(1), PROMPT_INLINE_LIMIT);
        assert_eq!(slot_budget(3), PROMPT_INLINE_LIMIT);
        assert_eq!(slot_budget(8), 3_000);
        assert_eq!(slot_budget(20), PROMPT_FLOOR, "低于下限就该顶住");
    }

    /// 制品摘要也参与预算：扇入时不能每个都按 2048 叠上去。
    #[test]
    fn artifact_summaries_also_follow_the_budget() {
        let long = "字".repeat(2_048);
        let mut pairs: Vec<(String, String, String)> = Vec::new();
        for index in 0..12 {
            pairs.push((format!("a{index}"), format!("art-{index}"), long.clone()));
        }
        let refs: Vec<(&str, &str, &str)> = pairs
            .iter()
            .map(|(alias, id, text)| (alias.as_str(), id.as_str(), text.as_str()))
            .collect();
        let artifacts = views(&refs);
        let task = (0..12)
            .map(|index| format!("{{{{artifacts.a{index}.summary}}}}"))
            .collect::<Vec<_>>()
            .join("\n");
        let text = resolve_task(&task, &HashMap::new(), &artifacts);
        assert!(
            text.chars().count() < PROMPT_BUDGET + 2_000,
            "12 份摘要不能各自 2048 地叠：{} 字",
            text.chars().count()
        );
        assert!(text.contains("摘要过长"), "削了就要说清楚：{text}");
    }

    #[test]
    fn unclosed_placeholder_is_left_alone() {
        let text = resolve_task("{{nodes.a.output", &outputs(&[("a", "x")]), &HashMap::new());
        assert_eq!(text, "{{nodes.a.output");
    }

    #[test]
    fn report_and_files_suffixes_both_resolve() {
        let text = resolve_task(
            "{{nodes.a.report}}|{{nodes.a.files}}",
            &outputs(&[("a", "报告")]),
            &HashMap::new(),
        );
        assert_eq!(text, "报告|报告");
    }

    #[test]
    fn artifact_fields_render() {
        let artifacts = views(&[("notes", "art-9", "三处风险")]);
        let text = resolve_task(
            "id={{artifacts.notes.id}}\n摘要={{artifacts.notes.summary}}\n裸={{artifacts.notes}}",
            &HashMap::new(),
            &artifacts,
        );
        assert!(text.contains("id=art-9"), "{text}");
        assert_eq!(text.matches("三处风险").count(), 2);

        let meta = resolve_task("{{artifacts.notes.meta}}", &HashMap::new(), &artifacts);
        assert!(meta.contains("\"id\":\"art-9\""), "{meta}");
        assert!(meta.contains("\"key\":\"report\""));
    }

    #[test]
    fn missing_artifact_degrades_to_summary() {
        // 写入失败：id 为空，但摘要还能用——不能变成空白。
        let artifacts = views(&[("notes", "", "兜底摘要")]);
        let text = resolve_task(
            "{{artifacts.notes.id}} / {{artifacts.notes.summary}}",
            &HashMap::new(),
            &artifacts,
        );
        assert!(text.contains("制品不可用"), "{text}");
        assert!(text.contains("兜底摘要"));
    }

    #[test]
    fn unknown_artifact_and_field_are_explained() {
        let text = resolve_task("{{artifacts.ghost.id}}", &HashMap::new(), &HashMap::new());
        assert!(text.contains("未声明的制品"), "{text}");

        let artifacts = views(&[("notes", "art-1", "x")]);
        let text = resolve_task("{{artifacts.notes.path}}", &HashMap::new(), &artifacts);
        assert!(text.contains("不支持 .path"), "{text}");
    }

    #[test]
    fn mixed_placeholders_keep_order() {
        let artifacts = views(&[("notes", "art-1", "摘要")]);
        let text = resolve_task(
            "A{{artifacts.notes.id}}B{{nodes.r.output}}C",
            &outputs(&[("r", "正文")]),
            &artifacts,
        );
        assert_eq!(text, "Aart-1B正文C");
    }

    #[test]
    fn tool_selection_filters_specs() {
        let specs = vec![
            serde_json::json!({"function": {"name": "read_file"}}),
            serde_json::json!({"function": {"name": "edit_file"}}),
            serde_json::json!({"function": {"name": "run_command"}}),
        ];
        // deny 优先
        let selection = ToolSelection {
            shared: None,
            private: Vec::new(),
            deny: vec!["run_command".to_string()],
        };
        let names: Vec<String> = filter_specs(specs.clone(), &selection)
            .into_iter()
            .map(|spec| spec["function"]["name"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(names, vec!["read_file", "edit_file"]);

        // 显式白名单：只有列表里的能看见
        let selection = ToolSelection {
            shared: Some(vec!["read_file".to_string()]),
            private: vec!["private_tool".to_string()],
            deny: Vec::new(),
        };
        let names: Vec<String> = filter_specs(specs, &selection)
            .into_iter()
            .map(|spec| spec["function"]["name"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(names, vec!["read_file"]);
    }

    /// 白名单里写的是内部名（`bin.meta.identify`），schema 里是 wire 名。
    ///
    /// 两边不归一，带点号的工具会被静默筛空：模型手上一个分析工具都没有，
    /// 只能回"我做不到"——这是"agent 不干活"的第二种成因，比 400 更难查。
    #[test]
    fn dotted_tools_survive_the_agent_allow_list() {
        let specs = vec![
            serde_json::json!({"function": {"name": "read_file"}}),
            serde_json::json!({"function": {"name": "bin_meta_identify"}}),
            serde_json::json!({"function": {"name": "prism_dag_node_add"}}),
        ];
        let selection = ToolSelection {
            shared: Some(vec!["read_file".to_string(), "bin.meta.identify".to_string()]),
            private: Vec::new(),
            deny: Vec::new(),
        };
        let names: Vec<String> = filter_specs(specs, &selection)
            .into_iter()
            .map(|spec| spec["function"]["name"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(names, vec!["read_file", "bin_meta_identify"]);

        // deny 走同一个归一逻辑：写内部名也能挡住 wire 名。
        let specs = vec![serde_json::json!({"function": {"name": "bin_meta_identify"}})];
        let selection = ToolSelection {
            shared: None,
            private: Vec::new(),
            deny: vec!["bin.meta.identify".to_string()],
        };
        assert!(filter_specs(specs, &selection).is_empty());
    }

    #[test]
    fn stop_handle_reports_timeout() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let stop = StopHandle::new(cancelled.clone(), Some(0));
        assert!(!stop.hit(), "0 秒表示不超时");
        cancelled.store(true, Ordering::SeqCst);
        assert!(stop.hit());
        assert_eq!(stop.reason(), "运行被取消");
    }

    /// 安全工种的显式白名单里没写 `tools.list`，它也必须活下来：模型在被裁剪
    /// 之后还得能自查"我到底有什么"。deny 仍然优先。
    #[test]
    fn self_inspection_tool_survives_the_allow_list() {
        let specs = vec![
            serde_json::json!({"function": {"name": "read_file"}}),
            serde_json::json!({"function": {"name": "tools_list"}}),
            serde_json::json!({"function": {"name": "write_file"}}),
        ];
        let selection = ToolSelection {
            shared: Some(vec!["read_file".to_string()]),
            private: Vec::new(),
            deny: Vec::new(),
        };
        let names: Vec<String> = filter_specs(specs, &selection)
            .into_iter()
            .map(|spec| spec["function"]["name"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(names, vec!["read_file", "tools_list"]);

        let specs = vec![serde_json::json!({"function": {"name": "tools_list"}})];
        let denied = ToolSelection {
            shared: None,
            private: Vec::new(),
            deny: vec!["tools.list".to_string()],
        };
        assert!(
            filter_specs(specs, &denied).is_empty(),
            "显式 deny 应当压过元工具例外"
        );
    }

    #[test]
    fn tool_failure_is_recorded_and_trimmed() {
        let mut output = NodeOutput::default();
        let long = "超时 ".repeat(200);
        record_failure(&mut output, "bin.jadx.decompile", &long);
        assert_eq!(output.failures.len(), 1);
        assert!(output.failures[0].starts_with("bin.jadx.decompile："));
        assert!(
            output.failures[0].chars().count() < FAILURE_TEXT_LIMIT + 40,
            "失败信息必须截断，否则一条就能吃掉下游预算"
        );

        // 同一个工具重复失败：合并计数，不刷屏。
        record_failure(&mut output, "bin.jadx.decompile", "又超时");
        record_failure(&mut output, "bin.jadx.decompile", "第三次");
        assert_eq!(output.failures.len(), 1);
        assert!(output.failures[0].contains("重复 3 次"), "{:?}", output.failures);

        // 不同工具各占一行；超出上限后只剩省略说明，但条数不再增长。
        for index in 0..20 {
            record_failure(&mut output, &format!("tool.{index}"), "失败");
        }
        assert_eq!(output.failures.len(), FAILURES_MAX);
        assert!(
            output.failures.last().unwrap().contains("未展开"),
            "超限后要给出省略数量：{:?}",
            output.failures.last()
        );
    }
}
