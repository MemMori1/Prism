//! 原生引擎的 turn 循环：模型 → 工具 → 模型。
//!
//! 事件形状刻意对齐 `src/liveTurn.ts` 的归约约定（`session.text.delta` /
//! `session.tool.called` …），这样前端不需要为原生引擎加任何适配逻辑：
//! 流式过程走事件，落定后走 `read_timeline`。

use super::{config, provider, store, tools};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

pub const EVENT_NAME: &str = "engine-event";

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

/// 历史消息 → provider 的 messages 数组。
fn provider_messages(directory: &str, history: &[store::StoredMessage]) -> Vec<Value> {
    let mut messages = vec![json!({ "role": "system", "content": system_prompt(directory) })];
    for message in history {
        match message.role.as_str() {
            "user" => messages.push(json!({ "role": "user", "content": message.text })),
            "assistant" => {
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
                messages.push(value);
            }
            "tool" => {
                let Some(call_id) = message.tool_call_id.clone() else {
                    continue;
                };
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": message.text,
                }));
            }
            _ => {}
        }
    }
    messages
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
    );
    match outcome {
        Ok(()) => {
            emit(
                app,
                "session.execution.succeeded",
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

fn run_turns(
    app: &AppHandle,
    raw: &str,
    session_id: &str,
    directory: &str,
    provider: &config::ProviderConfig,
    model: &str,
    max_turns: u32,
) -> Result<(), String> {
    let specs = tools::specs();

    for ordinal in 0..max_turns {
        let history = store::messages(raw);
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

        let outcome = provider::chat(
            provider,
            model,
            &messages,
            &specs,
            &mut on_text,
            &mut on_reasoning,
        )?;
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
            return Ok(());
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

        for call in &outcome.tool_calls {
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

            let result = tools::execute(directory, &call.name, &call.arguments);
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

            store::append(
                raw,
                &store::StoredMessage {
                    id: new_message_id(),
                    role: "tool".to_string(),
                    time: now_ms(),
                    text: result.text,
                    parts: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: Some(call.id.clone()),
                    tokens: None,
                },
            )?;
        }

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
            },
        )?;
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

    #[test]
    fn tool_messages_keep_call_id() {
        let mut tool = message("tool", "ok");
        tool.tool_call_id = Some("c9".to_string());
        let list = provider_messages("/tmp/demo", &[tool]);
        assert_eq!(list[1]["role"], "tool");
        assert_eq!(list[1]["tool_call_id"], "c9");
    }

    #[test]
    fn assistant_tool_calls_are_replayed() {
        let mut assistant = message("assistant", "查一下");
        assistant.tool_calls = vec![store::StoredToolCall {
            id: "c1".to_string(),
            name: "grep".to_string(),
            arguments: r#"{"pattern":"x"}"#.to_string(),
        }];
        let list = provider_messages("/tmp/demo", &[assistant]);
        let calls = list[1]["tool_calls"].as_array().expect("应有 tool_calls");
        assert_eq!(calls[0]["function"]["name"], "grep");
        assert_eq!(calls[0]["id"], "c1");
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
}
