//! 手机回合：把一条来自手机的消息变成一轮引擎会话，并把过程广播成带序号的
//! 事件流（长轮询按 `since` 续传，手机端断网重连也不丢过程）。
//!
//! 口径与 [`crate::routines::runner`] 一致：先订阅再开会话（晚一步会漏开头），
//! 事件按 `data.sessionID` 认领，终态 succeeded / failed / interrupted，超时
//! 先打断再收口。文本按「最后一段非空正文是结论」归集（同 DAG 执行器）。
//! 权限请求不阻塞等待：转发到手机页与微信 webhook，用户不答复就等超时兜底。

use std::collections::VecDeque;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use super::push::{self, PushKind};
use super::PhoneLinkState;
use crate::engines::codex::runtime::Runtime as CodexRuntime;
use crate::engines::engine::EngineRegistry;
use crate::engines::opencode::runtime::Runtime as OpencodeRuntime;
use crate::engines::opencode::search::prism_dir;

/// 单轮兜底时长；超时先打断再按 timeout 收口（与定时任务同限）。
const TURN_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// 事件轮询间隔。
const POLL: Duration = Duration::from_millis(500);
/// 事件缓冲上限：超过就丢最旧的（seq 只增不减，客户端用 latest 续传）。
const BUFFER_CAP: usize = 500;
/// 单段正文容量上限（引擎跑飞也不吃光内存）。
const TEXT_CAP: usize = 200_000;
/// 手机会话的工作区（数据主权：不动用户项目目录）。
const WORKSPACE_DIR: &str = "phone-workspace";

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
}

/// 带序号的事件环形缓冲：长轮询的续传依据。
#[derive(Default)]
pub struct EventBuffer {
    items: VecDeque<(u64, Value)>,
    next_seq: u64,
}

impl EventBuffer {
    /// 追加一条事件，返回它的 seq。
    pub fn push(&mut self, kind: &str, data: Value) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.items.push_back((seq, json!({ "seq": seq, "type": kind, "data": data })));
        while self.items.len() > BUFFER_CAP {
            self.items.pop_front();
        }
        seq
    }

    /// 取 seq > since 的全部事件。
    pub fn since(&self, since: u64) -> Vec<Value> {
        self.items
            .iter()
            .filter(|(seq, _)| *seq > since)
            .map(|(_, value)| value.clone())
            .collect()
    }

    pub fn latest(&self) -> u64 {
        self.next_seq.saturating_sub(1)
    }
}

/// 手机工作区目录（`~/.prism/phone-workspace`，需要时创建）。
fn workspace_dir() -> Result<String, String> {
    let dir = prism_dir()
        .ok_or_else(|| "无法定位 ~/.prism 数据目录".to_string())?
        .join(WORKSPACE_DIR);
    std::fs::create_dir_all(&dir).map_err(|error| format!("创建手机工作区失败：{error}"))?;
    Ok(dir.to_string_lossy().into_owned())
}

/// 跑一轮（后台线程）。同一时刻只允许一轮，忙时直接报错。
pub fn run_turn(app: &AppHandle, text: String) -> Result<(), String> {
    let Some(state) = app.try_state::<PhoneLinkState>() else {
        return Err("手机链接插件未启用".to_string());
    };
    if !state.try_begin_turn() {
        return Err("上一条消息还没跑完，稍等它收口".to_string());
    }
    let app = app.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || {
        let outcome = execute(&app, &text);
        if let Some(state) = app.try_state::<PhoneLinkState>() {
            state.end_turn(outcome.session_id);
        }
    });
    Ok(())
}

struct TurnOutcome {
    status: String,
    session_id: Option<String>,
    error: Option<String>,
    text: String,
}

fn execute(app: &AppHandle, text: &str) -> TurnOutcome {
    let mut outcome = TurnOutcome {
        status: "error".to_string(),
        session_id: None,
        error: None,
        text: String::new(),
    };
    let (config, existing_session) = {
        let Some(state) = app.try_state::<PhoneLinkState>() else {
            outcome.error = Some("手机链接插件未启用".to_string());
            return outcome;
        };
        (state.config(), state.current_session())
    };
    let directory = match workspace_dir() {
        Ok(directory) => directory,
        Err(error) => {
            outcome.error = Some(error);
            finish_turn(app, &config, &outcome);
            return outcome;
        }
    };
    // 引擎从注册表取（同 routines）：不按名字 match，注册表里没有就直接报。
    let engine = {
        let Some(registry) = app.try_state::<Mutex<EngineRegistry>>() else {
            outcome.error = Some("引擎插件未装配，手机会话无法开工".to_string());
            finish_turn(app, &config, &outcome);
            return outcome;
        };
        let guard = match registry.lock() {
            Ok(guard) => guard,
            Err(error) => {
                outcome.error = Some(format!("引擎注册表不可用：{error}"));
                finish_turn(app, &config, &outcome);
                return outcome;
            }
        };
        guard.get(&config.engine)
    };
    let Some(engine) = engine else {
        outcome.error = Some(format!("引擎（{}）未就绪，先在桌面端确认它可用", config.engine));
        finish_turn(app, &config, &outcome);
        return outcome;
    };

    // 先订阅再开会话：会话一建好引擎就可能开始推事件。
    let events = engine.subscribe();
    let session_id = match existing_session {
        // 继续已有会话：同一部手机的对话保持上下文。
        Some(session_id) => {
            outcome.session_id = Some(session_id.clone());
            session_id
        }
        None => match engine.create_session(&directory) {
            Ok(created) => {
                outcome.session_id = Some(created.session_id.clone());
                created.session_id
            }
            Err(error) => {
                outcome.error = Some(format!("新建会话失败：{error}"));
                finish_turn(app, &config, &outcome);
                return outcome;
            }
        },
    };
    if let Err(error) = engine.send_prompt(&session_id, text) {
        outcome.error = Some(format!("发送消息失败：{error}"));
        finish_turn(app, &config, &outcome);
        return outcome;
    }
    push_event(app, "status", json!({ "running": true }));

    let terminal = wait_terminal(app, &config, &session_id, events, &mut outcome, &engine);
    outcome.status = match terminal {
        Terminal::Succeeded => "succeeded".to_string(),
        Terminal::Failed(error) => {
            outcome.error = Some(error);
            "failed".to_string()
        }
        Terminal::Interrupted => "interrupted".to_string(),
        Terminal::Timeout => {
            outcome.error = Some("等待收口超时（已尝试打断）".to_string());
            "timeout".to_string()
        }
    };
    finish_turn(app, &config, &outcome);
    outcome
}

enum Terminal {
    Succeeded,
    Failed(String),
    Interrupted,
    Timeout,
}

/// 等本轮收口；过程事件实时进缓冲（手机长轮询与 webhook 共用）。
fn wait_terminal(
    app: &AppHandle,
    config: &super::store::PhoneLinkConfig,
    session_id: &str,
    events: mpsc::Receiver<Value>,
    outcome: &mut TurnOutcome,
    engine: &Arc<dyn crate::engines::engine::Engine>,
) -> Terminal {
    let deadline = Instant::now() + TURN_TIMEOUT;
    let mut tally = TextTally::default();
    while Instant::now() < deadline {
        let event = match events.recv_timeout(POLL) {
            Ok(event) => event,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if event.pointer("/data/sessionID").and_then(Value::as_str) != Some(session_id) {
            continue;
        }
        match event.get("type").and_then(Value::as_str).unwrap_or_default() {
            "session.text.delta" => {
                if let Some(delta) = event.pointer("/data/delta").and_then(Value::as_str) {
                    tally.delta(&event, delta);
                    push_event(app, "delta", json!({ "text": delta }));
                }
            }
            "session.text.ended" => {
                if let Some(full) = event.pointer("/data/text").and_then(Value::as_str) {
                    tally.set(&event, full);
                }
            }
            "session.tool.called" => {
                push_event(app, "tool", json!({ "name": tool_step(&event) }));
            }
            "permission.asked" => {
                let request_id = event
                    .pointer("/data/id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let title = event
                    .pointer("/data/title")
                    .and_then(Value::as_str)
                    .or_else(|| event.pointer("/data/tool").and_then(Value::as_str))
                    .unwrap_or("权限请求")
                    .to_string();
                push_event(
                    app,
                    "permission",
                    json!({ "id": request_id, "title": title }),
                );
                if config.webhook_url.is_some()
                    && push::should_push(&config.events, PushKind::Permission, None)
                {
                    push_webhook(config, PushKind::Permission, move |engine| {
                        push::permission_message(engine, &title)
                    });
                }
            }
            "session.execution.succeeded" => {
                outcome.text = tally.report();
                return Terminal::Succeeded;
            }
            "session.execution.interrupted" => {
                outcome.text = tally.report();
                return Terminal::Interrupted;
            }
            "session.execution.failed" => {
                outcome.text = tally.report();
                return Terminal::Failed(
                    error_text(event.pointer("/data/error"))
                        .unwrap_or_else(|| "引擎报告执行失败".to_string()),
                );
            }
            _ => {}
        }
    }
    if let Err(error) = engine.interrupt(session_id) {
        eprintln!("[prism] 手机回合超时打断失败：{error}");
    }
    outcome.text = tally.report();
    Terminal::Timeout
}

/// 收口：终态进缓冲 → webhook 推送 → 广播桌面事件。
fn finish_turn(app: &AppHandle, config: &super::store::PhoneLinkConfig, outcome: &TurnOutcome) {
    push_event(
        app,
        "turn_end",
        json!({
            "status": outcome.status,
            "text": outcome.text,
            "error": outcome.error,
        }),
    );
    match outcome.status.as_str() {
        "succeeded" | "interrupted" => {
            if push::should_push(&config.events, PushKind::Done, Some(&outcome.status)) {
                let status = outcome.status.clone();
                let text = outcome.text.clone();
                push_webhook(config, PushKind::Done, move |engine| {
                    push::done_message(engine, &status, &text)
                });
            }
        }
        _ => {
            let error = outcome.error.clone().unwrap_or_else(|| outcome.status.clone());
            if push::should_push(&config.events, PushKind::Error, None) {
                push_webhook(config, PushKind::Error, move |engine| {
                    push::error_message(engine, &error)
                });
            }
        }
    }
    let _ = app.emit(
        "phone-link-event",
        json!({
            "kind": "turn",
            "status": outcome.status,
            "sessionId": outcome.session_id,
            "error": outcome.error,
        }),
    );
}

/// 手机页对权限请求的答复：按会话所属引擎转发到对应 runtime。
pub fn reply_permission(app: &AppHandle, request_id: &str, decision: &str) -> Result<(), String> {
    let Some(state) = app.try_state::<PhoneLinkState>() else {
        return Err("手机链接插件未启用".to_string());
    };
    let (session_id, engine) = {
        let guard = state.lock();
        let Some(device_session) = guard.chat.session_id.clone() else {
            return Err("当前没有进行中的手机会话".to_string());
        };
        (device_session, guard.config.engine.clone())
    };
    match engine.as_str() {
        "codex" => {
            let Some(runtime) = app.try_state::<CodexRuntime>() else {
                return Err("Codex 引擎未启用".to_string());
            };
            runtime.inner().permission_reply(&session_id, request_id, decision)
        }
        _ => {
            let Some(runtime) = app.try_state::<OpencodeRuntime>() else {
                return Err("OpenCode 引擎未启用".to_string());
            };
            runtime.inner().permission_reply(&session_id, request_id, decision)
        }
    }
}

/// 打断当前回合（手机页的「停止」按钮）。
pub fn interrupt(app: &AppHandle) -> Result<(), String> {
    let Some(state) = app.try_state::<PhoneLinkState>() else {
        return Err("手机链接插件未启用".to_string());
    };
    let (session_id, engine) = {
        let Some(session_id) = state.current_session() else {
            return Err("当前没有进行中的手机会话".to_string());
        };
        (session_id, state.config().engine)
    };
    let registry = app
        .try_state::<Mutex<EngineRegistry>>()
        .ok_or_else(|| "引擎插件未装配".to_string())?;
    let target = registry
        .lock()
        .map_err(|error| format!("引擎注册表不可用：{error}"))?
        .get(&engine)
        .ok_or_else(|| format!("引擎（{engine}）未就绪"))?;
    target.interrupt(&session_id)
}

/// 追加事件进缓冲；`phone-link-event` 只广播给桌面端做状态刷新，失败静默。
fn push_event(app: &AppHandle, kind: &str, data: Value) {
    if let Some(state) = app.try_state::<PhoneLinkState>() {
        state.push_chat_event(kind, data);
    }
}

/// 发送 webhook：独立线程 + owned 数据（webhook 最多挂 10s，不能拖住回合路径）。
fn push_webhook(
    config: &super::store::PhoneLinkConfig,
    kind: PushKind,
    compose: impl FnOnce(&str) -> String + Send + 'static,
) {
    let Some(url) = config.webhook_url.clone() else {
        return;
    };
    let engine = config.engine.clone();
    let _ = std::thread::spawn(move || {
        if let Err(error) = push::send(&url, &compose(&engine)) {
            eprintln!("[prism] 手机链接推送失败（{kind:?}）：{error}");
        }
    });
}

/// 工具事件 → 可读的一行（形如 `read_file src/main.rs`）。
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

/// 引擎的错误字段可能是字符串或 `{ message }`。
fn error_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
        Value::Object(map) => map.get("message").and_then(Value::as_str).map(str::to_string),
        _ => None,
    }
}

/// 一轮里所有助手文本，按出现顺序存；交付物取最后一段非空。
#[derive(Default)]
struct TextTally {
    parts: Vec<(String, String)>,
}

impl TextTally {
    /// 事件 → 稳定的部分 key（消息 id + 序号），多段叙述不粘成一段。
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

    #[test]
    fn buffer_assigns_increasing_seq_and_supports_since() {
        let mut buffer = EventBuffer::default();
        let a = buffer.push("status", json!({ "running": true }));
        let b = buffer.push("delta", json!({ "text": "你好" }));
        assert_eq!(buffer.latest(), b);
        assert!(a < b);
        let events = buffer.since(a);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "delta");
        // latest 之后没有新事件。
        assert!(buffer.since(b).is_empty());
    }

    #[test]
    fn buffer_drops_oldest_beyond_cap() {
        let mut buffer = EventBuffer::default();
        for i in 0..(BUFFER_CAP + 50) {
            buffer.push("delta", json!({ "i": i }));
        }
        assert_eq!(buffer.items.len(), BUFFER_CAP);
        // 最旧的 seq 已被丢弃：since 只能取到现存部分。
        let oldest_kept = buffer.items.front().unwrap().0;
        assert_eq!(buffer.since(oldest_kept - 1).len(), BUFFER_CAP);
    }

    #[test]
    fn tally_last_nonempty_message_wins() {
        let event = |id: &str| json!({ "data": { "assistantMessageID": id, "ordinal": 0 } });
        let mut tally = TextTally::default();
        tally.delta(&event("m1"), "我先看看代码。");
        tally.delta(&event("m2"), "结论：没有问题。");
        assert_eq!(tally.report(), "结论：没有问题。");
        tally.set(&event("m2"), "结论：更新后的说法。");
        assert_eq!(tally.report(), "结论：更新后的说法。");
    }

    #[test]
    fn tool_step_prefers_readable_field() {
        let event = json!({
            "type": "session.tool.called",
            "data": { "name": "read_file", "input": { "path": "src/main.rs" } }
        });
        assert_eq!(tool_step(&event), "read_file src/main.rs");
        assert_eq!(tool_step(&json!({ "data": { "name": "list_files" } })), "list_files");
    }

    #[test]
    fn error_text_reads_string_or_message() {
        assert_eq!(error_text(Some(&json!("炸了"))).as_deref(), Some("炸了"));
        assert_eq!(
            error_text(Some(&json!({ "message": "超时" }))).as_deref(),
            Some("超时")
        );
        assert_eq!(error_text(None), None);
    }
}
