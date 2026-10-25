//! 外部引擎执行器：让 dsh / codex / opencode 也能当一个 DAG 节点的执行者。
//!
//! 定位
//! ----
//! [`NativeRunner`](super::runner::NativeRunner) 自己跑「模型 + 工具」的循环；
//! 外部引擎则是**另一个进程**在跑那个循环。所以这一层的活不是"调用模型"，而是
//! 「开会话 → 发提示词 → 从事件流里认领这一轮的文本 → 等终态」。
//!
//! 三条口径
//! --------
//! 1. **文本从事件流取**：`session.text.delta` 累加，`session.text.ended` 用整段
//!    覆盖（引擎自己知道这一条消息最终长什么样）。一轮里可能有多条助手消息
//!    （工具调用前后各一段），交付物取**最后一段非空文本**——那是这一轮真正的
//!    结论，前面那些是过程。
//! 2. **权限档不往下传**：dsh 的 sandbox、Codex 的 approval、OpenCode 的配置
//!    各有各的开关，`NodeRequest.permission` 只能约束原生引擎。这里不假装能
//!    透传——真要收紧，去改对应引擎自己的配置。
//! 3. **无人值守**：节点线程没人看着，引擎弹审批就没人点。遇到 `permission.asked`
//!    只把它记进节点步骤（运行面板上能看见"卡在等授权"），不自动放行。
//!
//! 会话不回收：一次节点 = 一个会话，留在引擎历史里可回溯（节点步骤里会写 id）。
//! 删掉反而让"这一轮到底跑了什么"再也查不到。

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::runner::{NodeOutput, NodeRequest, NodeRunner, StopHandle, PROMPT_NOTICE};
use crate::engines::engine::Engine;
use crate::engines::native::provider;

/// 等终态的最长时长。
///
/// `StopHandle` 只在计划里写了 `timeoutSecs` 时才有截止时间；没写就是"无限等"。
/// 引擎那边一旦哑掉（事件线程死了、子进程挂了却没报错），调度线程会永远
/// 挂着这个节点——所以这里必须有一条自己的硬上限。
const MAX_NODE: Duration = Duration::from_secs(30 * 60);

/// 事件轮询间隔：太短白烧 CPU，太长取消要等半天。
const POLL: Duration = Duration::from_millis(300);

/// 单轮文本最多留多少字符（引擎跑飞了也不会把内存吃光）。
const TEXT_CAP: usize = 400_000;

/// 交给外部引擎的提示词上限。
///
/// 外部引擎**没有重试的机会**：一次拒收，这个节点就废了（会话已经建好、
/// 提示词已经发出，改不了）。所以宁可在这里先截断，也不能让它带着一份
/// 装不进窗口的提示词出门。
///
/// 正常情况下 `resolve_task` 已经把上游报告压到 24 KB 以内，走到这里说明
/// 人设或任务原文本身就很长。头多留一点：任务目标与约束在最前面。
const PROMPT_CAP: usize = 60_000;
const PROMPT_HEAD: usize = 45_000;
const PROMPT_TAIL: usize = 12_000;

/// 一个引擎 + `NodeRunner` 的桥。
///
/// 只持有 `Arc<dyn Engine>`：会话创建、发消息、打断、事件订阅全在 trait 上，
/// 所以加一个引擎执行器不需要在这里加任何分支。
pub struct EngineRunner {
    engine: Arc<dyn Engine>,
}

impl EngineRunner {
    pub fn new(engine: Arc<dyn Engine>) -> Self {
        EngineRunner { engine }
    }
}

impl NodeRunner for EngineRunner {
    fn id(&self) -> &'static str {
        self.engine.id()
    }

    fn run(&self, request: NodeRequest) -> Result<NodeOutput, String> {
        // 先订阅再开会话：会话一建好引擎就可能开始推事件，晚一步会漏掉开头。
        let events = self.engine.subscribe();
        let created = self
            .engine
            .create_session(&request.directory)
            .map_err(|error| format!("{} 开会话失败：{error}", self.id()))?;
        let session = created.session_id;
        let prompt = compose_prompt(&request);

        if let Err(error) =
            self.engine
                .send_prompt_with_model(&session, &prompt, request.profile.model.as_deref())
        {
            return Err(format!("{} 发送提示词失败：{error}", self.id()));
        }

        let mut tally = TextTally::default();
        let mut steps = vec![format!("{} 会话 {session}", self.id())];
        // 扇入节点最常见：上游报告一多，提示词本身就很大。先把这个数字报出去，
        // 否则超限时只剩一句引擎原文，看不出起因。
        let prompt_chars = prompt.chars().count();
        if prompt_chars > PROMPT_NOTICE {
            let step = format!("提示词 {prompt_chars} 字（上游报告已按预算均分截断）");
            (request.on_step)(&step);
            steps.push(step);
        }
        let mut last_error: Option<String> = None;
        let mut waiting_permission = false;
        let started = Instant::now();

        loop {
            if request.stop.hit() {
                interrupt(&self.engine, &session);
                return Err(stop_message(&request.stop, &tally));
            }
            if started.elapsed() > MAX_NODE {
                interrupt(&self.engine, &session);
                return Err(format!(
                    "跑了 {} 分钟还没收口（已尝试打断）：{}",
                    MAX_NODE.as_secs() / 60,
                    tally.report().chars().take(200).collect::<String>()
                ));
            }

            match events.recv_timeout(POLL) {
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                // 事件源断开 = 这一轮不会再有任何终态了。别空转到硬上限。
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(format!(
                        "{} 的事件流断了，这一轮不会再有结果",
                        self.id()
                    ))
                }
                Ok(event) => {
                    if event.pointer("/data/sessionID").and_then(Value::as_str)
                        != Some(session.as_str())
                    {
                        continue;
                    }
                    match event.get("type").and_then(Value::as_str).unwrap_or_default() {
                        "session.text.delta" => {
                            if let Some(delta) = event.pointer("/data/delta").and_then(Value::as_str) {
                                tally.delta(&event, delta);
                                (request.on_text)(delta);
                            }
                        }
                        "session.text.ended" => {
                            if let Some(full) = event.pointer("/data/text").and_then(Value::as_str) {
                                tally.set(&event, full);
                            }
                        }
                        "session.tool.called" => {
                            if steps.len() < 24 {
                                steps.push(tool_step(&event));
                            }
                        }
                        "permission.asked" if !waiting_permission => {
                            // 只记一次：同一轮反复请求会把步骤刷满，反而看不清。
                            waiting_permission = true;
                            steps.push("等待授权（无人值守，没有自动放行）".to_string());
                        }
                        "session.step.failed" => {
                            last_error = error_text(event.pointer("/data/error"))
                                .or_else(|| Some("某一步失败".to_string()));
                        }
                        "session.execution.failed" => {
                            let error = error_text(event.pointer("/data/error"))
                                .or_else(|| last_error.clone())
                                .unwrap_or_else(|| "引擎报告执行失败".to_string());
                            // 上下文超限必须说人话：引擎原文是一串 "maximum context
                            // length ... tokens"，看不出是上游报告塞太多。
                            if provider::is_context_overflow(&error) {
                                return Err(overflow_message(&error));
                            }
                            return Err(error);
                        }
                        "session.execution.interrupted" => {
                            return Err("执行被引擎打断".to_string())
                        }
                        "session.execution.succeeded" => break,
                        _ => {}
                    }
                }
            }
        }

        let text = tally.report();
        if text.trim().is_empty() {
            return Err(last_error.unwrap_or_else(|| {
                "这一轮没有正文：引擎只调了工具，没有给出结论".to_string()
            }));
        }
        Ok(NodeOutput {
            text,
            // 外部引擎不回报 token 用量（各家协议都没有统一字段），留 0 让前端
            // 显示"—"，比编一个数好。
            tokens_in: 0,
            tokens_out: 0,
            tokens_cached: 0,
            turns: 1,
            steps,
        })
    }
}

fn interrupt(engine: &Arc<dyn Engine>, session: &str) {
    if let Err(error) = engine.interrupt(session) {
        eprintln!("[prism] 打断 {} 会话失败：{error}", engine.id());
    }
}

/// 取消 / 超时时的错误文案：带上已经拿到的正文长度，用户知道"白跑了多少"。
fn stop_message(stop: &StopHandle, tally: &TextTally) -> String {
    let got = tally.report();
    if got.trim().is_empty() {
        format!("{}（还没有正文）", stop.reason())
    } else {
        format!(
            "{}（已收到 {} 字正文，未作为结论采用）",
            stop.reason(),
            got.chars().count()
        )
    }
}

/// 发给引擎的提示词：Agent 的人设 + 节点任务 + 一条无人值守的边界说明。
///
/// 外部引擎没有"系统提示"入口（各家的字段名还不一样），所以人设直接拼在正文
/// 前面；末尾补一句环境事实，避免模型产出"请问你希望……"这种没法接的话。
fn compose_prompt(request: &NodeRequest) -> String {
    let mut text = String::new();
    let persona = request.profile.system_prompt.trim();
    if !persona.is_empty() {
        text.push_str(persona);
        text.push_str("\n\n---\n\n");
    }
    text.push_str(request.prompt.trim());
    text.push_str(
        "\n\n（你正在无人值守地执行编排图里的一个节点：没有人会回答你的追问，\
         需要的信息自己用工具去查；最后一段正文就是交给下游节点的结论。）",
    );
    cap_prompt(text)
}

/// 提示词超过 [`PROMPT_CAP`] 时截成「头 + 尾」。
///
/// 外部引擎一次拒收就废掉整个节点，没有像原生引擎那样"瘦身重试"的机会，
/// 所以只能事先保证它装得下。
fn cap_prompt(text: String) -> String {
    let count = text.chars().count();
    if count <= PROMPT_CAP {
        return text;
    }
    let head: String = text.chars().take(PROMPT_HEAD).collect();
    let tail: String = text.chars().skip(count - PROMPT_TAIL).collect();
    format!(
        "{head}\n\n…（提示词过长，中略 {} 字：上游报告的完整内容在上游制品里，\
         需要细节时按提示里给出的制品 id 去取）…\n\n{tail}",
        count - PROMPT_HEAD - PROMPT_TAIL
    )
}

/// 把引擎的原始超限报错翻译成「该怎么办」。
fn overflow_message(raw: &str) -> String {
    format!(
        "上下文超限：这一轮装不进模型窗口。编排里最常见的原因是上游报告/工具输出\
         太多——可以拆细扇入节点、让上游产出更短的结论，或换一个上下文更大的模型。\
         原始错误：{}",
        raw.chars().take(300).collect::<String>()
    )
}

/// 工具事件 → 节点步骤行（形如 `read_file src/main.rs`）。
fn tool_step(event: &Value) -> String {
    let name = event
        .pointer("/data/name")
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let Some(input) = event.pointer("/data/input") else {
        return name.to_string();
    };
    for key in ["command", "filePath", "path", "pattern", "query", "title"] {
        if let Some(value) = input.get(key).and_then(Value::as_str) {
            let value = value.trim();
            if !value.is_empty() {
                return format!("{name} {}", value.chars().take(60).collect::<String>());
            }
        }
    }
    name.to_string()
}

/// 引擎的错误字段可能是字符串，也可能是 `{ message }`（与前端归约口径一致）。
fn error_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
        Value::Object(map) => map
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

/// 一轮里所有助手文本，按出现顺序存；交付物取最后一段非空。
#[derive(Default)]
struct TextTally {
    parts: Vec<(String, String)>,
}

impl TextTally {
    /// 事件 → 稳定的部分 key。
    ///
    /// 用"消息 id + 序号"而不是固定 key：OpenCode 一轮里可能产出多条助手消息，
    /// 全塞进一个桶会把过程叙述和最终结论粘成一段。
    fn key(event: &Value) -> String {
        let message = event
            .pointer("/data/assistantMessageID")
            .and_then(Value::as_str)
            .unwrap_or("main");
        let ordinal = event
            .pointer("/data/ordinal")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        format!("{message}#{ordinal}")
    }

    fn slot(&mut self, key: &str) -> &mut String {
        if let Some(index) = self.parts.iter().position(|(id, _)| id == key) {
            return &mut self.parts[index].1;
        }
        self.parts.push((key.to_string(), String::new()));
        let last = self.parts.len() - 1;
        &mut self.parts[last].1
    }

    fn delta(&mut self, event: &Value, delta: &str) {
        let key = Self::key(event);
        let slot = self.slot(&key);
        if slot.chars().count() < TEXT_CAP {
            slot.push_str(delta);
        }
    }

    fn set(&mut self, event: &Value, full: &str) {
        let key = Self::key(event);
        let text: String = if full.chars().count() > TEXT_CAP {
            full.chars().take(TEXT_CAP).collect()
        } else {
            full.to_string()
        };
        *self.slot(&key) = text;
    }

    /// 交付物：最后一段非空文本（trim 过）。
    fn report(&self) -> String {
        self.parts
            .iter()
            .rev()
            .map(|(_, text)| text.trim())
            .find(|text| !text.is_empty())
            .unwrap_or_default()
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text_delta(message: &str, ordinal: u64, delta: &str) -> Value {
        json!({
            "type": "session.text.delta",
            "data": {
                "assistantMessageID": message,
                "ordinal": ordinal,
                "delta": delta,
            }
        })
    }

    #[test]
    fn deltas_of_one_message_accumulate() {
        let mut tally = TextTally::default();
        tally.delta(&text_delta("m1", 0, "第一段"), "第一段");
        tally.delta(&text_delta("m1", 0, "，第二段"), "，第二段");
        assert_eq!(tally.report(), "第一段，第二段");
    }

    #[test]
    fn last_message_wins() {
        let mut tally = TextTally::default();
        tally.delta(&text_delta("m1", 0, "我先看看代码。"), "我先看看代码。");
        tally.delta(&text_delta("m2", 0, "结论：没有问题。"), "结论：没有问题。");
        assert_eq!(tally.report(), "结论：没有问题。");
    }

    #[test]
    fn ended_event_replaces_partial_text() {
        let mut tally = TextTally::default();
        tally.delta(&text_delta("m1", 0, "半截"), "半截");
        tally.set(
            &json!({ "data": { "assistantMessageID": "m1", "ordinal": 0 } }),
            "完整的一段",
        );
        assert_eq!(tally.report(), "完整的一段");
    }

    #[test]
    fn blank_parts_are_skipped() {
        let mut tally = TextTally::default();
        tally.delta(&text_delta("m1", 0, "有内容"), "有内容");
        tally.delta(&text_delta("m2", 0, "   "), "   ");
        assert_eq!(tally.report(), "有内容");
    }

    #[test]
    fn tool_step_prefers_readable_field() {
        let event = json!({
            "type": "session.tool.called",
            "data": { "name": "read_file", "input": { "path": "src/main.rs" } }
        });
        assert_eq!(tool_step(&event), "read_file src/main.rs");
    }

    #[test]
    fn tool_step_without_input_is_just_the_name() {
        let event = json!({ "data": { "name": "list_files" } });
        assert_eq!(tool_step(&event), "list_files");
    }

    /// 外部引擎一次拒收就废掉节点，所以提示词必须自己先保证装得下。
    #[test]
    fn oversized_prompt_is_capped_head_and_tail() {
        let text = format!("{}{}", "甲".repeat(PROMPT_CAP), "乙".repeat(5_000));
        let capped = cap_prompt(text);
        assert!(capped.starts_with("甲甲"));
        assert!(capped.ends_with("乙乙"));
        assert!(capped.contains("提示词过长"));
        assert!(
            capped.chars().count() <= PROMPT_HEAD + PROMPT_TAIL + 200,
            "截完要真的落回上限：{}",
            capped.chars().count()
        );
    }

    #[test]
    fn a_prompt_within_the_cap_is_untouched() {
        let text = format!("{}{}", "甲".repeat(PROMPT_CAP - 1), "尾");
        assert_eq!(cap_prompt(text.clone()), text);
    }

    /// 引擎原文是一串 token 数字，用户看不懂起因在编排扇入。
    #[test]
    fn overflow_error_is_translated_into_actionable_advice() {
        let message = overflow_message(
            "This model's maximum context length is 128000 tokens. However, your messages resulted in 190000 tokens.",
        );
        assert!(message.contains("上下文超限"));
        assert!(message.contains("上游报告"));
        assert!(message.contains("maximum context length"), "原始错误要留证");
    }

    #[test]
    fn compose_prompt_keeps_persona_and_boundary() {
        use crate::agents::{AgentProfile, Origin, Permission, ToolSelection};

        let request = NodeRequest {
            run_id: "run".to_string(),
            dag_id: "dag-test".to_string(),
            node_id: "n1".to_string(),
            profile: AgentProfile {
                id: "reviewer".to_string(),
                name: "审查者".to_string(),
                description: String::new(),
                origin: Origin::Builtin,
                overrides_builtin: false,
                executor: "codex".to_string(),
                model: None,
                system_prompt: "你只挑毛病，不改代码。".to_string(),
                permission: Permission::ReadOnly,
                workspace: crate::agents::Workspace::Shared,
                tools: ToolSelection::default(),
                mcp: Vec::new(),
                tags: Vec::new(),
                link_dirs: Vec::new(),
                gate_default: false,
            },
            prompt: "审查 PR 147".to_string(),
            directory: "/tmp".to_string(),
            permission: Permission::ReadOnly,
            selection: ToolSelection::default(),
            stop: StopHandle::new(Arc::new(std::sync::atomic::AtomicBool::new(false)), None),
            on_text: Arc::new(|_: &str| {}),
            on_step: Arc::new(|_: &str| {}),
            on_usage: Arc::new(|_: i64, _: i64, _: i64| {}),
            on_turn: Arc::new(|_: u32, _: u32| {}),
        };
        let text = compose_prompt(&request);
        assert!(text.starts_with("你只挑毛病"));
        assert!(text.contains("审查 PR 147"));
        assert!(text.contains("无人值守"));
    }

    /// 真机验证：起一个引擎，让它跑一个节点，断言正文真的被收上来。
    ///
    /// `cargo test --lib -- --ignored runs_an_opencode_node`（会消耗额度，不走 CI）。
    /// 选 OpenCode 是因为它只需要本机 `opencode serve` + 已配置的模型；Codex 那条
    /// 路依赖它自己的本地模型代理，代理没跑时会在 `v1/responses` 上 502。
    #[test]
    #[ignore = "真机：会拉起 opencode serve 并消耗额度"]
    fn runs_an_opencode_node() {
        use crate::agents::{AgentProfile, Origin, Permission, ToolSelection};

        let runtime = Arc::new(crate::engines::opencode::runtime::Runtime::unattached());
        let runner = EngineRunner::new(runtime.clone());
        let dir = std::env::temp_dir().join(format!("prism-dag-oc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let seen = Arc::new(std::sync::Mutex::new(String::new()));
        let sink = seen.clone();

        let request = NodeRequest {
            run_id: "run-test".to_string(),
            dag_id: "dag-test".to_string(),
            node_id: "ping".to_string(),
            profile: AgentProfile {
                id: "ping".to_string(),
                name: "Ping".to_string(),
                description: String::new(),
                origin: Origin::Builtin,
                overrides_builtin: false,
                executor: "opencode".to_string(),
                model: None,
                system_prompt: String::new(),
                permission: Permission::ReadOnly,
                workspace: crate::agents::Workspace::Shared,
                tools: ToolSelection::default(),
                mcp: Vec::new(),
                tags: Vec::new(),
                link_dirs: Vec::new(),
                gate_default: false,
            },
            prompt: "只回复 PONG 四个字母，不要调用任何工具。".to_string(),
            directory: dir.display().to_string(),
            permission: Permission::ReadOnly,
            selection: ToolSelection::default(),
            // 3 分钟上限：真机上够用，卡住也不会把测试挂到天亮。
            stop: StopHandle::new(Arc::new(std::sync::atomic::AtomicBool::new(false)), Some(180)),
            on_text: Arc::new(move |delta: &str| {
                if let Ok(mut buffer) = sink.lock() {
                    buffer.push_str(delta);
                }
            }),
            on_step: Arc::new(|_: &str| {}),
            on_usage: Arc::new(|_: i64, _: i64, _: i64| {}),
            on_turn: Arc::new(|_: u32, _: u32| {}),
        };

        let output = runner.run(request).expect("opencode 节点应当跑通");
        println!("节点正文：{}", output.text);
        println!("节点步骤：{:?}", output.steps);
        let streamed = seen.lock().map(|text| text.clone()).unwrap_or_default();
        println!("流式增量：{streamed}");
        assert!(output.text.contains("PONG"), "正文不含 PONG：{}", output.text);
        assert!(
            output.steps
                .iter()
                .any(|step| step.starts_with("opencode 会话")),
            "步骤里应当带会话 id：{:?}",
            output.steps
        );
        runtime.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
