//! dsh 运行轴：一个 ACP 连接 + 事件翻译。
//!
//! 形状对齐 opencode / codex 那两条运行轴：`Runtime` 是 `Arc<Inner>` 的克隆句柄，
//! 事件同时走两条路——Tauri 的 `engine-event`（前端）和进程内订阅（例行任务、测试）。
//!
//! 翻译表（ACP `session/update` → Prism 事件）：
//!
//! | ACP | Prism |
//! |---|---|
//! | `agent_message_chunk` | `session.text.delta` |
//! | `agent_thought_chunk` | `session.reasoning.delta` |
//! | `tool_call`（pending/in_progress） | `session.tool.input.started` + `session.tool.called` |
//! | `tool_call_update`（completed/failed） | `session.tool.success` / `.failed` |
//! | `session/request_permission` | `permission.asked`（回复时回 ACP 响应） |
//! | `session/prompt` 的 `stopReason` | `session.execution.succeeded` / `.interrupted` / `.failed` |
//!
//! 只翻这些：ACP 明确不提供 plan / 标题 / 终端视图，Prism 也不假装有。

use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use super::acp::{AcpClient, Incoming};
use super::locator;

/// 与 opencode / codex 共用的前端事件名。
pub const EVENT_NAME: &str = "engine-event";

/// `initialize` 的超时：首次启动要装配 profile（可能装插件依赖），给足时间。
const INIT_TIMEOUT: Duration = Duration::from_secs(120);
/// 控制类请求（建会话、列会话）的超时。
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

struct PendingPermission {
    /// ACP 反过来的请求 id，回复时要带回。
    request_id: Value,
    /// `once` / `always` / `reject` → ACP 的 optionId。
    options: HashMap<&'static str, String>,
    session_id: String,
}

struct RuntimeInner {
    app: Option<AppHandle>,
    subscribers: Mutex<Vec<Sender<Value>>>,
    client: Mutex<Option<Arc<AcpClient>>>,
    permissions: Mutex<HashMap<String, PendingPermission>>,
    /// `session/new` 回来的 `configOptions`（模型 + 推理档位）。
    ///
    /// ACP 没有"列模型"的方法：可选模型只在会话的配置里露出来。所以第一次拿到
    /// 就缓存住——设置面板和组合器每次打开都问一遍，不能每次都建会话。
    config_options: Mutex<Option<Value>>,
}

impl RuntimeInner {
    fn broadcast(&self, value: &Value) {
        if let Some(app) = &self.app {
            let _ = app.emit(EVENT_NAME, value.clone());
        }
        let mut subscribers = self
            .subscribers
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        subscribers.retain(|sender| sender.send(value.clone()).is_ok());
    }
}

/// 运行轴句柄：Tauri 管理为单例，克隆后跨线程使用。
#[derive(Clone)]
pub struct Runtime {
    inner: Arc<RuntimeInner>,
}

impl Runtime {
    pub fn new(app: Option<AppHandle>) -> Self {
        Runtime {
            inner: Arc::new(RuntimeInner {
                app,
                subscribers: Mutex::new(Vec::new()),
                client: Mutex::new(None),
                permissions: Mutex::new(HashMap::new()),
                config_options: Mutex::new(None),
            }),
        }
    }

    /// 进程内订阅（例行任务执行器与测试用；前端走 Tauri 事件）。
    pub fn subscribe(&self) -> mpsc::Receiver<Value> {
        let (sender, receiver) = mpsc::channel();
        self.inner
            .subscribers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(sender);
        receiver
    }

    /// 懒启动 ACP 进程并握手。
    ///
    /// 失败信息要能直接给用户看：没装 dsh 与 profile 装不起来是两种完全不同的
    /// 处置方式，所以这里把底层错误原样带出去，不吞成一句"连接失败"。
    pub fn ensure_client(&self) -> Result<Arc<AcpClient>, String> {
        {
            let slot = self
                .inner
                .client
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(client) = slot.as_ref() {
                if client.is_alive() {
                    return Ok(client.clone());
                }
            }
        }

        let client = AcpClient::spawn(locator::home().as_deref())?;
        let handshake = client
            .call(
                "initialize",
                json!({
                    "protocolVersion": super::acp::PROTOCOL_VERSION,
                    // Prism 不接管文件系统与终端：工具都在 dsh 进程里跑，
                    // 声明不支持比假装支持更安全（ACP 会据此避开这些扩展）。
                    "clientCapabilities": {
                        "fs": { "readTextFile": false, "writeTextFile": false },
                        "terminal": false,
                    },
                }),
                INIT_TIMEOUT,
            )
            .map_err(|error| {
                // 这一段是"dsh 起不来"最常被撞见的地方：home 目录权限、profile
                // 首次装配、Node 版本，全在这一步暴露。原样带出去，别压成
                // 一句"连接失败"——用户拿着 `EACCES ... /profiles/acp` 才能自救。
                format!("dsh 初始化失败（`{error}`）")
            })?;
        let _ = handshake;

        if let Some(incoming) = client.take_incoming() {
            let inner = self.inner.clone();
            let client_for_pump = client.clone();
            thread::Builder::new()
                .name("prism-dsh-events".into())
                .spawn(move || pump(inner, client_for_pump, incoming))
                .map_err(|cause| format!("启动 dsh 事件线程失败：{cause}"))?;
        }

        let mut slot = self
            .inner
            .client
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *slot = Some(client.clone());
        Ok(client)
    }

    /// 在指定目录新建会话，返回 **Prism 侧**的会话 id（`dsh:<uuid>`）。
    ///
    /// ACP 给的是裸 UUID，和 codex 的线程 UUID 长得一样；不包前缀的话，上层
    /// 拿 id 判引擎会判错。
    pub fn create_session(&self, directory: &str) -> Result<String, String> {
        let client = self.ensure_client()?;
        let result = client.call(
            "session/new",
            json!({
                "cwd": directory,
                // 外部 MCP 先不挂：ACP 的 MCP 声明意味着把命令与环境授权给
                // 外部进程，Prism 这边还没做对应的信任界面。
                "mcpServers": [],
            }),
            CALL_TIMEOUT,
        )?;
        let raw = result
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| "dsh 没有返回会话 id".to_string())?
            .to_string();
        // 顺手收下可选模型/推理档位：这是 ACP 唯一透露它们的地方。
        if let Some(options) = result.get("configOptions").filter(|value| value.is_array()) {
            *self
                .inner
                .config_options
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(options.clone());
        }
        let session_id = super::session_id_of(&raw);
        Ok(session_id)
    }

    /// 可选模型 + 推理档位（ACP 的 `configOptions`）。
    ///
    /// 缓存为空时退一步：拿这个目录下最近的一个历史会话去 `session/resume`，
    /// 顺带把配置读回来。比"为了列模型先建一个空会话"干净——建会话会在
    /// `$DSH_HOME/sessions` 里留一份日志，用户会看到一堆空对话。
    pub fn config_options(&self, directory: &str) -> Result<Option<Value>, String> {
        if let Some(cached) = self
            .inner
            .config_options
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
        {
            return Ok(Some(cached));
        }

        let Some(meta) = super::history::scan(50)
            .into_iter()
            .find(|meta| meta.cwd == directory)
        else {
            // 这个目录还没有任何 dsh 会话：模型列表天然不存在，交给上层降级。
            return Ok(None);
        };

        let client = self.ensure_client()?;
        let resumed = client.call(
            "session/resume",
            json!({ "sessionId": meta.id, "cwd": meta.cwd, "mcpServers": [] }),
            CALL_TIMEOUT,
        )?;
        let options = resumed
            .get("configOptions")
            .filter(|value| value.is_array())
            .cloned();
        if let Some(options) = &options {
            *self
                .inner
                .config_options
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(options.clone());
        }
        Ok(options)
    }

    /// 改会话的某一项配置（`model` / `reasoning_effort`）。
    pub fn set_config_option(
        &self,
        session_id: &str,
        config_id: &str,
        value: &str,
    ) -> Result<(), String> {
        let client = self.ensure_client()?;
        let raw = super::raw_session_id(session_id)
            .ok_or_else(|| format!("非法的 dsh 会话 id：{session_id}"))?;
        client.call(
            "session/set_config_option",
            json!({ "sessionId": raw, "configId": config_id, "value": value }),
            CALL_TIMEOUT,
        )?;
        Ok(())
    }

    /// 发一条提示词。返回本地生成的用户消息 id（前端据此做乐观插入）。
    ///
    /// `session/prompt` 是长请求，要等到整个回合结束才返回，所以放到独立线程里
    /// 等，主流程只负责把 `execution.started` 先发出去。
    pub fn send_prompt(&self, session_id: &str, text: &str) -> Result<String, String> {
        let client = self.ensure_client()?;
        let raw = super::raw_session_id(session_id)
            .ok_or_else(|| format!("非法的 dsh 会话 id：{session_id}"))?
            .to_string();
        let message_id = format!("user-{}", now_millis());

        self.inner.broadcast(&json!({
            "type": "session.execution.started",
            "data": { "sessionID": session_id },
        }));

        let inner = self.inner.clone();
        let session = session_id.to_string();
        let prompt = text.to_string();
        let acp_session = raw;
        thread::Builder::new()
            .name("prism-dsh-prompt".into())
            .spawn(move || {
                let outcome = client.send_request(
                    "session/prompt",
                    json!({
                        "sessionId": acp_session,
                        "prompt": [{ "type": "text", "text": prompt }],
                    }),
                );
                let (key, receiver) = match outcome {
                    Ok(pair) => pair,
                    Err(error) => {
                        inner.broadcast(&json!({
                            "type": "session.execution.failed",
                            "data": { "sessionID": session, "error": error },
                        }));
                        return;
                    }
                };
                let _ = key;
                match receiver.recv() {
                    Ok(Ok(result)) => {
                        let stop = result
                            .get("stopReason")
                            .and_then(Value::as_str)
                            .unwrap_or("end_turn");
                        let event = match stop {
                            "cancelled" => json!({
                                "type": "session.execution.interrupted",
                                "data": { "sessionID": session },
                            }),
                            "refusal" | "max_tokens" => json!({
                                "type": "session.execution.failed",
                                "data": {
                                    "sessionID": session,
                                    "error": if stop == "refusal" { "模型拒绝了这次请求" } else { "达到最大 token" },
                                },
                            }),
                            _ => json!({
                                "type": "session.execution.succeeded",
                                "data": { "sessionID": session },
                            }),
                        };
                        inner.broadcast(&event);
                    }
                    Ok(Err(error)) => {
                        inner.broadcast(&json!({
                            "type": "session.execution.failed",
                            "data": { "sessionID": session, "error": error },
                        }));
                    }
                    Err(_) => {
                        inner.broadcast(&json!({
                            "type": "session.execution.failed",
                            "data": { "sessionID": session, "error": "dsh 连接中断" },
                        }));
                    }
                }
            })
            .map_err(|cause| format!("启动 dsh 回合线程失败：{cause}"))?;

        Ok(message_id)
    }

    /// 发一条提示词，并在发送前应用组合器选的模型/推理档位。
    ///
    /// dsh 的 `session/prompt` 不带模型参数：模型是会话级配置，要先
    /// `session/set_config_option` 改掉再发。改失败不中断这一轮——用着默认模型
    /// 回一句，比把消息吞掉好。
    pub fn send_prompt_with(
        &self,
        session_id: &str,
        text: &str,
        model: Option<&str>,
        variant: Option<&str>,
    ) -> Result<String, String> {
        if let Some(variant) = variant.map(str::trim).filter(|value| !value.is_empty()) {
            if let Err(error) = self.set_config_option(session_id, "reasoning_effort", variant) {
                eprintln!("[prism] dsh 设置推理档位失败：{error}");
            }
        }
        if let Some(model) = model.map(str::trim).filter(|value| !value.is_empty()) {
            let value = self.model_value(model).unwrap_or_else(|| {
                // 缓存里找不到（比如从没建过会话）：按 provider/model 猜一个。
                match model.split_once('/') {
                    Some((provider, id)) => json!([provider, id]).to_string(),
                    None => json!(["deepseek-official", model]).to_string(),
                }
            });
            if let Err(error) = self.set_config_option(session_id, "model", &value) {
                eprintln!("[prism] dsh 设置模型失败：{error}");
            }
        }
        self.send_prompt(session_id, text)
    }

    /// 把"模型 id"翻回 ACP 的 `configOptions` 取值（`["provider","model"]` 的字符串）。
    fn model_value(&self, model: &str) -> Option<String> {
        let cached = self
            .inner
            .config_options
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()?;
        let options = cached
            .as_array()?
            .iter()
            .find(|item| item.get("id").and_then(Value::as_str) == Some("model"))?;
        for group in options.get("options")?.as_array()? {
            // 带分组（按 provider）和扁平两种形状都要认。
            let candidates: Vec<&Value> = match group.get("options").and_then(Value::as_array) {
                Some(items) => items.iter().collect(),
                None => vec![group],
            };
            for candidate in candidates {
                let name = candidate
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if name == model {
                    return candidate
                        .get("value")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                }
            }
        }
        None
    }

    /// 打断当前回合。ACP 的 cancel 是通知，不等响应。
    pub fn interrupt(&self, session_id: &str) -> Result<(), String> {
        let client = self.ensure_client()?;
        let raw = super::raw_session_id(session_id)
            .ok_or_else(|| format!("非法的 dsh 会话 id：{session_id}"))?;
        client.notify("session/cancel", json!({ "sessionId": raw }))
    }

    /// 回应权限询问。`decision` 取 `once` / `always` / `reject`。
    pub fn permission_reply(
        &self,
        session_id: &str,
        request_id: &str,
        decision: &str,
    ) -> Result<(), String> {
        if !matches!(decision, "once" | "always" | "reject") {
            return Err(format!("未知的授权决定：{decision}"));
        }
        let pending = self
            .inner
            .permissions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(request_id);

        let Some(pending) = pending else {
            // 已经答过（或引擎自己撤了）：不当作错误，避免前端重试时报红。
            return Ok(());
        };

        let client = self.ensure_client()?;
        // ACP 的选项按 kind 分：allow_once / allow_always / reject_once / reject_always。
        let kind = match decision {
            "once" => "allow_once",
            "always" => "allow_always",
            _ => "reject_once",
        };
        let option = pending
            .options
            .get(match decision {
                "once" => "once",
                "always" => "always",
                _ => "reject",
            })
            .cloned()
            .or_else(|| {
                // 服务端没给这一档（比如只提供 allow_once）：退回同侧任意一档，
                // 总比把整轮卡死好。
                pending.options.get(kind).cloned()
            });

        let result = match option {
            Some(option_id) => json!({
                "outcome": { "outcome": "selected", "optionId": option_id }
            }),
            None => json!({ "outcome": { "outcome": "cancelled" } }),
        };
        client.respond(&pending.request_id, Ok(result))?;

        self.inner.broadcast(&json!({
            "type": if decision == "reject" { "permission.rejected" } else { "permission.replied" },
            "data": { "sessionID": session_id, "requestID": request_id },
        }));
        Ok(())
    }

    pub fn shutdown(&self) {
        let client = self
            .inner
            .client
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(client) = client {
            client.shutdown();
        }
    }
}

/// ACP 的 `sessionId` 是裸 UUID；事件出门前统一包上 Prism 的前缀。
///
/// 不包的话，前端拿 `event.data.sessionID` 和当前会话 id（`dsh:<uuid>`）比对
/// 永远不相等，表现就是"消息发出去了但界面没反应"。
fn prefix_session(mut event: Value) -> Value {
    let Some(raw) = event
        .pointer("/data/sessionID")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return event;
    };
    if let Some(data) = event.get_mut("data").and_then(Value::as_object_mut) {
        data.insert("sessionID".to_string(), json!(super::session_id_of(&raw)));
    }
    event
}

/// 事件泵：把 ACP 帧翻译成 Prism 事件。
fn pump(inner: Arc<RuntimeInner>, client: Arc<AcpClient>, incoming: mpsc::Receiver<Incoming>) {
    for frame in incoming {
        match frame {
            Incoming::Notification { method, params } => {
                if method == "session/update" {
                    for event in translate_update(&params) {
                        inner.broadcast(&prefix_session(event));
                    }
                }
            }
            Incoming::Request { id, method, params } => {
                if method == "session/request_permission" {
                    ask_permission(&inner, id, &params);
                } else {
                    // 不认识的反向请求必须回一个错误，否则对面会一直等。
                    let _ = client.respond(
                        &id,
                        Err(
                            json!({ "code": -32601, "message": format!("未实现的方法：{method}") }),
                        ),
                    );
                }
            }
            Incoming::Closed { reason } => {
                // 进程没了：挂起的权限请求全部作废，前端才能收起卡片。
                let drained = std::mem::take(
                    &mut *inner
                        .permissions
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()),
                );
                for (request_id, pending) in drained {
                    inner.broadcast(&json!({
                        "type": "permission.rejected",
                        "data": { "sessionID": pending.session_id, "requestID": request_id },
                    }));
                }
                if !reason.is_empty() {
                    eprintln!("[prism] dsh 连接关闭：{reason}");
                }
                break;
            }
        }
    }
}

/// 登记 ACP 的权限请求并广播卡片。
fn ask_permission(inner: &Arc<RuntimeInner>, request_id: Value, params: &Value) {
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .map(super::session_id_of)
        .unwrap_or_default();
    let tool_call = params.get("toolCall").cloned().unwrap_or(Value::Null);
    let title = tool_call
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("外部工具调用");

    // ACP 的选项带 kind，这里按 kind 建映射，回复时用它找 optionId。
    let mut options: HashMap<&'static str, String> = HashMap::new();
    if let Some(list) = params.get("options").and_then(Value::as_array) {
        for option in list {
            let Some(option_id) = option.get("optionId").and_then(Value::as_str) else {
                continue;
            };
            match option.get("kind").and_then(Value::as_str) {
                Some("allow_once") => options.insert("once", option_id.to_string()),
                Some("allow_always") => options.insert("always", option_id.to_string()),
                Some("reject_once") | Some("reject_always") => {
                    options.insert("reject", option_id.to_string())
                }
                _ => None,
            };
        }
    }

    let request_id_text = request_id.to_string();
    inner
        .permissions
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(
            request_id_text.clone(),
            PendingPermission {
                request_id,
                options,
                session_id: session_id.clone(),
            },
        );

    // 资源行：ACP 的 toolCall 用 locations 描述"动了哪些文件"。
    let resources: Vec<String> = tool_call
        .get("locations")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.get("path")
                        .and_then(Value::as_str)
                        .or_else(|| item.get("uri").and_then(Value::as_str))
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();

    inner.broadcast(&json!({
        "type": "permission.asked",
        "data": {
            "id": request_id_text,
            "sessionID": session_id,
            "action": "mcp",
            "resources": resources,
            "message": title,
            "source": {
                "id": tool_call.get("toolCallId").cloned().unwrap_or(Value::Null),
                "messageID": Value::Null,
            },
        },
    }));
}

/// ACP 的一条 `session/update` → 零到多条 Prism 事件。
pub fn translate_update(params: &Value) -> Vec<Value> {
    let Some(session) = params.get("sessionId").and_then(Value::as_str) else {
        return Vec::new();
    };
    let Some(update) = params.get("update") else {
        return Vec::new();
    };
    let kind = update
        .get("sessionUpdate")
        .and_then(Value::as_str)
        .unwrap_or_default();

    match kind {
        "agent_message_chunk" => {
            let text = content_text(update.get("content"));
            if text.is_empty() {
                return Vec::new();
            }
            vec![json!({
                "type": "session.text.delta",
                "data": {
                    "sessionID": session,
                    "assistantMessageID": "agent",
                    "ordinal": 0,
                    "delta": text,
                },
            })]
        }
        "agent_thought_chunk" => {
            let text = content_text(update.get("content"));
            if text.is_empty() {
                return Vec::new();
            }
            vec![json!({
                "type": "session.reasoning.delta",
                "data": {
                    "sessionID": session,
                    "assistantMessageID": "reasoning",
                    "ordinal": 0,
                    "delta": text,
                },
            })]
        }
        "tool_call" => {
            let id = update
                .get("toolCallId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let title = update
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("工具调用");
            let name = tool_name(update);
            vec![
                json!({
                    "type": "session.tool.input.started",
                    "data": {
                        "sessionID": session,
                        "id": id,
                        "name": name,
                        "assistantMessageID": id,
                    },
                }),
                json!({
                    "type": "session.tool.called",
                    "data": {
                        "sessionID": session,
                        "id": id,
                        "assistantMessageID": id,
                        "input": {
                            "title": title,
                            "kind": update.get("kind").cloned().unwrap_or(Value::Null),
                        },
                    },
                }),
            ]
        }
        "tool_call_update" => {
            let id = update
                .get("toolCallId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let status = update
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending");
            let output = tool_output(update);
            let content = if output.is_empty() {
                json!([])
            } else {
                json!([{ "type": "text", "text": output }])
            };
            match status {
                "completed" => vec![json!({
                    "type": "session.tool.success",
                    "data": {
                        "sessionID": session,
                        "id": id,
                        "assistantMessageID": id,
                        "content": content,
                        "metadata": {},
                    },
                })],
                "failed" => vec![json!({
                    "type": "session.tool.failed",
                    "data": {
                        "sessionID": session,
                        "id": id,
                        "assistantMessageID": id,
                        "content": content,
                    },
                })],
                // pending / in_progress 已经在 tool_call 里发过卡，这里不重复。
                _ => Vec::new(),
            }
        }
        // plan / available_commands_update / current_mode_update / usage_update：
        // ACP 会发，但 Prism 目前没有对应的展示位，安静丢掉好过塞进事件流。
        _ => Vec::new(),
    }
}

/// ACP 的 content 是 `{type:"text", text}` 或块数组。
fn content_text(content: Option<&Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    if let Some(text) = content.get("text").and_then(Value::as_str) {
        return text.to_string();
    }
    content
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

/// 工具名：ACP 没有稳定的工具名，用 `kind`（read / edit / execute …）兜底。
fn tool_name(update: &Value) -> String {
    update
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("tool")
        .to_string()
}

/// 工具输出：`content` 里的文本块拼起来。
fn tool_output(update: &Value) -> String {
    update
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| {
                    block
                        .get("content")
                        .and_then(|inner| inner.get("text"))
                        .and_then(Value::as_str)
                        .or_else(|| block.get("text").and_then(Value::as_str))
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_chunk_becomes_text_delta() {
        let events = translate_update(&json!({
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": "Hello" },
            },
        }));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "session.text.delta");
        assert_eq!(events[0]["data"]["delta"], "Hello");
        assert_eq!(events[0]["data"]["sessionID"], "session-1");
    }

    #[test]
    fn thought_chunk_becomes_reasoning_delta() {
        let events = translate_update(&json!({
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "agent_thought_chunk",
                "content": { "type": "text", "text": "想一下" },
            },
        }));
        assert_eq!(events[0]["type"], "session.reasoning.delta");
        assert_eq!(events[0]["data"]["delta"], "想一下");
    }

    #[test]
    fn tool_call_opens_a_card() {
        let events = translate_update(&json!({
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "tool_call",
                "toolCallId": "call-1",
                "title": "读取 README",
                "kind": "read",
                "status": "pending",
            },
        }));
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["type"], "session.tool.input.started");
        assert_eq!(events[1]["type"], "session.tool.called");
        assert_eq!(events[1]["data"]["input"]["title"], "读取 README");
    }

    #[test]
    fn completed_tool_call_reports_output() {
        let events = translate_update(&json!({
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-1",
                "status": "completed",
                "content": [{ "type": "content", "content": { "type": "text", "text": "PONG" } }],
            },
        }));
        assert_eq!(events[0]["type"], "session.tool.success");
        assert_eq!(events[0]["data"]["content"][0]["text"], "PONG");
    }

    #[test]
    fn in_progress_update_does_not_duplicate_the_card() {
        let events = translate_update(&json!({
            "sessionId": "session-1",
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "call-1",
                "status": "in_progress",
            },
        }));
        assert!(events.is_empty());
    }

    #[test]
    fn unknown_updates_are_dropped_quietly() {
        let events = translate_update(&json!({
            "sessionId": "session-1",
            "update": { "sessionUpdate": "usage_update", "used": 10, "size": 100 },
        }));
        assert!(events.is_empty());
    }

    /// 真机冒烟：dsh 起不来时，错误里必须带着 stderr 的真实原因。
    ///
    /// ```text
    /// DSH_BIN=/path/to/dsh cargo test --lib real_acp -- --ignored --nocapture
    /// ```
    ///
    /// 用一个普通**文件**当 `DSH_HOME` 制造必然的启动失败（比依赖某个真实目录的
    /// 权限更好复现）。这条链以前会把 `EACCES / ENOTDIR` 吞掉，只给用户一句
    /// "连接已断开"——所以断言里必须看到底层那句话。
    #[test]
    #[ignore = "会真的启动 dsh（需 DSH_BIN 或 PATH 里有 dsh）"]
    fn real_acp_surfaces_launch_failure() {
        let bad_home =
            std::env::temp_dir().join(format!("prism-dsh-badhome-{}", std::process::id()));
        std::fs::write(&bad_home, b"not a directory").expect("写占位文件");
        std::env::set_var("DSH_HOME", &bad_home);

        let runtime = Runtime::new(None);
        let error = runtime
            .create_session("/tmp")
            .expect_err("home 不可写时不该建会话成功");
        println!("错误信息：{error}");

        assert!(error.contains("初始化失败"), "错误没说明是哪一步：{error}");
        // 底层那句话必须露出来：EACCES（权限）或 ENOTDIR（路径不是目录）。
        let lower = error.to_lowercase();
        assert!(
            lower.contains("permission denied")
                || lower.contains("not a directory")
                || lower.contains("eacces")
                || lower.contains("enotdir"),
            "错误里没有底层原因：{error}"
        );

        std::env::remove_var("DSH_HOME");
        let _ = std::fs::remove_file(&bad_home);
    }

    /// 真机冒烟：真起 `dsh --profile acp`，握手 → 建会话 → 一问一答。
    ///
    /// ```text
    /// DSH_BIN=/path/to/dsh DSH_HOME=/tmp/dsh-home \
    ///   cargo test --lib real_acp -- --ignored --nocapture
    /// ```
    ///
    /// 会真的调一次模型（花一次 token），所以默认忽略；它验的是 unit test
    /// 验不到的那一层：进程能不能起、握手帧对不对、事件翻译在真实帧上成不成立。
    #[test]
    #[ignore = "会真的启动 dsh 并调一次模型（需 DSH_BIN / DSH_HOME）"]
    fn real_acp_round_trip() {
        let runtime = Runtime::new(None);
        let events = runtime.subscribe();
        let session_id = runtime.create_session("/tmp").expect("建会话");
        println!("会话：{session_id}");
        assert!(crate::engines::dsh::is_dsh_session(&session_id));

        let message_id = runtime
            .send_prompt(&session_id, "只回复两个字母：PONG")
            .expect("发提示词");
        println!("用户消息 id：{message_id}");

        let mut text = String::new();
        let mut finished = false;
        // 一轮真实回合通常几秒；给 3 分钟容忍首次装配/排队。
        let deadline = std::time::Instant::now() + Duration::from_secs(180);
        while std::time::Instant::now() < deadline {
            let Ok(event) = events.recv_timeout(Duration::from_secs(5)) else {
                continue;
            };
            let kind = event
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            println!("  事件 {kind}");
            match kind {
                "session.text.delta" => {
                    if let Some(delta) = event.pointer("/data/delta").and_then(Value::as_str) {
                        text.push_str(delta);
                    }
                }
                "session.execution.succeeded" | "session.execution.failed" => {
                    finished = true;
                    if kind == "session.execution.failed" {
                        let error = event
                            .pointer("/data/error")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        // 没有 API Key 是环境问题，不是代码问题：这场景下能拿到
                        // 一条结构正确的 failed 事件，本身就说明错误链路通了。
                        if error.contains("API key") || error.contains("API_KEY") {
                            println!("跳过（未配置模型凭据）：{error}");
                            runtime.shutdown();
                            return;
                        }
                        panic!("回合失败：{event}");
                    }
                    break;
                }
                _ => {}
            }
        }
        assert!(finished, "回合没有在超时前收尾");
        println!("助手回复：{text}");
        assert!(
            text.to_uppercase().contains("PONG"),
            "回复里没有 PONG：{text}"
        );

        // 会话必须出现在历史里：ACP 建会话与日志写入是同一条链。
        let summary = crate::engines::dsh::history::session_summary(&session_id)
            .expect("历史里应当有这个会话");
        assert_eq!(summary.directory, "/tmp");

        runtime.shutdown();
    }
}
