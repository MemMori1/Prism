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
    pub on_usage: Arc<dyn Fn(i64, i64) + Send + Sync>,
    /// 进入第几轮（1 起）+ 轮次上限。模型久不吐字时，面板靠它说明"还在第几轮"。
    pub on_turn: Arc<dyn Fn(u32, u32) + Send + Sync>,
}

/// 一次节点执行的产出。
#[derive(Debug, Default, Clone)]
pub struct NodeOutput {
    pub text: String,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub turns: u32,
    pub steps: Vec<String>,
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
            out.push_str(&node_text(id, outputs, artifacts));
        } else {
            let mut parts = inner.split('.');
            let alias = parts.next().unwrap_or("").trim();
            let field = parts.next().map(str::trim).unwrap_or("summary");
            out.push_str(&artifact_text(alias, if field.is_empty() { "summary" } else { field }, artifacts));
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
) -> String {
    let Some(text) = outputs.get(id) else {
        return format!("（节点 {id} 没有产出）");
    };
    if text.trim().is_empty() {
        return format!("（节点 {id} 没有留下正文）");
    }
    let artifact = artifacts.get(id).filter(|view| !view.id.is_empty());
    if text.chars().count() <= PROMPT_INLINE_LIMIT {
        return text.clone();
    }
    let mut out: String = text.chars().take(PROMPT_INLINE_LIMIT).collect();
    match artifact {
        Some(view) => out.push_str(&format!(
            "\n\n（上游报告过长，以上为前 {PROMPT_INLINE_LIMIT} 字。完整内容见制品 {}，\
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
            let mut out = view.summary.clone();
            if view.truncated {
                out.push_str(&format!(
                    "\n\n（摘要截到 {} 字；全文见制品 {}）",
                    super::artifact::SUMMARY_LIMIT,
                    if view.id.is_empty() { "（不可用）" } else { &view.id }
                ));
            }
            out
        }
        other => format!("（制品占位符不支持 .{other}）"),
    }
}

// ---------------------------------------------------------------- 原生执行器

/// 用 Prism 原生引擎（provider + 内置工具）跑一个节点。
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
        let max_turns = cfg.max_turns();
        let stop = request.stop.clone();
        let should_stop = move || stop.hit();

        for turn in 0..max_turns {
            if should_stop() {
                return Err(stop_error(&request.stop, output));
            }
            // 最后一轮不给工具：逼它把已知信息写成报告。否则"跑满轮次"的节点
            // 会带着一串半成品工具结果结束，下游什么也接不到。
            let final_turn = turn + 1 == max_turns;
            // 先报轮次再发请求：请求要等几秒到几分钟，这段时间面板至少要有个
            // "第 N/M 轮"在走，而不是干等一句"运行中"。
            (request.on_turn)(turn + 1, max_turns);
            let turn_specs: &[Value] = if final_turn { &[] } else { &specs };
            if final_turn {
                messages.push(serde_json::json!({
                    "role": "user",
                    "content": "轮次要用完了：不要再调用工具，直接把结论写成报告。",
                }));
            }
            let mut on_text = |delta: &str| (request.on_text)(delta);
            let mut on_reasoning = |_delta: &str| {};
            let chat = provider::chat(
                &provider_cfg,
                &model,
                &messages,
                turn_specs,
                &mut on_text,
                &mut on_reasoning,
                &should_stop,
            )
            .map_err(|error| {
                if request.stop.hit() {
                    stop_error(&request.stop, output.clone())
                } else {
                    error.to_string()
                }
            })?;
            if let Some(usage) = chat.usage {
                output.tokens_in += usage.input;
                output.tokens_out += usage.output;
                // 逐轮报出去：一个节点可能要跑十轮，等结束才显示数字就是十分钟空白。
                (request.on_usage)(usage.input, usage.output);
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
                // 无人值守：只读档的写类动作直接拒，其余放行（见文件头说明）。
                let denied = match (request.permission, crate::engines::native::approval::action_of(&call.name)) {
                    (Permission::ReadOnly, Some(action)) => Some(format!(
                        "节点权限是「只读」：{action} 被拦下了"
                    )),
                    _ => None,
                };
                let result = match denied {
                    Some(reason) => tools::ToolOutcome::fail(reason),
                    None => {
                        tools::run_with_args(
                            &tools::ToolContext {
                                app: self.app.as_ref(),
                                session_id: &format!("dag:{}:{}", request.run_id, request.node_id),
                                directory: &request.directory,
                                call_id: &call.id,
                                provider: Some(&provider_cfg),
                                model: Some(&model),
                                permission: Some(permission_str(request.permission)),
                                enabled: &enabled,
                                dag: Some(&scope),
                                should_stop: &should_stop,
                            },
                            &call.name,
                            &args,
                        )
                        .outcome
                    }
                };
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
                    "content": result.text,
                }));
            }
        }

        // 跑到这里说明轮次用尽仍没收敛。有正文就当归档交出去（比整场失败有用），
        // 但标清"是截断的"。
        if output.text.trim().is_empty() {
            return Err(format!(
                "跑满 {} 轮仍没有给出结论",
                max_turns
            ));
        }
        Ok(output)
    }
}

fn stop_error(stop: &StopHandle, output: NodeOutput) -> String {
    let mut text = format!("{}（已跑 {} 轮）", stop.reason(), output.turns);
    if !output.text.trim().is_empty() {
        text.push_str("，保留了部分正文");
    }
    text
}

fn permission_str(permission: Permission) -> &'static str {
    match permission {
        Permission::ReadOnly => "read-only",
        _ => "full",
    }
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
/// `shared: None` 表示"该档权限下的全部公用工具"，不是"没有工具"——这个
/// 区别在 `agents::ToolSelection` 那里已经讲清，这里只是照着执行。
/// 元工具：回答"我现在有什么能力"，因此**不参与白名单裁剪**。
///
/// 白名单是给具体工种配能力的；自省工具被裁掉，模型就失去在被裁剪之后自查
/// 的手段——恰好是最需要它的时候用不上。`deny` 仍然优先（显式禁就是禁）。
const META_TOOLS: &[&str] = &["tools_list"];

fn filter_specs(specs: Vec<Value>, selection: &ToolSelection) -> Vec<Value> {
    specs
        .into_iter()
        .filter(|spec| {
            let name = spec
                .get("function")
                .and_then(|value| value.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            // schema 里是 wire 名（`bin_meta_identify`），白名单里是内部名
            // （`bin.meta.identify`）——两边都压平再比，否则带点号的工具会被
            // 白名单全部筛掉：模型看不到工具，节点只会回一句"我做不到"。
            let listed = |item: &String| tools::wire_name(item) == name;
            if selection.deny.iter().any(listed) {
                return false;
            }
            if META_TOOLS.contains(&name) {
                return true;
            }
            let in_shared = match &selection.shared {
                None => true,
                Some(list) => list.iter().any(listed),
            };
            in_shared || selection.private.iter().any(listed)
        })
        .collect()
}

fn summarize_args(args: &Value) -> Option<String> {
    let object = args.as_object()?;
    for key in ["path", "file", "pattern", "command", "query", "url"] {
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
}
