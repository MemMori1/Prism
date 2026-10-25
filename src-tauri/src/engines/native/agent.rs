//! 原生引擎的 turn 循环：模型 → 工具 → 模型。
//!
//! 事件形状刻意对齐 `src/liveTurn.ts` 的归约约定（`session.text.delta` /
//! `session.tool.called` …），这样前端不需要为原生引擎加任何适配逻辑：
//! 流式过程走事件，落定后走 `read_timeline`。

use super::{approval, config, provider, store, tools};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{AppHandle, Emitter};

pub const EVENT_NAME: &str = "engine-event";

/// 主代理的完整工具清单（含 `task`，受设置里的开关约束）。
///
/// 整份清单由工具注册表统一给出——`task` 也在表里，不再需要在这里单独
/// `push` 一次；关掉的工具不会出现在这里，也就不占 schema 的 token。
pub(crate) fn tool_specs() -> Vec<Value> {
    tools::specs(&tools::EnabledTools::from_config())
}

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
            // 正卡在审批上的线程要靠这个醒过来（否则得等下一次 200ms 轮询）。
            approval::release_session(session_id);
            true
        }
        None => false,
    }
}

/// 占住这个会话的回合槽位，Drop 时自动释放——即使中途 panic 也不会把会话锁死。
///
/// `/compact` 也用同一个槽位：压缩和跑回合都是「这个会话正在忙」，谁先占到算谁的；
/// 停止按钮解的是同一个标记，所以压缩也能被打断。
pub(crate) struct RunSlot {
    session_id: String,
    flag: Arc<AtomicBool>,
}

impl RunSlot {
    pub(crate) fn take(session_id: &str) -> Result<Self, String> {
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

    pub(crate) fn stopped(&self) -> bool {
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

/// 执行中投进来的消息：等这一轮的工具走完、下一次请求模型前接上（插话）。
#[derive(Debug, Clone)]
struct PendingInput {
    id: String,
    text: String,
}

fn pending() -> &'static Mutex<HashMap<String, VecDeque<PendingInput>>> {
    static PENDING: OnceLock<Mutex<HashMap<String, VecDeque<PendingInput>>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 这个会话当前有没有回合在跑。
fn is_running(session_id: &str) -> bool {
    running()
        .lock()
        .map(|map| map.contains_key(session_id))
        .unwrap_or(false)
}

/// 最多排几条：排太多说明用户在刷屏，不如让他等一下。
const MAX_PENDING: usize = 4;

fn enqueue(session_id: &str, text: &str) -> Result<PendingInput, String> {
    let mut map = pending().lock().expect("插话队列锁");
    let queue = map.entry(session_id.to_string()).or_default();
    if queue.len() >= MAX_PENDING {
        return Err(format!("已经排了 {MAX_PENDING} 条，等这一轮跑完再发"));
    }
    let input = PendingInput {
        id: new_message_id(),
        text: text.to_string(),
    };
    queue.push_back(input.clone());
    Ok(input)
}

fn take_inputs(session_id: &str) -> Vec<PendingInput> {
    let mut map = pending().lock().expect("插话队列锁");
    match map.get_mut(session_id) {
        Some(queue) => queue.drain(..).collect(),
        None => Vec::new(),
    }
}

fn has_inputs(session_id: &str) -> bool {
    pending()
        .lock()
        .map(|map| {
            map.get(session_id)
                .map(|queue| !queue.is_empty())
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

/// 把没送出去的插话退回前端（用户点了停止 / 这一轮失败）。
fn release_inputs(app: &AppHandle, session_id: &str) {
    let dropped = drop_inputs(session_id);
    if dropped.is_empty() {
        return;
    }
    emit(
        app,
        "session.input.dropped",
        json!({ "sessionID": session_id, "texts": dropped }),
    );
}

/// 停手 / 失败时把还没送出去的还回去（前端会把它们放回输入框）。
fn drop_inputs(session_id: &str) -> Vec<String> {
    let mut map = pending().lock().expect("插话队列锁");
    match map.remove(session_id) {
        Some(queue) => queue.into_iter().map(|item| item.text).collect(),
        None => Vec::new(),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
}

pub(crate) fn new_message_id() -> String {
    // 同一纳秒内连发两条会撞出同一个 id（单测就是这么撞上的）。时间戳负责
    // "大体有序"，自增序号负责"绝对不重"——前端拿它当列表 key。
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or(0);
    format!("m{nanos}-{seq}")
}

fn emit(app: &AppHandle, event_type: &str, data: Value) {
    let _ = app.emit(EVENT_NAME, json!({ "type": event_type, "data": data }));
}

/// 系统提示：只讲规矩，不讲人格（省 token，也避免抢模型自己的行为习惯）。
pub(crate) fn system_prompt(directory: &str) -> String {
    format!(
        "你是 Prism 内置的编码 agent，工作目录是 {directory}。\n\
         规则：\n\
         1. 改文件前先用 read_file 读原文，edit_file 的 old_string 必须与原文逐字一致（含缩进）。\n\
         2. 需要事实时用 grep / list_files 查，不要凭印象编造文件内容。\n\
         3. 命令要短、可复现；不要执行需要交互输入的命令。预计超过 ~30 秒的（构建 / 全量测试 / dev server）用 run_command 的 background=true 转后台：它会进任务坞继续跑，别干等；之后用 task_status 看进度，stop=true 收掉。\n\
         4. 回答简洁：先说结论，再给必要的改动说明。\n\
         5. 大范围探索 / 调研（翻一堆文件、搜一堆关键词）交给 task 子代理：它有独立上下文，只把结论带回来。"
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
    message.tool_calls.iter().any(|call| call.id == call_id)
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
                messages.push(json!({
                    "role": "user",
                    "content": user_content(message),
                }));
            }
            // `/compact` 留下的摘要：当成「已经发生过的历史背景」，不是要回应的请求。
            // 用 user 而不是第二条 system——多数 OpenAI 兼容端只认开头那一条 system。
            "compact" => {
                orphans.clear();
                paired.clear();
                messages.push(json!({
                    "role": "user",
                    "content": compact_content(&message.text),
                }));
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

/// 用户消息的 `content`：没图就是纯字符串（最省 token 也最兼容），
/// 有图才升级成 OpenAI 的 parts 数组。
///
/// 图片从磁盘读出来再 base64：历史消息里只有引用，重放时现读，避免会话文件膨胀。
fn user_content(message: &store::StoredMessage) -> Value {
    let images: Vec<&store::StoredPart> = message
        .parts
        .iter()
        .filter(|part| matches!(part, store::StoredPart::Image { .. }))
        .collect();
    if images.is_empty() {
        return Value::String(message.text.clone());
    }
    let mut parts: Vec<Value> = Vec::new();
    if !message.text.trim().is_empty() {
        parts.push(json!({ "type": "text", "text": message.text }));
    }
    for part in images {
        let store::StoredPart::Image { mime, path, .. } = part else {
            continue;
        };
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        parts.push(json!({
            "type": "image_url",
            "image_url": { "url": format!("data:{mime};base64,{encoded}") },
        }));
    }
    if parts.is_empty() {
        return Value::String(message.text.clone());
    }
    Value::Array(parts)
}

/// 摘要以 user 消息送出，所以要一句话说清它的身份，否则模型会把它当新指令
/// 去「回答」。这句话不能省：省掉之后模型经常回一句「好的，我继续」然后停住。
fn compact_content(text: &str) -> String {
    format!(
        "（以下是此前对话的压缩摘要，作为已知背景。不要回应这段摘要本身，等我下一条指令。）\n\n{}",
        text.trim()
    )
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
pub(crate) fn resolve_pick(
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
    let (provider, model) = cfg
        .selection()
        .ok_or_else(|| "原生引擎还没配置：打开 设置 → 模型管理，添加一个模型服务".to_string())?;
    Ok((provider.clone(), model.id.clone()))
}

/// IPC 传进来的一张图：`data` 是 base64（不带 `data:image/png;base64,` 前缀）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncomingImage {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub mime: String,
    #[serde(default)]
    pub data: String,
}

/// 发一条用户消息，跑完整轮。返回用户消息 id。
pub fn send(
    app: &AppHandle,
    session_id: &str,
    text: &str,
    provider_id: Option<&str>,
    model_id: Option<&str>,
    approval_mode: Option<&str>,
    images: &[IncomingImage],
) -> Result<String, String> {
    let raw =
        super::raw_session_id(session_id).ok_or_else(|| format!("不是原生会话：{session_id}"))?;
    let meta = store::load_meta(raw).ok_or_else(|| "原生会话不存在".to_string())?;
    let directory = meta.directory.clone();
    let session_id = session_id.to_string();

    // 槽位先占住：同一会话不允许并发两轮。占不到＝这一轮正在跑，
    // 这条消息改成插话——排进队列，等下次请求模型前接上，立刻返回。
    let slot = match RunSlot::take(&session_id) {
        Ok(slot) => slot,
        Err(error) => {
            if !is_running(&session_id) {
                return Err(error);
            }
            if !images.is_empty() {
                return Err("这一轮还在跑：图片不能排队（排队只存文字），等结束再发".to_string());
            }
            let queued = enqueue(&session_id, text)?;
            emit(
                app,
                "session.input.queued",
                json!({ "sessionID": session_id, "id": queued.id, "text": queued.text }),
            );
            return Ok(queued.id);
        }
    };

    let cfg = config::load();
    let (provider, model) = resolve_pick(&cfg, provider_id, model_id)?;

    let picked = provider
        .models
        .iter()
        .find(|entry| entry.id == model)
        .cloned()
        .unwrap_or_else(|| config::ModelEntry::new(model.clone()));
    if !images.is_empty() && !picked.supports_images {
        return Err(format!(
            "{} 没开图片输入：设置 → 模型服务 → 这个模型右边的「图片」开关打开后再发",
            picked.display()
        ));
    }

    // 图片先落盘：消息里只留引用，时间线读起来才不背着几百 KB 的 base64。
    let mut parts: Vec<store::StoredPart> = vec![store::StoredPart::Text {
        text: text.to_string(),
    }];
    for image in images {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(image.data.trim())
            .map_err(|error| format!("图片不是合法 base64：{error}"))?;
        if bytes.is_empty() {
            continue;
        }
        let name = if image.name.trim().is_empty() {
            "image"
        } else {
            image.name.trim()
        };
        let mime = if image.mime.trim().is_empty() {
            "image/png"
        } else {
            image.mime.trim()
        };
        let (path, size) = store::save_image(raw, name, &bytes, mime)?;
        parts.push(store::StoredPart::Image {
            name: name.to_string(),
            mime: mime.to_string(),
            path,
            bytes: size as i64,
        });
    }

    let user_id = new_message_id();
    let user_message = store::StoredMessage {
        id: user_id.clone(),
        role: "user".to_string(),
        time: now_ms(),
        text: text.to_string(),
        parts,
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

    // 回合丢到后台线程：原生引擎一轮可能跑几分钟，IPC 一直挂着的话，
    // 前端「发送中」就整轮为 true——输入框被锁、按钮全灰，连"插话"都点不了。
    // 投递完立刻返回；过程靠事件推，出错也走 session.execution.failed。
    let worker_app = app.clone();
    let worker_raw = raw.to_string();
    let worker_session = session_id.clone();
    let worker_directory = directory.clone();
    let worker_permission = approval::parse(approval_mode);
    let max_turns = cfg.max_turns();
    std::thread::spawn(move || {
        let outcome = run_turns(
            &worker_app,
            &worker_raw,
            &worker_session,
            &worker_directory,
            &provider,
            &model,
            max_turns,
            worker_permission,
            &slot,
        );
        match outcome {
            Ok(Stop::Done) => {
                emit(
                    &worker_app,
                    "session.execution.succeeded",
                    json!({ "sessionID": worker_session }),
                );
            }
            Ok(Stop::Cancelled) => {
                release_inputs(&worker_app, &worker_session);
                emit(
                    &worker_app,
                    "session.execution.interrupted",
                    json!({ "sessionID": worker_session }),
                );
            }
            Err(error) => {
                release_inputs(&worker_app, &worker_session);
                emit(
                    &worker_app,
                    "session.execution.failed",
                    json!({ "sessionID": worker_session, "error": error }),
                );
            }
        }
    });

    Ok(user_id)
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
    permission: approval::Mode,
    slot: &RunSlot,
) -> Result<Stop, String> {
    /// 一轮 run_turns 里最多自动压缩几次。压完历史只剩摘要（几千 token），
    /// 正常一次就够；跑满三次说明有什么在造巨量输出，别再挖了——交给
    /// 用户看见的真实错误。
    ///
    /// 编排里的 `dag::runner::NativeRunner` 用的是另一套救援（机械裁剪，上限 2），
    /// 因为它不能为一次超限再叫一次模型。改这里的语义时那边不会自动跟上，
    /// 见 `overflow.rs` 文件头的「两套循环，一处定义」。
    const RESCUE_LIMIT: u32 = 3;
    // 开关本轮只读一次：工具清单与执行校验用同一份，不会出现
    // "schema 里有、执行时又说被关了"的错位。
    let enabled = tools::EnabledTools::from_config();
    let specs = tools::specs(&enabled);
    // 历史只读一次：原来每轮都重读整份 messages.jsonl 再逐行解析，长会话
    // 走一个工具循环就是平方级开销。本轮自己写进去的消息同步补进来，
    // 下一轮看到的历史与磁盘一致。
    let mut history = store::messages(raw);
    let mut rescues = 0u32;
    // 轮次计数自己管：救援重试不该烧掉一轮（超限发生在最后一轮时，
    // for 循环的 continue 会静默结束、连个回答都不剩）。
    let mut ordinal = -1i32;
    loop {
        ordinal += 1;
        if ordinal as u32 >= max_turns {
            break;
        }
        if slot.stopped() {
            return Ok(Stop::Cancelled);
        }
        // 插话：上一次工具跑完、下一次请求模型前接上。落库顺序就是它被看到的顺序。
        for input in take_inputs(session_id) {
            let message = store::StoredMessage {
                id: input.id.clone(),
                role: "user".to_string(),
                time: now_ms(),
                text: input.text.clone(),
                parts: vec![store::StoredPart::Text {
                    text: input.text.clone(),
                }],
                tool_calls: Vec::new(),
                tool_call_id: None,
                tokens: None,
            };
            store::append(raw, &message)?;
            emit(
                app,
                "session.input.delivered",
                json!({ "sessionID": session_id, "id": input.id }),
            );
            history.push(message);
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
            // 模型把请求整个拒了（上下文超限）：这是会话太大，不是请求错。
            // 压一次历史再重发，会话自己救活，不让用户去背「/compact 是什么」。
            //
            // 只有聊天这条路能"叫模型做摘要"：编排节点无人值守、也没有会话
            // 文件，那边是机械裁剪（`dag::runner::NativeRunner` 的 `trim_messages`）。
            // 两条路都要能自愈，别只修一边。
            Err(provider::ChatError::Failed(error))
                if provider::is_context_overflow(&error) && rescues < RESCUE_LIMIT =>
            {
                rescues += 1;
                match super::compact::rescue(app, session_id, raw, provider, model, slot) {
                    // 重写后的磁盘才是下一轮要发的东西，内存里这份历史已作废；
                    // 轮次不前进——救援消耗的是被拒的那次请求，不是用户的一轮。
                    Ok(super::compact::Rescue::Done) => {
                        history = store::messages(raw);
                        ordinal -= 1;
                        continue;
                    }
                    Ok(super::compact::Rescue::Cancelled) => return Ok(Stop::Cancelled),
                    Err(why) => {
                        return Err(format!(
                            "上下文超限，自动压缩也没救回来：{why}\n原始错误：{error}"
                        ))
                    }
                }
            }
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
                cached: usage.cached,
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
            // 刚好这时候有人插话：这一轮不算完，接上继续。
            if has_inputs(session_id) {
                continue;
            }
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

            // 权限闸门：只读直接拒；询问档在这里发事件并等用户回复。
            let gate = approval::action_of(&call.name).and_then(|action| {
                let verdict = approval::gate(
                    app,
                    session_id,
                    permission,
                    action,
                    approval::resources_of(&call.name, &input),
                    approval::message_of(&call.name),
                    &call.id,
                    &|| slot.stopped(),
                );
                match verdict {
                    approval::Verdict::Allow => None,
                    approval::Verdict::Deny(reason) => Some(reason),
                }
            });
            // 所有工具走同一条路：注册表按名字取出执行体。
            // `task`（子代理）不再特判——它也在表里，只是执行体内部起一个
            // 隔离上下文的循环。
            let result = match &gate {
                Some(reason) => tools::ToolResult::fail(reason.clone()),
                None => tools::execute(
                    &tools::ToolContext {
                        app: Some(app),
                        session_id,
                        directory,
                        call_id: &call.id,
                        provider: Some(provider),
                        model: Some(model),
                        permission: Some(permission.as_str()),
                        enabled: &enabled,
                        dag: None,
                        should_stop: &|| slot.stopped(),
                    },
                    &call.name,
                    &call.arguments,
                ),
            };
            let extra_meta = result.meta;
            let extra_tokens = result.tokens;
            let result = result.outcome;
            // 进度元数据（子代理的步骤轨迹）与错误标志一起给到前端。
            let mut metadata = json!({ "error": result.error.is_some() });
            if let Some(extra) = &extra_meta {
                if let (Some(meta), Some(extra)) = (metadata.as_object_mut(), extra.as_object()) {
                    for (key, value) in extra {
                        meta.insert(key.clone(), value.clone());
                    }
                }
            }
            emit(
                app,
                "session.tool.success",
                json!({
                    "sessionID": session_id,
                    "id": call.id,
                    "content": [{ "type": "text", "text": result.text }],
                    "metadata": metadata,
                }),
            );

            parts.push(store::StoredPart::Tool {
                name: call.name.clone(),
                call_id: call.id.clone(),
                status: if result.error.is_some() {
                    "error"
                } else {
                    "completed"
                }
                .to_string(),
                summary: summarize_input(&input),
                input: if input.is_null() { None } else { Some(input) },
                output: Some(result.text.clone()),
                error: result.error.clone(),
                metadata: extra_meta.clone(),
            });

            tool_messages.push(store::StoredMessage {
                id: new_message_id(),
                role: "tool".to_string(),
                time: now_ms(),
                text: result.text,
                parts: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: Some(call.id.clone()),
                tokens: extra_tokens,
            });
        }

        let tokens = outcome.usage.map(|usage| store::StoredTokens {
            input: usage.input,
            output: usage.output,
            cached: usage.cached,
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
    use super::super::subagent;
    use super::*;

    #[test]
    fn pending_inputs_roundtrip() {
        let sid = "native:test-pending-roundtrip";
        assert!(!has_inputs(sid));
        let first = enqueue(sid, "一").expect("入队");
        let second = enqueue(sid, "二").expect("入队");
        assert!(has_inputs(sid));
        assert_ne!(first.id, second.id);

        let taken = take_inputs(sid);
        assert_eq!(taken.len(), 2);
        assert_eq!(taken[0].text, "一");
        assert_eq!(taken[1].text, "二");
        // 取走就清空，别让下一轮重复注入
        assert!(!has_inputs(sid));
        assert!(take_inputs(sid).is_empty());
    }

    #[test]
    fn pending_inputs_cap_and_drop() {
        let sid = "native:test-pending-cap";
        for index in 0..MAX_PENDING {
            enqueue(sid, &format!("第 {index} 条")).expect("入队");
        }
        assert!(enqueue(sid, "超了").is_err(), "超过上限要拒绝");
        let dropped = drop_inputs(sid);
        assert_eq!(dropped.len(), MAX_PENDING);
        assert_eq!(dropped[0], "第 0 条");
        assert!(drop_inputs(sid).is_empty());
    }

    #[test]
    fn idle_session_is_not_running() {
        assert!(!is_running("native:definitely-not-running"));
    }

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

    /// 没图就必须还是字符串：老服务端对 parts 数组的兼容性不如纯文本。
    /// 有图才升级成 parts，且图片是现读磁盘拼 data URL（消息里只存引用）。
    #[test]
    fn user_content_upgrades_to_parts_only_with_images() {
        let plain = message("user", "看这个");
        assert_eq!(super::user_content(&plain), json!("看这个"));

        let path = std::env::temp_dir().join(format!("prism-vision-{}.png", std::process::id()));
        std::fs::write(&path, [0x89u8, 0x50, 0x4e, 0x47]).expect("写临时图片");
        let mut with_image = message("user", "看这个");
        with_image.parts = vec![
            store::StoredPart::Text {
                text: "看这个".to_string(),
            },
            store::StoredPart::Image {
                name: "shot.png".to_string(),
                mime: "image/png".to_string(),
                path: path.to_string_lossy().to_string(),
                bytes: 4,
            },
        ];
        let parts = super::user_content(&with_image);
        let parts = parts.as_array().expect("带图应该是 parts 数组");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[1]["type"], "image_url");
        assert!(parts[1]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn provider_messages_start_with_system_prompt() {
        let list = provider_messages("/tmp/demo", &[message("user", "hi")]);
        assert_eq!(list[0]["role"], "system");
        assert!(list[0]["content"].as_str().unwrap().contains("/tmp/demo"));
        assert_eq!(list[1]["role"], "user");
    }

    /// 前缀缓存的命中条件是「两次请求的前部逐字节一致」。这条用**真实构造链**
    /// （`provider_messages` 逐条拼）走两轮，确认第二轮只是第一轮的纯追加。
    ///
    /// 为什么不靠 `provider.rs` 里那条 `consecutive_turns_are_append_only_...`：
    /// 那条喂的是手工写死的最小 messages，测的是"HTTP 层没乱动数组"；真实路径
    /// 还要过 `provider_messages` 的重排、`system_prompt` 的目录拼接、`user_content`
    /// 的 parts 转换——任何一处抖动都会让前缀 miss，而手工数据看不见。
    #[test]
    fn provider_messages_are_append_only_across_turns() {
        let dir = "/tmp/prism-cache";
        let mut history = vec![message("user", "你好")];
        let first = provider_messages(dir, &history);
        history.push(message("assistant", "你好，有什么可以帮你的"));
        history.push(message("user", "看看这个目录"));
        let second = provider_messages(dir, &history);
        assert_eq!(
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&second[..first.len()]).unwrap(),
            "第二轮必须以上一轮为逐字节前缀，否则前缀缓存全 miss"
        );
    }

    /// 工具数组也在缓存前缀里：同一份开关配置下必须逐字节稳定。
    ///
    /// `specs()` 每次都会过一遍 `visible()`（排序 + `needs_met` 探测）。排序是
    /// 显式的；探测如果对同一个后端一会儿有一会儿没有，工具清单就会时多时少，
    /// 整个请求前缀跟着全 miss。
    #[test]
    fn tool_specs_are_byte_stable_between_calls() {
        let first = serde_json::to_string(&tools::specs(&tools::EnabledTools::from_config()))
            .expect("序列化工具清单");
        let again = serde_json::to_string(&tools::specs(&tools::EnabledTools::from_config()))
            .expect("序列化工具清单");
        assert_eq!(first, again, "重建工具清单必须逐字节一致，否则前缀缓存全 miss");
    }

    /// 压缩摘要要变成「背景 user 消息」，且前后不能被工具结果串味：
    /// 拿走摘要时手里挂着的孤儿工具结果要一起清掉，否则会挂到错误的位置。
    #[test]
    fn compact_summary_becomes_background_user_message() {
        let mut compact = message("compact", "## 目标\n改 README");
        compact.id = "c1".to_string();
        let history = vec![message("user", "开始"), compact, message("user", "继续")];
        let list = provider_messages("/tmp/demo", &history);
        let roles: Vec<&str> = list.iter().map(|item| item["role"].as_str().unwrap()).collect();
        assert_eq!(roles, ["system", "user", "user", "user"]);
        let summary = list[2]["content"].as_str().expect("摘要应是纯文本");
        assert!(summary.contains("压缩摘要"), "{summary}");
        assert!(summary.contains("改 README"), "{summary}");
        assert!(summary.contains("不要回应这段摘要本身"), "{summary}");
        // 真正的用户消息不能被加料。
        assert_eq!(list[3]["content"], "继续");
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
        assert_eq!(
            roles,
            ["user", "assistant", "tool"],
            "工具结果要跟在其归属之后"
        );
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

    /// task 派发的回填路径：报告转正、token 折进消息、元数据原样带回。
    #[test]
    fn subagent_fold_returns_report_and_tokens() {
        let outcome = subagent::SubagentOutcome {
            report: "结论：入口在 login.ts".to_string(),
            error: None,
            trace: subagent::SubagentTrace {
                agent_type: "explore",
                label: "探索",
                turns: 3,
                forced: false,
                steps: Vec::new(),
            },
            tokens_input: 1200,
            tokens_output: 300,
            tokens_cached: 1100,
        };
        let folded = subagent::fold(outcome);
        assert!(folded.outcome.error.is_none(), "{:?}", folded.outcome.error);
        assert_eq!(folded.outcome.text, "结论：入口在 login.ts");
        // 元数据带回（含子代理轨迹），前端据此画步骤。
        let meta = folded.meta.expect("子代理应当带回轨迹元数据");
        assert_eq!(meta["subagent"]["agentType"], "explore");
        // token 折进工具消息，累加到会话总量。
        let tokens = folded.tokens.expect("有 token 就该折进去");
        assert_eq!(tokens.input, 1200);
        assert_eq!(tokens.output, 300);
        assert_eq!(tokens.cached, 1100, "缓存命中要一路折进会话账目");
    }

    /// 子代理失败时：错误转红，但已产出的部分报告也一并带出来（不白跑）。
    #[test]
    fn subagent_fold_keeps_partial_report_on_error() {
        let outcome = subagent::SubagentOutcome {
            report: "查到了入口".to_string(),
            error: Some("子代理跑满 16 轮仍未交出报告，已停止".to_string()),
            trace: subagent::SubagentTrace::default(),
            tokens_input: 0,
            tokens_output: 0,
            tokens_cached: 0,
        };
        let folded = subagent::fold(outcome);
        assert!(folded.outcome.error.is_some());
        assert!(
            folded.outcome.text.contains("查到了入口"),
            "部分报告不能丢：{}",
            folded.outcome.text
        );
        assert!(
            folded.outcome.text.contains("已产出的部分报告"),
            "{}",
            folded.outcome.text
        );
        // 没烧 token 就不折 token（None，别记个 0/0 上去）。
        assert!(folded.tokens.is_none());
    }
}
