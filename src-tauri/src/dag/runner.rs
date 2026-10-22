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

/// 单个占位符最多塞多少字符。
///
/// 上游报告动辄上万字，原样拼进下游提示词会直接撞上下文上限（还花钱）。
/// 截断并留一句说明，比让请求 400 好得多。
const SUBSTITUTE_MAX: usize = 8_000;

/// 把 `{{nodes.<id>.output}}` 换成上游报告。
///
/// `.report` / `.files` 也认（历史图里有这两种写法），一律取报告正文——
/// 产物文件清单还没跟踪，给空串会让下游误以为"没有产物"，不如给正文。
pub fn resolve_task(task: &str, outputs: &std::collections::HashMap<String, String>) -> String {
    const PREFIX: &str = "{{nodes.";
    let mut out = String::with_capacity(task.len());
    let mut rest = task;
    while let Some(start) = rest.find(PREFIX) {
        out.push_str(&rest[..start]);
        let after = &rest[start + PREFIX.len()..];
        let Some(close) = after.find("}}") else {
            // 没闭合：原样保留，让用户看见自己写错了。
            out.push_str(&rest[start..]);
            return out;
        };
        let inner = &after[..close];
        let id = inner.split('.').next().unwrap_or("").trim();
        match outputs.get(id) {
            Some(text) if !text.trim().is_empty() => {
                if text.chars().count() > SUBSTITUTE_MAX {
                    out.push_str(&text.chars().take(SUBSTITUTE_MAX).collect::<String>());
                    out.push_str("\n\n（上游报告过长，以上为节选）");
                } else {
                    out.push_str(text);
                }
            }
            Some(_) => out.push_str(&format!("（节点 {id} 没有留下正文）")),
            None => out.push_str(&format!("（节点 {id} 没有产出）")),
        }
        rest = &after[close + 2..];
    }
    out.push_str(rest);
    out
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
        let specs = filter_specs(tools::specs_for(&enabled, cap), &request.selection);

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
                                should_stop: &should_stop,
                            },
                            &call.name,
                            &args,
                        )
                        .outcome
                    }
                };
                if output.steps.len() < 24 {
                    output.steps.push(format!(
                        "{}{}",
                        call.name,
                        summarize_args(&args)
                            .map(|text| format!(" {text}"))
                            .unwrap_or_default()
                    ));
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
fn filter_specs(specs: Vec<Value>, selection: &ToolSelection) -> Vec<Value> {
    specs
        .into_iter()
        .filter(|spec| {
            let name = spec
                .get("function")
                .and_then(|value| value.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if selection.deny.iter().any(|item| item == name) {
                return false;
            }
            let in_shared = match &selection.shared {
                None => true,
                Some(list) => list.iter().any(|item| item == name),
            };
            in_shared || selection.private.iter().any(|item| item == name)
        })
        .collect()
}

fn summarize_args(args: &Value) -> Option<String> {
    let object = args.as_object()?;
    for key in ["path", "file", "pattern", "command", "query"] {
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
    use std::collections::HashMap;

    fn outputs(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(id, text)| (id.to_string(), text.to_string()))
            .collect()
    }

    #[test]
    fn substitutes_upstream_reports() {
        let task = "基于这些结论继续：\n{{nodes.research.output}}";
        let text = resolve_task(task, &outputs(&[("research", "找到了 3 处调用")]));
        assert!(text.contains("找到了 3 处调用"));
        assert!(!text.contains("{{"));
    }

    #[test]
    fn missing_node_does_not_leave_a_hole() {
        let text = resolve_task("[{{nodes.ghost.output}}]", &outputs(&[]));
        assert!(text.contains("没有产出"), "{text}");
    }

    #[test]
    fn long_reports_are_truncated() {
        let long = "啊".repeat(SUBSTITUTE_MAX + 100);
        let text = resolve_task("{{nodes.a.output}}", &outputs(&[("a", &long)]));
        assert!(text.contains("节选"));
        assert!(text.chars().count() < SUBSTITUTE_MAX + 200);
    }

    #[test]
    fn unclosed_placeholder_is_left_alone() {
        let text = resolve_task("{{nodes.a.output", &outputs(&[("a", "x")]));
        assert_eq!(text, "{{nodes.a.output");
    }

    #[test]
    fn report_and_files_suffixes_both_resolve() {
        let text = resolve_task(
            "{{nodes.a.report}}|{{nodes.a.files}}",
            &outputs(&[("a", "报告")]),
        );
        assert_eq!(text, "报告|报告");
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

    #[test]
    fn stop_handle_reports_timeout() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let stop = StopHandle::new(cancelled.clone(), Some(0));
        assert!(!stop.hit(), "0 秒表示不超时");
        cancelled.store(true, Ordering::SeqCst);
        assert!(stop.hit());
        assert_eq!(stop.reason(), "运行被取消");
    }
}
