//! 原生引擎的 turn 循环：模型 → 工具 → 模型。
//!
//! 事件形状刻意对齐 `src/liveTurn.ts` 的归约约定（`session.text.delta` /
//! `session.tool.called` …），这样前端不需要为原生引擎加任何适配逻辑：
//! 流式过程走事件，落定后走 `read_timeline`。

use super::{config, provider, store, tools};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{AppHandle, Emitter};

pub const EVENT_NAME: &str = "engine-event";

/// 正在跑的回合：会话 id → 停止标记。
fn running() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    static RUNNING: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    RUNNING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 标记停手。返回 false 表示这个会话当前没在跑回合（已经结束了）。
pub fn cancel(session_id: &str) -> bool {
    let map = running().lock().expect("回合表锁");
    match map.get(session_id) {
        Some(flag) => {
            flag.store(true, Ordering::SeqCst);
            true
        }
        None => false,
    }
}

/// 占住这个会话的回合槽位，Drop 时自动释放——即使中途 panic 也不会把会话锁死。
struct RunSlot {
    session_id: String,
    flag: Arc<AtomicBool>,
}

impl RunSlot {
    fn take(session_id: &str) -> Result<Self, String> {
        let flag = Arc::new(AtomicBool::new(false));
        let mut map = running().lock().expect("回合表锁");
        if map.contains_key(session_id) {
            return Err("上一轮还没结束，等它跑完或点「停止」".to_string());
        }
        map.insert(session_id.to_string(), flag.clone());
        Ok(Self {
            session_id: session_id.to_string(),
            flag,
        })
    }

    fn stopped(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

impl Drop for RunSlot {
    fn drop(&mut self) {
        if let Ok(mut map) = running().lock() {
            map.remove(&self.session_id);
        }
    }
}


fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
}

fn new_message_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or(0);
    format!("m{nanos}")
}

fn emit(app: &AppHandle, event_type: &str, data: Value) {
    let _ = app.emit(EVENT_NAME, json!({ "type": event_type, "data": data }));
}

/// 系统提示：只讲规矩，不讲人格（省 token，也避免抢模型自己的行为习惯）。
fn system_prompt(directory: &str) -> String {
    format!(
        "你是 Prism 内置的编码 agent，工作目录是 {directory}。\n\
         规则：\n\
         1. 改文件前先用 read_file 读原文，edit_file 的 old_string 必须与原文逐字一致（含缩进）。\n\
         2. 需要事实时用 grep / list_files 查，不要凭印象编造文件内容。\n\
         3. 命令要短、可复现；不要执行需要交互输入的命令。\n\
         4. 回答简洁：先说结论，再给必要的改动说明。"
    )
}

fn assistant_value(message: &store::StoredMessage) -> Value {
    let mut value = json!({
        "role": "assistant",
        "content": message.text,
    });
    if !message.tool_calls.is_empty() {
        let calls: Vec<Value> = message
            .tool_calls
            .iter()
            .map(|call| {
                json!({
                    "id": call.id,
                    "type": "function",
                    "function": { "name": call.name, "arguments": call.arguments }
                })
            })
            .collect();
        value["tool_calls"] = Value::Array(calls);
    }
    value
}

fn tool_value(message: &store::StoredMessage, call_id: &str) -> Value {
    json!({
        "role": "tool",
        "tool_call_id": call_id,
        "content": message.text,
    })
}

fn owns_call(message: &store::StoredMessage, call_id: &str) -> bool {
    message
        .tool_calls
        .iter()
        .any(|call| call.id == call_id)
}

/// 这个工具结果是不是「本该排在前面的那条 assistant 还没出现」。
fn owned_later(history: &[store::StoredMessage], index: usize, call_id: &str) -> bool {
    history[index + 1..]
        .iter()
        .any(|item| item.role == "assistant" && owns_call(item, call_id))
}

/// 历史消息 → provider 的 messages 数组。
///
/// 早期版本把工具结果先于「带 tool_calls 的 assistant」落库，直接回放会被
/// OpenAI 兼容端判成孤立 tool 消息（400）。这里按 call_id 重新配对：先挂起
/// 暂时没有归属的工具结果，等对应的 assistant 出现时补在它后面；到用户
/// 消息还配不上的直接丢掉——宁可少一条工具结果，也别整轮请求失败。
///
/// 末尾还有一道 `drop_unanswered_tool_calls`：中断落库会留下「只有 tool_calls
/// 没有结果」的 assistant，那同样会让整轮请求 400。
fn provider_messages(directory: &str, history: &[store::StoredMessage]) -> Vec<Value> {
    let mut messages = vec![json!({ "role": "system", "content": system_prompt(directory) })];
    let mut orphans: Vec<&store::StoredMessage> = Vec::new();
    let mut paired: Vec<String> = Vec::new();

    for (index, message) in history.iter().enumerate() {
        match message.role.as_str() {
            "user" => {
                // 用户消息是硬边界：手里没配上的工具结果不可能再配上了。
                orphans.clear();
                paired.clear();
                messages.push(json!({ "role": "user", "content": message.text }));
            }
            "assistant" => {
                messages.push(assistant_value(message));
                for call in &message.tool_calls {
                    paired.push(call.id.clone());
                    let hit = orphans
                        .iter()
                        .position(|item| item.tool_call_id.as_deref() == Some(call.id.as_str()));
                    if let Some(position) = hit {
                        let tool = orphans.remove(position);
                        messages.push(tool_value(tool, &call.id));
                    }
                }
                // 剩下的挂起项归属别处，等后面的 assistant。
            }
            "tool" => {
                let Some(call_id) = message.tool_call_id.clone() else {
                    continue;
                };
                if owned_later(history, index, &call_id) {
                    orphans.push(message);
                } else if paired.iter().any(|id| id == &call_id) {
                    messages.push(tool_value(message, &call_id));
                }
                // 既不在前面也不在后面：孤立的工具结果，丢掉。
            }
            _ => {}
        }
    }
    drop_unanswered_tool_calls(&mut messages);
    messages
}

/// 结尾兜底：带 tool_calls 的 assistant 后面必须紧跟着每个 call 的工具结果
/// （OpenAI 兼容端硬要求）。缺哪个就从声明里摘掉哪个——摘掉只是少一段工具
/// 上下文，留着就是整轮请求发不出去、会话从此卡死。
///
/// 产生这种半截数据的情形：回复中断、进程被杀、停止正好卡在工具落库前后。
fn drop_unanswered_tool_calls(messages: &mut [Value]) {
    let mut index = 0;
    while index < messages.len() {
        if messages[index]["role"] != "assistant" {
            index += 1;
            continue;
        }
        let Some(calls) = messages[index]["tool_calls"].as_array().cloned() else {
            index += 1;
            continue;
        };
        // 工具结果只能紧跟在 assistant 后面，所以只看后面这一串。
        let mut cursor = index + 1;
        let mut answered: Vec<String> = Vec::new();
        while cursor < messages.len() && messages[cursor]["role"] == "tool" {
            if let Some(id) = messages[cursor]["tool_call_id"].as_str() {
                answered.push(id.to_string());
            }
            cursor += 1;
        }
        let kept: Vec<Value> = calls
            .into_iter()
            .filter(|call| {
                call["id"]
                    .as_str()
                    .map(|id| answered.iter().any(|item| item == id))
                    .unwrap_or(false)
            })
            .collect();
        if kept.is_empty() {
            if let Some(object) = messages[index].as_object_mut() {
                object.remove("tool_calls");
            }
        } else {
            messages[index]["tool_calls"] = Value::Array(kept);
        }
        index = cursor.max(index + 1);
    }
}

/// 工具卡摘要（与前端 `summarizeToolInput` 的取值顺序保持一致）。
fn summarize_input(input: &Value) -> Option<String> {
    const KEYS: [&str; 6] = ["command", "path", "pattern", "query", "url", "description"];
    for key in KEYS {
        if let Some(value) = input.get(key).and_then(Value::as_str) {
            let first = value.lines().next().unwrap_or("").trim();
            if !first.is_empty() {
                let mut out: String = first.chars().take(96).collect();
                if first.chars().count() > 96 {
                    out.push('…');
                }
                return Some(out);
            }
        }
    }
    None
}

/// 会话里指定的模型与服务优先；没指定就用设置里的默认模型。
fn resolve_pick(
    cfg: &config::NativeConfig,
    provider_id: Option<&str>,
    model_id: Option<&str>,
) -> Result<(config::ProviderConfig, String), String> {
    if let Some(id) = provider_id.map(str::trim).filter(|value| !value.is_empty()) {
        let provider = cfg
            .find(id)
            .ok_or_else(|| format!("模型服务不存在：{id}"))?;
        let model = match model_id.map(str::trim).filter(|value| !value.is_empty()) {
            Some(model) => provider
                .models
                .iter()
                .find(|item| item.id == model)
                .ok_or_else(|| format!("{} 下没有模型 {model}", provider.label))?,
            None => provider
                .models
                .first()
                .ok_or_else(|| format!("{} 还没勾选模型", provider.label))?,
        };
        return Ok((provider.clone(), model.id.clone()));
    }
    // 只给了模型 id：先找当前服务的，再找别家的同名模型。
    if let Some(model) = model_id.map(str::trim).filter(|value| !value.is_empty()) {
        let current = cfg
            .selection()
            .map(|(provider, _)| provider.id.as_str())
            .unwrap_or("");
        let hit = cfg
            .providers
            .iter()
            .find(|provider| {
                provider.id == current && provider.models.iter().any(|item| item.id == model)
            })
            .or_else(|| {
                cfg.providers
                    .iter()
                    .find(|provider| provider.models.iter().any(|item| item.id == model))
            });
        if let Some(provider) = hit {
            return Ok((provider.clone(), model.to_string()));
        }
    }
    let (provider, model) = cfg.selection().ok_or_else(|| {
        "原生引擎还没配置：打开 设置 → 模型管理，添加一个模型服务".to_string()
    })?;
    Ok((provider.clone(), model.id.clone()))
}

/// 发一条用户消息，跑完整轮。返回用户消息 id。
pub fn send(
    app: &AppHandle,
    session_id: &str,
    text: &str,
    provider_id: Option<&str>,
    model_id: Option<&str>,
) -> Result<String, String> {
    let raw = super::raw_session_id(session_id)
        .ok_or_else(|| format!("不是原生会话：{session_id}"))?;
    let meta = store::load_meta(raw).ok_or_else(|| "原生会话不存在".to_string())?;
    let cfg = config::load();
    let (provider, model) = resolve_pick(&cfg, provider_id, model_id)?;
    let directory = meta.directory.clone();
    let session_id = session_id.to_string();
    // 槽位先占住：同一会话不允许并发两轮。
    let slot = RunSlot::take(&session_id)?;

    let user_id = new_message_id();
    let user_message = store::StoredMessage {
        id: user_id.clone(),
        role: "user".to_string(),
        time: now_ms(),
        text: text.to_string(),
        parts: vec![store::StoredPart::Text { text: text.to_string() }],
        tool_calls: Vec::new(),
        tool_call_id: None,
        tokens: None,
    };
    store::append(raw, &user_message)?;

    emit(
        app,
        "session.execution.started",
        json!({ "sessionID": session_id }),
    );

    let outcome = run_turns(
        app,
        raw,
        &session_id,
        &directory,
        &provider,
        &model,
        cfg.max_turns(),
        &slot,
    );
    match outcome {
        Ok(Stop::Done) => {
            emit(
                app,
                "session.execution.succeeded",
                json!({ "sessionID": session_id }),
            );
            Ok(user_id)
        }
        Ok(Stop::Cancelled) => {
            emit(
                app,
                "session.execution.interrupted",
                json!({ "sessionID": session_id }),
            );
            Ok(user_id)
        }
        Err(error) => {
            emit(
                app,
                "session.execution.failed",
                json!({ "sessionID": session_id, "error": error }),
            );
            Err(error)
        }
    }
}

/// 一轮回合是怎么结束的：跑完 / 被叫停。
enum Stop {
    Done,
    Cancelled,
}

#[allow(clippy::too_many_arguments)]
fn run_turns(
    app: &AppHandle,
    raw: &str,
    session_id: &str,
    directory: &str,
    provider: &config::ProviderConfig,
    model: &str,
    max_turns: u32,
    slot: &RunSlot,
) -> Result<Stop, String> {
    let specs = tools::specs();
    // 历史只读一次：原来每轮都重读整份 messages.jsonl 再逐行解析，长会话
    // 走一个工具循环就是平方级开销。本轮自己写进去的消息同步补进来，
    // 下一轮看到的历史与磁盘一致。
    let mut history = store::messages(raw);

    for ordinal in 0..max_turns {
        if slot.stopped() {
            return Ok(Stop::Cancelled);
        }
        let messages = provider_messages(directory, &history);
        let assistant_id = new_message_id();

        // 流式增量直接推事件；assistantMessageID + ordinal 决定前端把 delta 拼到哪一段。
        let mut on_text = |delta: &str| {
            emit(
                app,
                "session.text.delta",
                json!({
                    "sessionID": session_id,
                    "assistantMessageID": assistant_id,
                    "ordinal": ordinal,
                    "delta": delta,
                }),
            );
        };
        let mut on_reasoning = |delta: &str| {
            emit(
                app,
                "session.reasoning.delta",
                json!({
                    "sessionID": session_id,
                    "assistantMessageID": assistant_id,
                    "ordinal": ordinal,
                    "delta": delta,
                }),
            );
        };

        let stop = || slot.stopped();
        let outcome = match provider::chat(
            provider,
            model,
            &messages,
            &specs,
            &mut on_text,
            &mut on_reasoning,
            &stop,
        ) {
            Ok(outcome) => outcome,
            Err(provider::ChatError::Cancelled) => return Ok(Stop::Cancelled),
            Err(error) => return Err(error.to_string()),
        };
        // 第一轮就记下模型：即使后面撞上轮次上限，回显也是对的。
        let _ = store::set_model(raw, model, &provider.id);

        if !outcome.text.is_empty() {
            emit(
                app,
                "session.text.ended",
                json!({
                    "sessionID": session_id,
                    "assistantMessageID": assistant_id,
                    "ordinal": ordinal,
                    "text": outcome.text.clone(),
                }),
            );
        }

        // 没有工具调用 = 这一轮就是最终答复。
        if outcome.tool_calls.is_empty() {
            let mut parts: Vec<store::StoredPart> = Vec::new();
            if !outcome.reasoning.is_empty() {
                parts.push(store::StoredPart::Reasoning {
                    text: outcome.reasoning.clone(),
                });
            }
            parts.push(store::StoredPart::Text {
                text: outcome.text.clone(),
            });
            let tokens = outcome.usage.map(|usage| store::StoredTokens {
                input: usage.input,
                output: usage.output,
            });
            store::append(
                raw,
                &store::StoredMessage {
                    id: assistant_id,
                    role: "assistant".to_string(),
                    time: now_ms(),
                    text: outcome.text,
                    parts,
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    tokens,
                },
            )?;
            return Ok(Stop::Done);
        }

        // 有工具调用：先执行，再把「助手消息 + 每个工具结果」落库，进入下一轮。
        let mut parts: Vec<store::StoredPart> = Vec::new();
        if !outcome.reasoning.is_empty() {
            parts.push(store::StoredPart::Reasoning {
                text: outcome.reasoning.clone(),
            });
        }
        if !outcome.text.trim().is_empty() {
            parts.push(store::StoredPart::Text {
                text: outcome.text.clone(),
            });
        }

        let mut tool_messages: Vec<store::StoredMessage> = Vec::new();
        for call in &outcome.tool_calls {
            if slot.stopped() {
                return Ok(Stop::Cancelled);
            }
            let input: Value = serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
            emit(
                app,
                "session.tool.input.started",
                json!({
                    "sessionID": session_id,
                    "id": call.id,
                    "name": call.name,
                    "assistantMessageID": assistant_id,
                }),
            );
            emit(
                app,
                "session.tool.called",
                json!({
                    "sessionID": session_id,
                    "id": call.id,
                    "name": call.name,
                    "input": input,
                    "assistantMessageID": assistant_id,
                }),
            );

            let result = tools::execute_with_stop(directory, &call.name, &call.arguments, &|| {
                slot.stopped()
            });
            emit(
                app,
                "session.tool.success",
                json!({
                    "sessionID": session_id,
                    "id": call.id,
                    "content": [{ "type": "text", "text": result.text }],
                    "metadata": { "error": result.error.is_some() },
                }),
            );

            parts.push(store::StoredPart::Tool {
                name: call.name.clone(),
                call_id: call.id.clone(),
                status: if result.error.is_some() { "error" } else { "completed" }.to_string(),
                summary: summarize_input(&input),
                input: if input.is_null() { None } else { Some(input) },
                output: Some(result.text.clone()),
                error: result.error.clone(),
            });

            tool_messages.push(store::StoredMessage {
                id: new_message_id(),
                role: "tool".to_string(),
                time: now_ms(),
                text: result.text,
                parts: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: Some(call.id.clone()),
                tokens: None,
            });
        }

        let tokens = outcome.usage.map(|usage| store::StoredTokens {
            input: usage.input,
            output: usage.output,
        });
        // 顺序要紧：OpenAI 兼容协议要求带 tool_calls 的 assistant 排在它的
        // tool 结果之前，否则下一轮请求会被判成孤立 tool 消息（400）。
        let assistant = store::StoredMessage {
            id: assistant_id,
            role: "assistant".to_string(),
            time: now_ms(),
            text: outcome.text,
            parts,
            tool_calls: outcome
                .tool_calls
                .iter()
                .map(|call| store::StoredToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                })
                .collect(),
            tool_call_id: None,
            tokens,
        };
        store::append(raw, &assistant)?;
        for message in &tool_messages {
            store::append(raw, message)?;
        }
        // 落库成功之后才更新本地视图：下一轮直接用，不再重读整份历史。
        history.push(assistant);
        history.extend(tool_messages);
    }

    Err(format!("超过 {max_turns} 轮仍未收敛，已停止"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: &str, text: &str) -> store::StoredMessage {
        store::StoredMessage {
            id: "m1".to_string(),
            role: role.to_string(),
            time: 0,
            text: text.to_string(),
            parts: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tokens: None,
        }
    }

    #[test]
    fn provider_messages_start_with_system_prompt() {
        let list = provider_messages("/tmp/demo", &[message("user", "hi")]);
        assert_eq!(list[0]["role"], "system");
        assert!(list[0]["content"].as_str().unwrap().contains("/tmp/demo"));
        assert_eq!(list[1]["role"], "user");
    }

    fn assistant_with_call(id: &str) -> store::StoredMessage {
        let mut assistant = message("assistant", "");
        assistant.id = format!("a-{id}");
        assistant.tool_calls = vec![store::StoredToolCall {
            id: id.to_string(),
            name: "grep".to_string(),
            arguments: "{}".to_string(),
        }];
        assistant
    }

    fn tool_result(id: &str) -> store::StoredMessage {
        let mut tool = message("tool", "ok");
        tool.id = format!("t-{id}");
        tool.tool_call_id = Some(id.to_string());
        tool
    }

    /// 正常顺序：assistant(tool_calls) → tool。
    #[test]
    fn tool_messages_follow_their_assistant() {
        let history = vec![
            message("user", "查一下"),
            assistant_with_call("c9"),
            tool_result("c9"),
        ];
        let list = provider_messages("/tmp/demo", &history);
        assert_eq!(list.len(), 4);
        assert_eq!(list[2]["role"], "assistant");
        assert_eq!(list[3]["role"], "tool");
        assert_eq!(list[3]["tool_call_id"], "c9");
    }

    /// 旧数据把 tool 先落了库：回放时要挪到 assistant 后面，否则协议报 400。
    #[test]
    fn orphan_tool_before_assistant_is_reordered() {
        let history = vec![
            message("user", "查一下"),
            tool_result("c1"),
            assistant_with_call("c1"),
        ];
        let list = provider_messages("/tmp/demo", &history);
        let roles: Vec<&str> = list[1..]
            .iter()
            .map(|item| item["role"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(roles, ["user", "assistant", "tool"], "工具结果要跟在其归属之后");
        assert_eq!(list[3]["tool_call_id"], "c1");
    }

    /// 完全没有归属的工具结果必须丢掉，否则整轮请求都会 400。
    #[test]
    fn orphan_tool_without_owner_is_dropped() {
        let history = vec![message("user", "查一下"), tool_result("gone")];
        let list = provider_messages("/tmp/demo", &history);
        assert_eq!(list.len(), 2, "只剩 system + user");
    }

    /// 带结果的 tool_calls 要原样回放（工具上下文靠它接上）。
    #[test]
    fn assistant_tool_calls_are_replayed() {
        let history = vec![
            message("user", "查一下"),
            assistant_with_call("c1"),
            tool_result("c1"),
        ];
        let list = provider_messages("/tmp/demo", &history);
        let calls = list[2]["tool_calls"].as_array().expect("应有 tool_calls");
        assert_eq!(calls[0]["function"]["name"], "grep");
        assert_eq!(calls[0]["id"], "c1");
        assert_eq!(list[3]["tool_call_id"], "c1");
    }

    /// 只有 tool_calls、没有工具结果（回复中断 / 落库半截）时必须摘掉声明：
    /// 留着它 OpenAI 兼容端会整轮 400，这个会话从此发不出消息。
    #[test]
    fn unanswered_tool_calls_are_dropped() {
        let history = vec![message("user", "查一下"), assistant_with_call("c1")];
        let list = provider_messages("/tmp/demo", &history);
        assert_eq!(list.len(), 3);
        assert!(
            list[2]["tool_calls"].as_array().is_none(),
            "没有工具结果的 tool_calls 必须摘掉：{}",
            list[2]
        );
    }

    /// 两个 call 只答了一个：有结果的那个照常回放，另一个摘掉。
    #[test]
    fn only_answered_tool_calls_survive() {
        let mut assistant = assistant_with_call("c1");
        assistant.tool_calls.push(store::StoredToolCall {
            id: "c2".to_string(),
            name: "read_file".to_string(),
            arguments: "{}".to_string(),
        });
        let history = vec![message("user", "查一下"), assistant, tool_result("c1")];
        let list = provider_messages("/tmp/demo", &history);
        let calls = list[2]["tool_calls"].as_array().expect("应保留 c1");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["id"], "c1");
    }

    /// 用户消息把配对切断后：前面那个 assistant 的 tool_calls 要摘掉，
    /// 落单的工具结果也要丢掉——只丢结果、留下无应答的声明照样 400。
    #[test]
    fn tool_calls_split_by_user_message_are_dropped() {
        let history = vec![
            message("user", "第一步"),
            assistant_with_call("c1"),
            message("user", "算了"),
            tool_result("c1"),
        ];
        let list = provider_messages("/tmp/demo", &history);
        let roles: Vec<&str> = list[1..]
            .iter()
            .map(|item| item["role"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(roles, ["user", "assistant", "user"]);
        assert!(list[2]["tool_calls"].as_array().is_none(), "{}", list[2]);
    }

    #[test]
    fn tool_summary_prefers_command_then_path() {
        assert_eq!(
            summarize_input(&json!({ "command": "cargo test", "path": "a.rs" })),
            Some("cargo test".to_string())
        );
        assert_eq!(
            summarize_input(&json!({ "path": "src/main.rs" })),
            Some("src/main.rs".to_string())
        );
        assert_eq!(summarize_input(&json!({})), None);
    }

    #[test]
    fn run_slot_blocks_double_send_and_releases() {
        let slot = RunSlot::take("native:slot-test").expect("第一个能占住");
        assert!(
            RunSlot::take("native:slot-test").is_err(),
            "同一会话不允许并发两轮"
        );
        assert!(!slot.stopped());
        assert!(cancel("native:slot-test"), "在跑的会话应当能被叫停");
        assert!(slot.stopped(), "停止标记要传到跑着的那一轮");
        drop(slot);
        assert!(!cancel("native:slot-test"), "跑完之后再叫停应当返回 false");
        let again = RunSlot::take("native:slot-test").expect("释放后可以再跑");
        drop(again);
    }
}
