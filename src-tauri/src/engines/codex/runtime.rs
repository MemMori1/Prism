//! Codex 运行轴：`codex app-server` 长活进程（stdio 行分隔 JSON-RPC）。
//!
//! 协议摸底结论（codex-cli 0.133.0，真机验证）：
//! - 握手：`initialize`（声明 experimentalApi 能力）→ `thread/start`
//!   （`ephemeral=false` 会立即写 ~/.codex/state_5.sqlite，浏览层直接可见）
//!   或 `thread/resume`（续跑历史会话）→ `turn/start` 发消息；
//!   流式期间 `turn/steer` 插话、`turn/interrupt` 打断（通知里带
//!   threadId/turnId，可精准回打）。
//! - 事件：服务器以通知推送 `turn/*`、`item/*`（agentMessage 增量、
//!   reasoning 摘要、commandExecution 输出、fileChange 等）。
//! - 审批：服务器反向发请求（`item/commandExecution/requestApproval` 等），
//!   本模块登记挂起表并广播 `permission.asked`；回复时按
//!   `availableDecisions` 把前端三档 once/always/reject 映射为
//!   accept / acceptForSession / decline（缺省回退 accept / cancel）。
//!   `serverRequest/resolved` 表示请求已被引擎侧解决（自动放行等），
//!   广播 `permission.replied` 让各视图收卡。
//! - 配置主权：完全继承 ~/.codex/config.toml（模型 / 审批策略 / 沙箱），
//!   Prism 不写入、不覆盖；唯一例外是测试注入环境变量
//!   `PRISM_CODEX_APP_SERVER_ARGS`（JSON 数组，附加 CLI 参数）。
//!
//! 与 OpenCode 运行轴对齐：事件归一化成同形 `{ type, data }` 广播
//! （Tauri `engine-event` + 进程内订阅者），会话 id 约定为
//! `codex:<threadId>`（与浏览层一致，ipc 层按前缀分发）。

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use super::locator;
use crate::catalog;
use crate::engines::opencode::runtime::{CreatedSession, PermissionRequest, RuntimeStatus};

/// 前端订阅的事件名（Tauri 事件），与 OpenCode 运行轴共用。
pub const EVENT_NAME: &str = "engine-event";

/// Codex 会话 id 前缀（浏览层同款约定）。
pub const SESSION_PREFIX: &str = "codex:";

const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// 单个 item 累计输出上限（字符），防长命令撑爆内存。
const MAX_ITEM_OUTPUT: usize = 256 * 1024;

/// 从会话 id 取线程 id（不是 Codex 会话则为 None）。
pub fn thread_id_of(session_id: &str) -> Option<&str> {
    session_id.strip_prefix(SESSION_PREFIX)
}

fn session_id_of(thread_id: &str) -> String {
    format!("{SESSION_PREFIX}{thread_id}")
}

/// 调试追踪用：截断长文本打印（仅 debug 构建引用）。
#[cfg(debug_assertions)]
fn truncate(text: &str, max: usize) -> String {
    let mut result: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        result.push('…');
    }
    result
}

/// 审批请求类型（决定回复载荷的形态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApprovalKind {
    Exec,
    FileChange,
    /// 其余类型（权限配置等）：登记但暂不支持回复。
    Other,
}

/// 挂起中的审批请求。
#[derive(Debug, Clone)]
struct PendingApproval {
    /// JSON-RPC 请求 id（服务器分配，响应时原样回写）。
    raw_id: Value,
    kind: ApprovalKind,
    /// 引擎给出的可选决策（归一化后的字符串集合）。
    available: Vec<String>,
    /// 暴露给前端的请求形态（列表端点与事件同源）。
    request: PermissionRequest,
}

#[derive(Default)]
struct SessionState {
    /// 本进程已加载（thread/start 或 thread/resume 过）的线程。
    loaded: HashSet<String>,
    /// 进行中的轮次：threadId → turnId。
    active_turns: HashMap<String, String>,
    /// 挂起审批：前端请求 id 字符串 → 详情。
    approvals: HashMap<String, PendingApproval>,
    /// 命令输出累计：itemId → 文本（完成时取用，聚合输出优先）。
    item_output: HashMap<String, String>,
}

struct Conn {
    child: Mutex<Child>,
    stdin: Mutex<Option<ChildStdin>>,
    next_id: AtomicU64,
}

struct Inner {
    app: Option<AppHandle>,
    /// 进程内事件订阅者（调试与测试用；前端走 Tauri 事件）。
    subscribers: Mutex<Vec<mpsc::Sender<Value>>>,
    /// 我们发起的请求等待表：id → 响应通道。
    pending: Mutex<HashMap<u64, mpsc::Sender<Result<Value, String>>>>,
    conn: Mutex<Option<Arc<Conn>>>,
    sessions: Mutex<SessionState>,
    /// 追加的 app-server 启动参数（测试注入用，正常为空）。
    extra_args: Vec<String>,
    shutting_down: AtomicBool,
}

impl Inner {
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

    /// 清空等待表：让所有在途调用立即拿到连接错误，而不是干等超时。
    fn clear_pending(&self) {
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
    }
}

/// 运行轴句柄：Tauri 管理为全局单例，App 内克隆即可跨线程使用。
#[derive(Clone)]
pub struct Runtime {
    inner: Arc<Inner>,
}

impl Runtime {
    pub fn new(app: AppHandle) -> Self {
        Self::build(Some(app), app_server_args_from_env())
    }

    fn build(app: Option<AppHandle>, extra_args: Vec<String>) -> Self {
        Self {
            inner: Arc::new(Inner {
                app,
                subscribers: Mutex::new(Vec::new()),
                pending: Mutex::new(HashMap::new()),
                conn: Mutex::new(None),
                sessions: Mutex::new(SessionState::default()),
                extra_args,
                shutting_down: AtomicBool::new(false),
            }),
        }
    }

    #[cfg(test)]
    fn unattached_with_args(extra_args: Vec<String>) -> Self {
        Self::build(None, extra_args)
    }

    /// 进程内订阅引擎事件（例行任务执行器与测试用；前端走 Tauri 事件）。
    pub fn subscribe(&self) -> mpsc::Receiver<Value> {
        let (sender, receiver) = mpsc::channel();
        self.inner
            .subscribers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(sender);
        receiver
    }

    /// 当前状态（不启动 app-server；为设置页运行轴状态预留）。
    #[allow(dead_code)]
    pub fn status(&self) -> RuntimeStatus {
        let conn = self
            .inner
            .conn
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if let Some(conn) = conn {
            let mut child = conn.child.lock().unwrap_or_else(|error| error.into_inner());
            if let Ok(None) = child.try_wait() {
                return RuntimeStatus {
                    running: true,
                    base_url: None,
                    pid: Some(child.id()),
                };
            }
        }
        RuntimeStatus {
            running: false,
            base_url: None,
            pid: None,
        }
    }

    /// 关停：结束 app-server 与订阅通道（在 App 退出时调用）。
    pub fn shutdown(&self) {
        self.inner.shutting_down.store(true, Ordering::Relaxed);
        let conn = self
            .inner
            .conn
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(conn) = conn {
            terminate_conn(&conn);
        }
        self.inner.clear_pending();
        self.inner
            .subscribers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
    }

    // ------------------------------------------------------------ 会话操作

    /// 在指定目录新建会话（首次调用会拉起 app-server，可能稍慢）。
    pub fn create_session(&self, directory: &str) -> Result<CreatedSession, String> {
        let directory = directory.trim();
        if directory.is_empty() {
            return Err("会话目录为空".to_string());
        }
        let conn = self.ready_conn()?;
        let response = self.call_on(
            &conn,
            "thread/start",
            json!({ "cwd": directory, "ephemeral": false }),
            CALL_TIMEOUT,
        )?;
        let thread_id = response
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .ok_or_else(|| "thread/start 响应缺少 thread.id".to_string())?
            .to_string();
        let actual_cwd = response
            .pointer("/thread/cwd")
            .and_then(Value::as_str)
            .map(str::to_string);
        self.inner
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .loaded
            .insert(thread_id.clone());
        #[cfg(debug_assertions)]
        println!("[prism] codex thread/start -> {thread_id}");
        Ok(CreatedSession {
            session_id: session_id_of(&thread_id),
            directory: actual_cwd,
        })
    }

    /// 发送一条用户消息：空闲时开新轮，进行中则插话（steer）。
    /// 返回空串（Codex 没有 OpenCode 那样的用户消息 id）。
    pub fn send_prompt(&self, session_id: &str, text: &str) -> Result<String, String> {
        self.send_prompt_with(session_id, text, None, None)
    }

    /// 发送一条用户消息（带可选的模型 / 推理强度覆盖，对后续轮次持续生效）。
    pub fn send_prompt_with(
        &self,
        session_id: &str,
        text: &str,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<String, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("消息内容为空".to_string());
        }
        let thread_id = thread_id_of(session_id)
            .ok_or_else(|| format!("不是 Codex 会话：{session_id}"))?
            .to_string();
        let conn = self.ready_conn()?;
        self.ensure_thread_loaded(&conn, &thread_id)?;

        let input = json!([{ "type": "text", "text": text }]);

        // 进行中的轮次 → 插话；轮次刚结束的竞态则回退为新轮。
        let active = self
            .inner
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .active_turns
            .get(&thread_id)
            .cloned();
        if let Some(turn_id) = active {
            let steer = self.call_on(
                &conn,
                "turn/steer",
                json!({ "threadId": thread_id, "expectedTurnId": turn_id, "input": input }),
                CALL_TIMEOUT,
            );
            if steer.is_ok() {
                return Ok(String::new());
            }
            #[cfg(debug_assertions)]
            println!("[prism] codex turn/steer 失败回退新轮：{steer:?}");
            self.inner
                .sessions
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .active_turns
                .remove(&thread_id);
        }

        let mut start = json!({ "threadId": thread_id, "input": input });
        if let Some(model) = model.filter(|value| !value.trim().is_empty()) {
            start["model"] = json!(model);
        }
        if let Some(effort) = effort.filter(|value| !value.trim().is_empty() && *value != "default")
        {
            start["effort"] = json!(effort);
        }
        self.call_on(&conn, "turn/start", start, CALL_TIMEOUT)?;
        Ok(String::new())
    }

    /// 未加载的线程先 resume（幂等，重复 resume 无副作用）。
    fn ensure_thread_loaded(&self, conn: &Arc<Conn>, thread_id: &str) -> Result<(), String> {
        let is_loaded = self
            .inner
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .loaded
            .contains(thread_id);
        if is_loaded {
            return Ok(());
        }
        self.call_on(
            conn,
            "thread/resume",
            json!({ "threadId": thread_id }),
            CALL_TIMEOUT,
        )?;
        self.inner
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .loaded
            .insert(thread_id.to_string());
        Ok(())
    }

    /// 模型目录（每个模型带支持的推理档位）。
    pub fn list_models(&self) -> Result<Vec<catalog::EngineModel>, String> {
        let conn = self.ready_conn()?;
        let result = self.call_on(&conn, "model/list", json!({}), CALL_TIMEOUT)?;
        Ok(catalog::parse_codex_models(&result))
    }

    /// 在线程里直接跑一条 shell 命令（不走模型；不继承线程沙箱策略）。
    pub fn shell_command(&self, session_id: &str, command: &str) -> Result<(), String> {
        let command = command.trim();
        if command.is_empty() {
            return Err("shell 命令为空".to_string());
        }
        let thread_id = thread_id_of(session_id)
            .ok_or_else(|| format!("不是 Codex 会话：{session_id}"))?
            .to_string();
        let conn = self.ready_conn()?;
        self.ensure_thread_loaded(&conn, &thread_id)?;
        self.call_on(
            &conn,
            "thread/shellCommand",
            json!({ "threadId": thread_id, "command": command }),
            CALL_TIMEOUT,
        )?;
        Ok(())
    }

    /// 中断当前执行（没有进行中的轮次时视为已停）。
    pub fn interrupt(&self, session_id: &str) -> Result<(), String> {
        let thread_id = thread_id_of(session_id)
            .ok_or_else(|| format!("不是 Codex 会话：{session_id}"))?
            .to_string();
        let active = self
            .inner
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .active_turns
            .get(&thread_id)
            .cloned();
        let Some(turn_id) = active else {
            return Ok(());
        };
        let conn = self.ready_conn()?;
        self.call_on(
            &conn,
            "turn/interrupt",
            json!({ "threadId": thread_id, "turnId": turn_id }),
            CALL_TIMEOUT,
        )?;
        Ok(())
    }

    /// 会话当前挂起的权限请求（选择会话时补拉）。
    pub fn list_permissions(&self, session_id: &str) -> Result<Vec<PermissionRequest>, String> {
        if thread_id_of(session_id).is_none() {
            return Ok(Vec::new());
        }
        let state = self
            .inner
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Ok(state
            .approvals
            .values()
            .filter(|approval| approval.request.session_id == session_id)
            .map(|approval| approval.request.clone())
            .collect())
    }

    /// 回复权限请求：once / always / reject（映射到引擎决策）。
    pub fn permission_reply(
        &self,
        session_id: &str,
        request_id: &str,
        decision: &str,
    ) -> Result<(), String> {
        if !matches!(decision, "once" | "always" | "reject") {
            return Err(format!("不支持的权限决定：{decision}"));
        }
        if thread_id_of(session_id).is_none() {
            return Err(format!("不是 Codex 会话：{session_id}"));
        }
        let approval = {
            let state = self
                .inner
                .sessions
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state
                .approvals
                .get(request_id)
                .filter(|approval| approval.request.session_id == session_id)
                .cloned()
        };
        let Some(approval) = approval else {
            return Err("该请求已处理或不存在".to_string());
        };
        let payload = decision_payload(approval.kind, &approval.available, decision)?;
        let conn = self.ready_conn()?;
        write_response(&conn, &approval.raw_id, payload)?;
        self.inner
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .approvals
            .remove(request_id);
        self.inner.broadcast(&json!({
            "type": "permission.replied",
            "data": { "sessionID": session_id, "requestID": request_id },
        }));
        #[cfg(debug_assertions)]
        println!("[prism] codex permission_reply {request_id} -> {decision}");
        Ok(())
    }

    // ------------------------------------------------------------ 连接管理

    /// 确保 app-server 就绪（必要时拉起并初始化）。
    fn ready_conn(&self) -> Result<Arc<Conn>, String> {
        if self.inner.shutting_down.load(Ordering::Relaxed) {
            return Err("运行轴已关闭".to_string());
        }
        let mut slot = self
            .inner
            .conn
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(conn) = slot.as_ref() {
            let alive = conn
                .child
                .lock()
                .map(|mut child| child.try_wait().map(|status| status.is_none()).unwrap_or(false))
                .unwrap_or(false);
            if alive {
                return Ok(conn.clone());
            }
        }
        if let Some(dead) = slot.take() {
            #[cfg(debug_assertions)]
            println!("[prism] codex app-server 已退出，重新启动");
            terminate_conn(&dead);
        }

        let conn = spawn_app_server(&self.inner, &self.inner.extra_args)?;
        // 冷启动握手：失败则回收进程。
        let result = self.call_on(
            &conn,
            "initialize",
            json!({
                "clientInfo": {
                    "name": "prism",
                    "title": "Prism",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": { "experimentalApi": true },
            }),
            CALL_TIMEOUT,
        );
        if let Err(cause) = result {
            terminate_conn(&conn);
            return Err(format!("codex app-server 初始化失败：{cause}"));
        }
        // 新进程重置会话簿记（线程需要重新 resume）。
        {
            let mut sessions = self
                .inner
                .sessions
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            sessions.loaded.clear();
            sessions.active_turns.clear();
            sessions.approvals.clear();
            sessions.item_output.clear();
        }
        self.inner.clear_pending();
        #[cfg(debug_assertions)]
        println!(
            "[prism] codex app-server 已启动（pid={}）",
            conn.child.lock().map(|child| child.id()).unwrap_or(0)
        );
        *slot = Some(conn.clone());
        Ok(conn)
    }

    /// 发起一次 JSON-RPC 调用并等待响应。
    fn call_on(
        &self,
        conn: &Arc<Conn>,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let id = conn.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel();
        self.inner
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(id, sender);
        let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if let Err(cause) = write_line(conn, &line.to_string()) {
            self.inner
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&id);
            terminate_conn(conn);
            return Err(format!("{method} 发送失败：{cause}"));
        }
        match receiver.recv_timeout(timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.inner
                    .pending
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .remove(&id);
                Err(format!("{method} 响应超时（{}s）", timeout.as_secs()))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(format!("{method} 中断：app-server 连接已重置"))
            }
        }
    }
}

// ---------------------------------------------------------------- 进程

fn app_server_args_from_env() -> Vec<String> {
    std::env::var("PRISM_CODEX_APP_SERVER_ARGS")
        .ok()
        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
        .unwrap_or_default()
}

fn spawn_app_server(inner: &Arc<Inner>, extra_args: &[String]) -> Result<Arc<Conn>, String> {
    let cli = locator::cli_path().ok_or_else(|| "未找到 codex CLI，无法启动运行轴".to_string())?;
    let mut command = Command::new(&cli);
    command
        .arg("app-server")
        .args(extra_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(home) = std::env::var_os("HOME") {
        command.current_dir(home);
    }
    let mut child = command
        .spawn()
        .map_err(|cause| format!("启动 codex app-server 失败：{cause}"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "app-server stdin 不可用".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "app-server stdout 不可用".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "app-server stderr 不可用".to_string())?;

    let conn = Arc::new(Conn {
        child: Mutex::new(child),
        stdin: Mutex::new(Some(stdin)),
        next_id: AtomicU64::new(1),
    });

    thread::Builder::new()
        .name("prism-codex-err".into())
        .spawn(move || {
            for line in BufReader::new(stderr).lines() {
                if line.is_err() {
                    break; // 管道关闭
                }
                // 仅 debug 构建回显 app-server 的 stderr，便于排查。
                #[cfg(debug_assertions)]
                if let Ok(line) = &line {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        println!("[prism] app-server (stderr): {trimmed}");
                    }
                }
            }
        })
        .map_err(|cause| format!("启动 stderr 线程失败：{cause}"))?;

    // 读线程：独占 stdout，按消息形态分发（响应 / 反向请求 / 通知）。
    let reader_conn = conn.clone();
    let reader_inner = inner.clone();
    thread::Builder::new()
        .name("prism-codex-out".into())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                #[cfg(debug_assertions)]
                if std::env::var_os("PRISM_CODEX_TRACE").is_some() {
                    println!("[prism] app-server <- {}", truncate(trimmed, 200));
                }
                match serde_json::from_str::<Value>(trimmed) {
                    Ok(value) => dispatch_message(&reader_inner, &reader_conn, value),
                    Err(cause) => {
                        eprintln!("[prism] app-server 输出解析失败：{cause}");
                    }
                }
            }
            // stdout 关闭：清空等待者，让调用方立即拿到错误而不是等超时。
            reader_inner.clear_pending();
        })
        .map_err(|cause| format!("启动读线程失败：{cause}"))?;

    Ok(conn)
}

/// 写一行 JSON 到 app-server stdin。
fn write_line(conn: &Arc<Conn>, line: &str) -> Result<(), String> {
    let mut slot = conn.stdin.lock().unwrap_or_else(|error| error.into_inner());
    let stdin = slot
        .as_mut()
        .ok_or_else(|| "app-server stdin 已关闭".to_string())?;
    stdin
        .write_all(line.as_bytes())
        .and_then(|_| stdin.write_all(b"\n"))
        .and_then(|_| stdin.flush())
        .map_err(|cause| cause.to_string())
}

/// 回复服务器反向请求（result 形态）。
fn write_response(conn: &Arc<Conn>, raw_id: &Value, result: Value) -> Result<(), String> {
    let line = json!({ "jsonrpc": "2.0", "id": raw_id, "result": result });
    write_line(conn, &line.to_string())
}

/// 回复服务器反向请求（error 形态）。
fn write_error_response(
    conn: &Arc<Conn>,
    raw_id: &Value,
    code: i64,
    message: &str,
) -> Result<(), String> {
    let line = json!({
        "jsonrpc": "2.0",
        "id": raw_id,
        "error": { "code": code, "message": message },
    });
    write_line(conn, &line.to_string())
}

/// 结束连接：先关 stdin 让进程体面退出，再兜底 kill。
fn terminate_conn(conn: &Arc<Conn>) {
    if let Ok(mut slot) = conn.stdin.lock() {
        slot.take();
    }
    thread::sleep(Duration::from_millis(120));
    if let Ok(mut child) = conn.child.lock() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

// ---------------------------------------------------------------- 消息分发

/// 读线程分发：三类消息（响应 / 反向请求 / 通知）。
fn dispatch_message(inner: &Arc<Inner>, conn: &Arc<Conn>, value: Value) {
    let id = value.get("id").cloned();
    let method = value
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_string);
    match (id, method) {
        // 响应：唤醒等待方。
        (Some(id), None) => {
            let sender = {
                let mut pending = inner
                    .pending
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                id.as_u64().and_then(|key| pending.remove(&key))
            };
            if let Some(sender) = sender {
                let outcome = match value.get("error") {
                    Some(error) if !error.is_null() => Err(error_message(error)),
                    _ => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = sender.send(outcome);
            }
        }
        // 反向请求：审批 / 问答等。
        (Some(id), Some(method)) => {
            handle_server_request(inner, conn, id, &method, value.get("params"));
        }
        // 通知：归一化成前端事件广播。
        (None, Some(method)) => {
            let events = {
                let mut sessions = inner
                    .sessions
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                apply_notification(&mut sessions, &method, value.get("params"))
            };
            for event in events {
                inner.broadcast(&event);
            }
        }
        (None, None) => {}
    }
}

fn error_message(error: &Value) -> String {
    error
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| error.to_string())
}

fn handle_server_request(
    inner: &Arc<Inner>,
    conn: &Arc<Conn>,
    raw_id: Value,
    method: &str,
    params: Option<&Value>,
) {
    let params = params.cloned().unwrap_or(Value::Null);
    match method {
        "item/commandExecution/requestApproval"
        | "item/fileChange/requestApproval"
        | "item/permissions/requestApproval"
        | "execCommandApproval"
        | "applyPatchApproval" => {
            if let Some((request, kind)) = approval_request(&raw_id, method, &params) {
                let key = raw_id_string(&raw_id);
                let available = normalize_available(params.get("availableDecisions"));
                let pending = PendingApproval {
                    raw_id: raw_id.clone(),
                    kind,
                    available,
                    request: request.clone(),
                };
                inner
                    .sessions
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .approvals
                    .insert(key, pending);
                inner.broadcast(&json!({
                    "type": "permission.asked",
                    "data": permission_event_data(&request),
                }));
                #[cfg(debug_assertions)]
                println!(
                    "[prism] codex 审批请求：{}（{}）",
                    request.action, request.session_id
                );
            } else {
                let _ = write_error_response(conn, &raw_id, -32602, "无法解析审批请求");
            }
        }
        // 问答卡：v1 不拦截，回空答卷让模型继续（不阻塞执行）。
        "item/tool/requestUserInput" => {
            let _ = write_response(conn, &raw_id, json!({ "answers": {} }));
        }
        // MCP 引发的交互请求：礼貌拒绝，避免挂住。
        "mcpServer/elicitation/request" => {
            let _ = write_response(conn, &raw_id, json!({ "action": "decline" }));
        }
        // 其余（鉴权刷新 / 证明生成 / 动态工具等）：仅 Codex 官方客户端可处理。
        _ => {
            #[cfg(debug_assertions)]
            println!("[prism] codex 反向请求暂不支持：{method}");
            let _ = write_error_response(
                conn,
                &raw_id,
                -32601,
                &format!("Prism 暂不支持此请求：{method}"),
            );
        }
    }
}

fn raw_id_string(raw_id: &Value) -> String {
    match raw_id {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// 审批请求 → 前端权限请求形态（解析失败返回 None）。
fn approval_request(
    raw_id: &Value,
    method: &str,
    params: &Value,
) -> Option<(PermissionRequest, ApprovalKind)> {
    let thread_id = params.get("threadId").and_then(Value::as_str).or_else(|| {
        // 旧协议（execCommandApproval 等）没有 threadId 时宽容处理。
        params.get("conversationId").and_then(Value::as_str)
    })?;
    let session_id = session_id_of(thread_id);
    let item_id = params
        .get("itemId")
        .or_else(|| params.get("callId"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let reason = params
        .get("reason")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(str::to_string);
    let (action, resources, kind) = match method {
        "item/commandExecution/requestApproval" | "execCommandApproval" => {
            let command = params.get("command").and_then(Value::as_str);
            let resources: Vec<String> = command.iter().map(|text| text.to_string()).collect();
            ("exec", resources, ApprovalKind::Exec)
        }
        "item/fileChange/requestApproval" | "applyPatchApproval" => {
            let root = params.get("grantRoot").and_then(Value::as_str);
            let resources: Vec<String> = root.iter().map(|text| text.to_string()).collect();
            ("edit", resources, ApprovalKind::FileChange)
        }
        _ => ("permissions", Vec::new(), ApprovalKind::Other),
    };
    let message = reason.or_else(|| {
        (action == "exec")
            .then(|| params.get("command").and_then(Value::as_str).map(str::to_string))
            .flatten()
    });
    Some((
        PermissionRequest {
            id: raw_id_string(raw_id),
            session_id,
            action: action.to_string(),
            resources,
            message,
            source_message_id: None,
            source_call_id: item_id,
        },
        kind,
    ))
}

/// 归一化 `availableDecisions`：字符串项直取，对象项取键名。
fn normalize_available(value: Option<&Value>) -> Vec<String> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| match item {
            Value::String(text) => Some(text.clone()),
            Value::Object(map) => map.keys().next().cloned(),
            _ => None,
        })
        .collect()
}

/// 前端三档 → 引擎决策载荷（按 availableDecisions 回退）。
fn decision_payload(
    kind: ApprovalKind,
    available: &[String],
    decision: &str,
) -> Result<Value, String> {
    if kind == ApprovalKind::Other {
        return Err("该请求类型暂不支持在 Prism 中回复".to_string());
    }
    let has = |name: &str| available.iter().any(|item| item == name);
    let pick = |preferred: &str, fallback: &str| -> String {
        if available.is_empty() || has(preferred) {
            preferred.to_string()
        } else if has(fallback) {
            fallback.to_string()
        } else {
            preferred.to_string()
        }
    };
    let value = match decision {
        "once" => pick("accept", "accept"),
        "always" => pick("acceptForSession", "accept"),
        "reject" => pick("decline", "cancel"),
        other => return Err(format!("不支持的权限决定：{other}")),
    };
    Ok(json!({ "decision": value }))
}

/// 权限请求 → `permission.asked` 事件 data（前端解析的引擎形态）。
fn permission_event_data(request: &PermissionRequest) -> Value {
    json!({
        "id": request.id,
        "sessionID": request.session_id,
        "action": request.action,
        "resources": request.resources,
        "message": request.message,
        "source": {
            "id": request.source_call_id,
            "messageID": request.source_message_id,
        },
    })
}

// ---------------------------------------------------------------- 事件归一化

/// 通知 → 前端事件（可能多条；无对应事件返回空）。
///
/// 纯状态变换 + 事件构造（IO 之外），单元测试直接调用。
fn apply_notification(
    state: &mut SessionState,
    method: &str,
    params: Option<&Value>,
) -> Vec<Value> {
    let params = params.cloned().unwrap_or(Value::Null);
    let thread_id = params
        .get("threadId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let session_id = if thread_id.is_empty() {
        None
    } else {
        Some(session_id_of(&thread_id))
    };

    match method {
        "turn/started" => {
            let Some(session) = session_id else {
                return Vec::new();
            };
            if let Some(turn_id) = params.pointer("/turn/id").and_then(Value::as_str) {
                state
                    .active_turns
                    .insert(thread_id.clone(), turn_id.to_string());
            }
            vec![json!({ "type": "session.execution.started", "data": { "sessionID": session } })]
        }
        "turn/completed" => {
            let Some(session) = session_id else {
                return Vec::new();
            };
            state.active_turns.remove(&thread_id);
            let status = params
                .pointer("/turn/status")
                .and_then(Value::as_str)
                .unwrap_or("completed");
            match status {
                "completed" => {
                    vec![json!({ "type": "session.execution.succeeded", "data": { "sessionID": session } })]
                }
                "interrupted" => {
                    vec![json!({ "type": "session.execution.interrupted", "data": { "sessionID": session } })]
                }
                _ => {
                    let message = params
                        .pointer("/turn/error/message")
                        .and_then(Value::as_str)
                        .filter(|text| !text.trim().is_empty())
                        .unwrap_or("执行失败")
                        .to_string();
                    vec![json!({
                        "type": "session.execution.failed",
                        "data": { "sessionID": session, "error": message },
                    })]
                }
            }
        }
        "item/agentMessage/delta" => {
            let Some(session) = session_id else {
                return Vec::new();
            };
            let Some(delta) = params.get("delta").and_then(Value::as_str) else {
                return Vec::new();
            };
            let item_id = params
                .get("itemId")
                .and_then(Value::as_str)
                .unwrap_or("agent");
            vec![json!({
                "type": "session.text.delta",
                "data": {
                    "sessionID": session,
                    "assistantMessageID": item_id,
                    "ordinal": 0,
                    "delta": delta,
                },
            })]
        }
        "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
            let Some(session) = session_id else {
                return Vec::new();
            };
            let Some(delta) = params.get("delta").and_then(Value::as_str) else {
                return Vec::new();
            };
            let item_id = params
                .get("itemId")
                .and_then(Value::as_str)
                .unwrap_or("reasoning");
            vec![json!({
                "type": "session.reasoning.delta",
                "data": {
                    "sessionID": session,
                    "assistantMessageID": item_id,
                    "ordinal": 0,
                    "delta": delta,
                },
            })]
        }
        "item/started" => {
            let Some(session) = session_id else {
                return Vec::new();
            };
            let item = params.get("item").cloned().unwrap_or(Value::Null);
            let item_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
            match item.get("type").and_then(Value::as_str) {
                Some("commandExecution") => {
                    state
                        .item_output
                        .insert(item_id.to_string(), String::new());
                    let command = item.get("command").and_then(Value::as_str).unwrap_or_default();
                    let cwd = item.get("cwd").and_then(Value::as_str).unwrap_or_default();
                    vec![
                        json!({
                            "type": "session.tool.input.started",
                            "data": {
                                "sessionID": session,
                                "id": item_id,
                                "name": "exec_command",
                                "assistantMessageID": item_id,
                            },
                        }),
                        json!({
                            "type": "session.tool.called",
                            "data": {
                                "sessionID": session,
                                "id": item_id,
                                "assistantMessageID": item_id,
                                "input": { "command": command, "cwd": cwd },
                            },
                        }),
                    ]
                }
                Some("fileChange") => {
                    let files = file_change_files(&item);
                    vec![
                        json!({
                            "type": "session.tool.input.started",
                            "data": {
                                "sessionID": session,
                                "id": item_id,
                                "name": "apply_patch",
                                "assistantMessageID": item_id,
                            },
                        }),
                        json!({
                            "type": "session.tool.called",
                            "data": {
                                "sessionID": session,
                                "id": item_id,
                                "assistantMessageID": item_id,
                                "input": { "files": files },
                            },
                        }),
                    ]
                }
                _ => Vec::new(),
            }
        }
        "item/commandExecution/outputDelta" => {
            let Some(item_id) = params.get("itemId").and_then(Value::as_str) else {
                return Vec::new();
            };
            if let Some(delta) = params.get("delta").and_then(Value::as_str) {
                let entry = state.item_output.entry(item_id.to_string()).or_default();
                if entry.len() < MAX_ITEM_OUTPUT {
                    entry.push_str(delta);
                }
            }
            // 输出增量只做累计（工具卡在完成时取整体输出），不逐条广播。
            Vec::new()
        }
        "item/completed" => {
            let Some(session) = session_id else {
                return Vec::new();
            };
            let item = params.get("item").cloned().unwrap_or(Value::Null);
            let item_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
            match item.get("type").and_then(Value::as_str) {
                Some("agentMessage") => {
                    let text = item.get("text").and_then(Value::as_str).unwrap_or_default();
                    if text.trim().is_empty() {
                        return Vec::new();
                    }
                    vec![json!({
                        "type": "session.text.ended",
                        "data": {
                            "sessionID": session,
                            "assistantMessageID": item_id,
                            "ordinal": 0,
                            "text": text,
                        },
                    })]
                }
                Some("reasoning") => {
                    let summary = item
                        .get("summary")
                        .and_then(Value::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join("")
                        })
                        .unwrap_or_default();
                    if summary.trim().is_empty() {
                        return Vec::new();
                    }
                    vec![json!({
                        "type": "session.reasoning.ended",
                        "data": {
                            "sessionID": session,
                            "assistantMessageID": item_id,
                            "ordinal": 0,
                            "text": summary,
                        },
                    })]
                }
                Some("commandExecution") => {
                    let accumulated = state.item_output.remove(item_id).unwrap_or_default();
                    let output = item
                        .get("aggregatedOutput")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .filter(|text| !text.is_empty())
                        .unwrap_or(accumulated);
                    let exit_code = item.get("exitCode").cloned().unwrap_or(Value::Null);
                    let duration = item.get("durationMs").cloned().unwrap_or(Value::Null);
                    let content = if output.is_empty() {
                        json!([])
                    } else {
                        json!([{ "type": "text", "text": output }])
                    };
                    vec![json!({
                        "type": "session.tool.success",
                        "data": {
                            "sessionID": session,
                            "id": item_id,
                            "assistantMessageID": item_id,
                            "content": content,
                            "metadata": { "exitCode": exit_code, "durationMs": duration },
                        },
                    })]
                }
                Some("fileChange") => {
                    let files = file_change_files(&item);
                    vec![json!({
                        "type": "session.tool.success",
                        "data": {
                            "sessionID": session,
                            "id": item_id,
                            "assistantMessageID": item_id,
                            "content": [],
                            "metadata": { "files": files },
                        },
                    })]
                }
                _ => Vec::new(),
            }
        }
        "error" => {
            let Some(session) = session_id else {
                return Vec::new();
            };
            let message = params
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("引擎错误")
                .to_string();
            vec![json!({
                "type": "session.step.failed",
                "data": { "sessionID": session, "error": message },
            })]
        }
        // 审批在引擎侧被解决（自动放行 / 超时收口等）：收卡。
        "serverRequest/resolved" => {
            let Some(session) = session_id else {
                return Vec::new();
            };
            let request_id = params
                .get("requestId")
                .map(raw_id_string)
                .unwrap_or_default();
            state.approvals.remove(&request_id);
            vec![json!({
                "type": "permission.replied",
                "data": { "sessionID": session, "requestID": request_id },
            })]
        }
        _ => Vec::new(),
    }
}

/// fileChange 条目 → 前端 diff 渲染用的 files（metadata.files[].patch）。
fn file_change_files(item: &Value) -> Vec<Value> {
    item.get("changes")
        .and_then(Value::as_array)
        .map(|changes| {
            changes
                .iter()
                .map(|change| {
                    let path = change
                        .get("path")
                        .or_else(|| change.get("file"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let patch = change
                        .get("diff")
                        .or_else(|| change.get("patch"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    json!({ "path": path, "patch": patch })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event_type(value: &Value) -> &str {
        value.get("type").and_then(Value::as_str).unwrap_or_default()
    }

    fn data_str<'a>(value: &'a Value, key: &str) -> &'a str {
        value
            .pointer(&format!("/data/{key}"))
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    #[test]
    fn maps_turn_lifecycle() {
        let mut state = SessionState::default();
        let events = apply_notification(
            &mut state,
            "turn/started",
            Some(&json!({
                "threadId": "t1",
                "turn": { "id": "turn-1", "status": "inProgress" },
            })),
        );
        assert_eq!(events.len(), 1);
        assert_eq!(event_type(&events[0]), "session.execution.started");
        assert_eq!(data_str(&events[0], "sessionID"), "codex:t1");
        assert_eq!(
            state.active_turns.get("t1").map(String::as_str),
            Some("turn-1")
        );

        let events = apply_notification(
            &mut state,
            "turn/completed",
            Some(&json!({ "threadId": "t1", "turn": { "id": "turn-1", "status": "completed" } })),
        );
        assert_eq!(event_type(&events[0]), "session.execution.succeeded");
        assert!(state.active_turns.is_empty());

        let events = apply_notification(
            &mut state,
            "turn/completed",
            Some(&json!({ "threadId": "t1", "turn": { "id": "turn-2", "status": "interrupted" } })),
        );
        assert_eq!(event_type(&events[0]), "session.execution.interrupted");

        let events = apply_notification(
            &mut state,
            "turn/completed",
            Some(&json!({
                "threadId": "t1",
                "turn": { "id": "turn-3", "status": "failed", "error": { "message": "连接失败" } },
            })),
        );
        assert_eq!(event_type(&events[0]), "session.execution.failed");
        assert_eq!(data_str(&events[0], "error"), "连接失败");
    }

    #[test]
    fn maps_assistant_text_and_reasoning() {
        let mut state = SessionState::default();
        let delta = apply_notification(
            &mut state,
            "item/agentMessage/delta",
            Some(&json!({ "threadId": "t1", "itemId": "msg_1", "delta": "PO" })),
        );
        assert_eq!(event_type(&delta[0]), "session.text.delta");
        assert_eq!(data_str(&delta[0], "assistantMessageID"), "msg_1");
        assert_eq!(data_str(&delta[0], "delta"), "PO");

        let ended = apply_notification(
            &mut state,
            "item/completed",
            Some(&json!({
                "threadId": "t1",
                "item": { "type": "agentMessage", "id": "msg_1", "text": "PONG" },
            })),
        );
        assert_eq!(event_type(&ended[0]), "session.text.ended");
        assert_eq!(data_str(&ended[0], "text"), "PONG");

        let reasoning = apply_notification(
            &mut state,
            "item/reasoning/summaryTextDelta",
            Some(&json!({ "threadId": "t1", "itemId": "rs_1", "delta": "想一下" })),
        );
        assert_eq!(event_type(&reasoning[0]), "session.reasoning.delta");

        let reasoning_done = apply_notification(
            &mut state,
            "item/completed",
            Some(&json!({
                "threadId": "t1",
                "item": { "type": "reasoning", "id": "rs_1", "summary": ["想一下"] },
            })),
        );
        assert_eq!(event_type(&reasoning_done[0]), "session.reasoning.ended");
        assert_eq!(data_str(&reasoning_done[0], "text"), "想一下");
    }

    #[test]
    fn maps_command_execution_items() {
        let mut state = SessionState::default();
        let started = apply_notification(
            &mut state,
            "item/started",
            Some(&json!({
                "threadId": "t1",
                "item": {
                    "type": "commandExecution",
                    "id": "call_1",
                    "command": "/bin/zsh -lc 'echo hi'",
                    "cwd": "/tmp",
                },
            })),
        );
        assert_eq!(started.len(), 2);
        assert_eq!(event_type(&started[0]), "session.tool.input.started");
        assert_eq!(
            started[1]
                .pointer("/data/input/command")
                .and_then(Value::as_str),
            Some("/bin/zsh -lc 'echo hi'")
        );

        // 输出增量累计；完成时聚合输出为空则取累计值。
        apply_notification(
            &mut state,
            "item/commandExecution/outputDelta",
            Some(&json!({ "threadId": "t1", "itemId": "call_1", "delta": "hi\n" })),
        );
        let done = apply_notification(
            &mut state,
            "item/completed",
            Some(&json!({
                "threadId": "t1",
                "item": {
                    "type": "commandExecution",
                    "id": "call_1",
                    "aggregatedOutput": null,
                    "exitCode": 0,
                    "durationMs": 5,
                },
            })),
        );
        assert_eq!(event_type(&done[0]), "session.tool.success");
        assert_eq!(
            done[0]
                .pointer("/data/content/0/text")
                .and_then(Value::as_str),
            Some("hi\n")
        );
        assert_eq!(
            done[0]
                .pointer("/data/metadata/exitCode")
                .and_then(Value::as_i64),
            Some(0)
        );

        // 聚合输出优先于累计。
        let mut state = SessionState::default();
        apply_notification(
            &mut state,
            "item/started",
            Some(&json!({ "threadId": "t1", "item": { "type": "commandExecution", "id": "call_2" } })),
        );
        apply_notification(
            &mut state,
            "item/commandExecution/outputDelta",
            Some(&json!({ "threadId": "t1", "itemId": "call_2", "delta": "累计" })),
        );
        let done = apply_notification(
            &mut state,
            "item/completed",
            Some(&json!({
                "threadId": "t1",
                "item": { "type": "commandExecution", "id": "call_2", "aggregatedOutput": "聚合", "exitCode": 1 },
            })),
        );
        assert_eq!(
            done[0]
                .pointer("/data/content/0/text")
                .and_then(Value::as_str),
            Some("聚合")
        );
        assert_eq!(
            done[0]
                .pointer("/data/metadata/exitCode")
                .and_then(Value::as_i64),
            Some(1)
        );
    }

    #[test]
    fn builds_approval_request_and_event() {
        let raw_id = json!(0);
        let params = json!({
            "threadId": "t1",
            "turnId": "turn-1",
            "itemId": "call_1",
            "reason": "需要写文件来完成任务，可以吗？",
            "command": "/bin/zsh -lc 'touch /tmp/x'",
            "availableDecisions": [
                "accept",
                { "acceptWithExecpolicyAmendment": { "execpolicy_amendment": ["touch"] } },
                "cancel",
            ],
        });
        let (request, kind) =
            approval_request(&raw_id, "item/commandExecution/requestApproval", &params)
                .expect("审批解析");
        assert_eq!(kind, ApprovalKind::Exec);
        assert_eq!(request.id, "0");
        assert_eq!(request.session_id, "codex:t1");
        assert_eq!(request.action, "exec");
        assert_eq!(
            request.message.as_deref(),
            Some("需要写文件来完成任务，可以吗？")
        );
        assert_eq!(request.source_call_id.as_deref(), Some("call_1"));

        let event = permission_event_data(&request);
        assert_eq!(
            event.get("sessionID").and_then(Value::as_str),
            Some("codex:t1")
        );
        assert_eq!(
            event.pointer("/source/id").and_then(Value::as_str),
            Some("call_1")
        );

        let available = normalize_available(params.get("availableDecisions"));
        assert_eq!(
            available,
            vec!["accept", "acceptWithExecpolicyAmendment", "cancel"]
        );
    }

    #[test]
    fn maps_file_change_approval_and_falls_back_to_command_message() {
        let raw_id = json!(1);
        let params = json!({
            "threadId": "t2",
            "itemId": "patch_1",
            "grantRoot": "/Users/me/project",
        });
        let (request, kind) =
            approval_request(&raw_id, "item/fileChange/requestApproval", &params).expect("审批解析");
        assert_eq!(kind, ApprovalKind::FileChange);
        assert_eq!(request.action, "edit");
        assert_eq!(request.resources, vec!["/Users/me/project"]);

        // 命令审批没有 reason 时以命令本身作为标题。
        let params = json!({
            "threadId": "t2",
            "itemId": "call_9",
            "command": "/bin/zsh -lc 'npm install'",
        });
        let (request, _) =
            approval_request(&raw_id, "execCommandApproval", &params).expect("审批解析");
        assert_eq!(request.message.as_deref(), Some("/bin/zsh -lc 'npm install'"));
    }

    #[test]
    fn maps_decisions_with_fallback() {
        let full = vec![
            "accept".to_string(),
            "acceptForSession".to_string(),
            "decline".to_string(),
        ];
        assert_eq!(
            decision_payload(ApprovalKind::Exec, &full, "once").unwrap(),
            json!({ "decision": "accept" })
        );
        assert_eq!(
            decision_payload(ApprovalKind::Exec, &full, "always").unwrap(),
            json!({ "decision": "acceptForSession" })
        );
        assert_eq!(
            decision_payload(ApprovalKind::Exec, &full, "reject").unwrap(),
            json!({ "decision": "decline" })
        );

        // 升级场景只给 accept/cancel：always 回退 accept、reject 回退 cancel。
        let limited = vec!["accept".to_string(), "cancel".to_string()];
        assert_eq!(
            decision_payload(ApprovalKind::Exec, &limited, "always").unwrap(),
            json!({ "decision": "accept" })
        );
        assert_eq!(
            decision_payload(ApprovalKind::Exec, &limited, "reject").unwrap(),
            json!({ "decision": "cancel" })
        );

        assert!(decision_payload(ApprovalKind::Other, &full, "once").is_err());
    }

    #[test]
    fn maps_server_request_resolved() {
        let mut state = SessionState::default();
        state.approvals.insert(
            "3".to_string(),
            PendingApproval {
                raw_id: json!(3),
                kind: ApprovalKind::Exec,
                available: Vec::new(),
                request: PermissionRequest {
                    id: "3".to_string(),
                    session_id: "codex:t1".to_string(),
                    action: "exec".to_string(),
                    resources: Vec::new(),
                    message: None,
                    source_message_id: None,
                    source_call_id: None,
                },
            },
        );
        let events = apply_notification(
            &mut state,
            "serverRequest/resolved",
            Some(&json!({ "threadId": "t1", "requestId": 3 })),
        );
        assert_eq!(event_type(&events[0]), "permission.replied");
        assert_eq!(data_str(&events[0], "requestID"), "3");
        assert!(state.approvals.is_empty());
    }

    #[test]
    fn maps_file_change_items() {
        let mut state = SessionState::default();
        let started = apply_notification(
            &mut state,
            "item/started",
            Some(&json!({
                "threadId": "t1",
                "item": {
                    "type": "fileChange",
                    "id": "patch_1",
                    "changes": [
                        { "path": "src/main.rs", "diff": "@@ -1 +1 @@\n-old\n+new" },
                    ],
                },
            })),
        );
        assert_eq!(started.len(), 2);
        assert_eq!(event_type(&started[0]), "session.tool.input.started");
        assert_eq!(
            started[1]
                .pointer("/data/input/files/0/path")
                .and_then(Value::as_str),
            Some("src/main.rs")
        );

        let done = apply_notification(
            &mut state,
            "item/completed",
            Some(&json!({
                "threadId": "t1",
                "item": {
                    "type": "fileChange",
                    "id": "patch_1",
                    "changes": [{ "path": "src/main.rs", "diff": "@@ -1 +1 @@\n-old\n+new" }],
                },
            })),
        );
        assert_eq!(event_type(&done[0]), "session.tool.success");
        assert_eq!(
            done[0]
                .pointer("/data/metadata/files/0/patch")
                .and_then(Value::as_str),
            Some("@@ -1 +1 @@\n-old\n+new")
        );
    }

    #[test]
    fn session_id_roundtrip() {
        assert_eq!(session_id_of("abc"), "codex:abc");
        assert_eq!(thread_id_of("codex:abc"), Some("abc"));
        assert_eq!(thread_id_of("ses_123"), None);
    }

    // ------------------------------------------------------------ 真机集成

    /// 内嵌假模型：给 app-server 指一个本地 Responses API 端点，
    /// 全链路（拉起进程 / 发消息 / 流式 / 工具 / 审批 / 打断收口）走真
    /// app-server，模型侧完全可控且不消耗用户额度。
    #[test]
    #[ignore]
    fn drives_real_app_server() {
        use std::net::TcpListener;

        // 假模型服务：顺序处理请求即可（codex 每轮一连接）。
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定假模型端口");
        let port = listener.local_addr().expect("端口").port();
        thread::Builder::new()
            .name("prism-fake-model".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    fake_provider_respond(&mut stream);
                }
            })
            .expect("启动假模型线程");

        let args = vec![
            "-c".to_string(),
            "model_provider=\"prismfake\"".to_string(),
            "-c".to_string(),
            format!(
                "model_providers.prismfake={{ name=\"PrismFake\", wire_api=\"responses\", base_url=\"http://127.0.0.1:{port}/v1\" }}"
            ),
        ];
        let runtime = Runtime::unattached_with_args(args);
        let events = runtime.subscribe();

        let cwd = std::env::temp_dir().join(format!("prism-codex-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&cwd).expect("临时目录");
        let cwd_str = cwd.to_string_lossy().to_string();

        let created = runtime.create_session(&cwd_str).expect("新建 Codex 会话");
        assert!(created.session_id.starts_with("codex:"));
        let thread_id = created
            .session_id
            .strip_prefix("codex:")
            .unwrap()
            .to_string();

        // 第一轮：纯文本流（含 reasoning 摘要与增量）。
        runtime
            .send_prompt(&created.session_id, "请回复 PONG")
            .expect("发送第一轮");
        let collected = collect_until(&events, &created.session_id, "session.execution.succeeded", Duration::from_secs(120))
            .expect("第一轮应完成");
        assert!(
            collected.iter().any(|event| {
                event.get("type").and_then(Value::as_str) == Some("session.text.ended")
                    && event.pointer("/data/text").and_then(Value::as_str) == Some("PONG")
            }),
            "应收到文本收口事件：{collected:#?}"
        );
        assert!(
            collected.iter().any(|event| {
                event.get("type").and_then(Value::as_str) == Some("session.reasoning.ended")
            }),
            "应收到思考收口事件"
        );

        // 第二轮：沙箱命令（读路径，自动放行，无审批）。
        runtime
            .send_prompt(&created.session_id, "TOOL 任务：请执行命令")
            .expect("发送第二轮");
        let collected = collect_until(&events, &created.session_id, "session.execution.succeeded", Duration::from_secs(120))
            .expect("第二轮应完成");
        assert!(
            collected.iter().any(|event| {
                event.get("type").and_then(Value::as_str) == Some("session.tool.success")
                    && event
                        .pointer("/data/content/0/text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .contains("prism-tool-ok")
            }),
            "命令输出应回传：{collected:#?}"
        );

        // 第三轮：升级权限命令 → 审批卡 → once 放行 → 命令真实执行。
        let marker = cwd.join("escalated-ok");
        let prompt = format!(
            "TOOL 任务：ESCALATE TOOLCMD=touch {}",
            marker.display()
        );
        runtime
            .send_prompt(&created.session_id, &prompt)
            .expect("发送第三轮");
        let collected =
            collect_until(&events, &created.session_id, "permission.asked", Duration::from_secs(120))
                .expect("应收到审批请求");
        let request_id = collected
            .last()
            .and_then(|event| event.pointer("/data/id"))
            .and_then(Value::as_str)
            .expect("审批请求应带 id")
            .to_string();

        // 补拉端点应能拿到同一请求。
        let pending = runtime
            .list_permissions(&created.session_id)
            .expect("补拉权限列表");
        assert!(
            pending.iter().any(|request| request.id == request_id),
            "补拉列表应包含挂起请求"
        );

        runtime
            .permission_reply(&created.session_id, &request_id, "once")
            .expect("回复审批");
        let collected = collect_until(&events, &created.session_id, "session.execution.succeeded", Duration::from_secs(120))
            .expect("第三轮应完成");
        assert!(
            marker.exists(),
            "放行后命令应写入标记文件；事件：{collected:#?}"
        );
        assert!(
            collected.iter().any(|event| {
                event.get("type").and_then(Value::as_str) == Some("session.tool.success")
            }),
            "审批轮应产出工具完成事件"
        );

        // 清理：归档测试线程（Codex 自带的归档语义），再关停。
        if let Ok(conn) = runtime.ready_conn() {
            let _ = runtime.call_on(
                &conn,
                "thread/archive",
                json!({ "threadId": thread_id }),
                CALL_TIMEOUT,
            );
        }
        runtime.shutdown();
        let _ = std::fs::remove_dir_all(&cwd);
    }

    /// 目录真机冒烟：model/list 返回模型目录（不发模型请求，不耗额度）。
    /// `cargo test --lib -- --ignored --nocapture model_list_real`
    #[test]
    #[ignore]
    fn model_list_real() {
        let runtime = Runtime::unattached_with_args(Vec::new());
        let models = runtime.list_models().expect("model/list");
        println!("模型 {} 个：", models.len());
        for model in &models {
            println!(
                "  {} 档位={:?} 默认档={:?} 默认模型={}",
                model.id,
                model.variants,
                model.default_variant,
                model.is_default
            );
        }
        assert!(!models.is_empty(), "应返回模型目录");
        assert!(models.iter().any(|model| model.is_default), "应有默认模型");
        assert!(
            models.iter().any(|model| !model.variants.is_empty()),
            "应有带推理档位的模型"
        );
        runtime.shutdown();
    }

    /// 组合器真机冒烟：显式模型 + 推理强度落进发给模型的请求（假模型端点，不耗额度）。
    /// `cargo test --lib -- --ignored --nocapture overrides_model_and_effort_real`
    #[test]
    #[ignore]
    fn overrides_model_and_effort_real() {
        use std::net::TcpListener;
        use std::sync::{Arc as StdArc, Mutex as StdMutex};

        let captured: StdArc<StdMutex<Vec<String>>> = StdArc::new(StdMutex::new(Vec::new()));
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定假模型端口");
        let port = listener.local_addr().expect("端口").port();
        let sink = captured.clone();
        thread::Builder::new()
            .name("prism-fake-model".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    fake_provider_respond_sink(&mut stream, Some(&sink));
                }
            })
            .expect("启动假模型线程");

        let args = vec![
            "-c".to_string(),
            "model_provider=\"prismfake\"".to_string(),
            "-c".to_string(),
            format!(
                "model_providers.prismfake={{ name=\"PrismFake\", wire_api=\"responses\", base_url=\"http://127.0.0.1:{port}/v1\" }}"
            ),
        ];
        let runtime = Runtime::unattached_with_args(args);
        let events = runtime.subscribe();

        let cwd = std::env::temp_dir().join(format!("prism-m20-codex-{}", std::process::id()));
        std::fs::create_dir_all(&cwd).expect("临时目录");
        let created = runtime
            .create_session(cwd.to_str().expect("临时目录路径"))
            .expect("新建 Codex 会话");
        let thread_id = created
            .session_id
            .strip_prefix("codex:")
            .expect("codex 前缀")
            .to_string();

        runtime
            .send_prompt_with(
                &created.session_id,
                "请回复 PONG",
                Some("gpt-5.4-mini"),
                Some("low"),
            )
            .expect("带模型/强度发送");
        collect_until(
            &events,
            &created.session_id,
            "session.execution.succeeded",
            Duration::from_secs(120),
        )
        .expect("应完成");

        let bodies = captured.lock().expect("捕获表").clone();
        let last = bodies.last().cloned().unwrap_or_default();
        let preview: String = last.chars().take(1000).collect();
        println!("模型请求体（截断）：{preview}");
        assert!(last.contains("gpt-5.4-mini"), "请求应带显式模型");
        assert!(last.contains("\"effort\":\"low\""), "请求应带推理强度");

        if let Ok(conn) = runtime.ready_conn() {
            let _ = runtime.call_on(
                &conn,
                "thread/archive",
                json!({ "threadId": thread_id }),
                CALL_TIMEOUT,
            );
        }
        runtime.shutdown();
        let _ = std::fs::remove_dir_all(&cwd);
    }

    /// 真机组会话 shell：thread/shellCommand 协议可调用（假模型端点，不耗额度）。
    /// `cargo test --lib -- --ignored --nocapture shell_command_real`
    #[test]
    #[ignore]
    fn shell_command_real() {
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定假模型端口");
        let port = listener.local_addr().expect("端口").port();
        thread::Builder::new()
            .name("prism-fake-model".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    fake_provider_respond(&mut stream);
                }
            })
            .expect("启动假模型线程");

        let args = vec![
            "-c".to_string(),
            "model_provider=\"prismfake\"".to_string(),
            "-c".to_string(),
            format!(
                "model_providers.prismfake={{ name=\"PrismFake\", wire_api=\"responses\", base_url=\"http://127.0.0.1:{port}/v1\" }}"
            ),
        ];
        let runtime = Runtime::unattached_with_args(args);
        let cwd = std::env::temp_dir().join(format!("prism-m20-cshell-{}", std::process::id()));
        std::fs::create_dir_all(&cwd).expect("临时目录");
        let created = runtime
            .create_session(cwd.to_str().expect("临时目录路径"))
            .expect("新建 Codex 会话");
        let thread_id = created
            .session_id
            .strip_prefix("codex:")
            .expect("codex 前缀")
            .to_string();

        runtime
            .shell_command(&created.session_id, "echo prism-shell-ok")
            .expect("thread/shellCommand 应被接受");
        assert!(runtime.shell_command(&created.session_id, "   ").is_err());
        println!("codex shell 已执行");

        if let Ok(conn) = runtime.ready_conn() {
            let _ = runtime.call_on(
                &conn,
                "thread/archive",
                json!({ "threadId": thread_id }),
                CALL_TIMEOUT,
            );
        }
        runtime.shutdown();
        let _ = std::fs::remove_dir_all(&cwd);
    }

    /// 读取事件直到出现目标终止事件（只看目标会话）；超时返回 None。
    fn collect_until(
        receiver: &mpsc::Receiver<Value>,
        session_id: &str,
        done_type: &str,
        timeout: Duration,
    ) -> Option<Vec<Value>> {
        let deadline = std::time::Instant::now() + timeout;
        let mut collected = Vec::new();
        loop {
            let remaining = deadline.checked_duration_since(std::time::Instant::now())?;
            let event = receiver.recv_timeout(remaining).ok()?;
            if event.pointer("/data/sessionID").and_then(Value::as_str) != Some(session_id) {
                continue;
            }
            let is_done = event.get("type").and_then(Value::as_str) == Some(done_type);
            collected.push(event);
            if is_done {
                return Some(collected);
            }
        }
    }

    fn push_event(out: &mut String, value: Value) {
        out.push_str(&format!("data: {value}\n\n"));
    }

    fn extract_marker(text: &str, marker: &str) -> Option<String> {
        let after = text.split(marker).nth(1)?;
        let value = after.lines().next()?.trim();
        (!value.is_empty()).then(|| value.to_string())
    }

    /// 按"最后一条用户消息 + 末项类型"判定剧本（避免历史里的
    /// function_call_output 误触发收尾分支）。
    fn fake_script(body: &str) -> String {
        let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let mut last_user_text = String::new();
        let mut last_item_type = String::new();
        if let Some(items) = parsed.get("input").and_then(Value::as_array) {
            for item in items {
                last_item_type = item
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if item.get("role").and_then(Value::as_str) == Some("user") {
                    if let Some(content) = item.get("content").and_then(Value::as_array) {
                        last_user_text = content
                            .iter()
                            .filter_map(|part| part.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n");
                    }
                }
            }
        }
        let tool_result_last = last_item_type == "function_call_output";

        let mut out = String::new();
        let created = json!({
            "type": "response.created",
            "response": { "id": "resp_fake", "status": "in_progress", "output": [] },
        });
        let completed = json!({
            "type": "response.completed",
            "response": {
                "id": "resp_fake",
                "status": "completed",
                "output": [],
                "usage": { "input_tokens": 12, "output_tokens": 4, "total_tokens": 16 },
            },
        });

        if tool_result_last {
            push_event(&mut out, created);
            push_event(
                &mut out,
                json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "item": {
                        "id": "msg_final",
                        "type": "message",
                        "status": "completed",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": "工具执行完毕。" }],
                    },
                }),
            );
            push_event(&mut out, completed);
            return out;
        }

        if last_user_text.contains("ESCALATE") {
            let cmd =
                extract_marker(&last_user_text, "TOOLCMD=").unwrap_or_else(|| "echo escalated".to_string());
            push_event(&mut out, created);
            push_event(
                &mut out,
                json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "item": {
                        "id": "fc_1",
                        "type": "function_call",
                        "status": "completed",
                        "call_id": "call_fake_1",
                        "name": "exec_command",
                        "arguments": json!({
                            "cmd": cmd,
                            "justification": "需要写文件来完成任务，可以吗？",
                            "sandbox_permissions": "require_escalated",
                        })
                        .to_string(),
                    },
                }),
            );
            push_event(&mut out, completed);
            return out;
        }

        if last_user_text.contains("TOOL") {
            push_event(&mut out, created);
            push_event(
                &mut out,
                json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "item": {
                        "id": "fc_1",
                        "type": "function_call",
                        "status": "completed",
                        "call_id": "call_fake_1",
                        "name": "exec_command",
                        "arguments": json!({ "cmd": "echo prism-tool-ok" }).to_string(),
                    },
                }),
            );
            push_event(&mut out, completed);
            return out;
        }

        // 默认：思考摘要 + 流式 PONG。
        push_event(&mut out, created);
        push_event(
            &mut out,
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": { "id": "rs_1", "type": "reasoning", "summary": [] },
            }),
        );
        push_event(
            &mut out,
            json!({
                "type": "response.reasoning_summary_text.delta",
                "item_id": "rs_1",
                "summary_index": 0,
                "delta": "简单应答即可。",
            }),
        );
        push_event(
            &mut out,
            json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "id": "rs_1",
                    "type": "reasoning",
                    "summary": [{ "type": "summary_text", "text": "简单应答即可。" }],
                },
            }),
        );
        push_event(
            &mut out,
            json!({
                "type": "response.output_item.added",
                "output_index": 1,
                "item": {
                    "id": "msg_1",
                    "type": "message",
                    "status": "in_progress",
                    "role": "assistant",
                    "content": [],
                },
            }),
        );
        for chunk in ["P", "O", "N", "G"] {
            push_event(
                &mut out,
                json!({
                    "type": "response.output_text.delta",
                    "item_id": "msg_1",
                    "output_index": 1,
                    "delta": chunk,
                }),
            );
        }
        push_event(
            &mut out,
            json!({
                "type": "response.output_item.done",
                "output_index": 1,
                "item": {
                    "id": "msg_1",
                    "type": "message",
                    "status": "completed",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "PONG" }],
                },
            }),
        );
        push_event(&mut out, completed);
        out
    }

    fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    /// 最小 HTTP 处理：读完整请求 → 按剧本回 SSE。
    fn fake_provider_respond(stream: &mut std::net::TcpStream) {
        fake_provider_respond_sink(stream, None);
    }

    /// 假模型响应；可按需把请求体抄送进 sink（断言模型/强度等参数用）。
    fn fake_provider_respond_sink(
        stream: &mut std::net::TcpStream,
        sink: Option<&std::sync::Mutex<Vec<String>>>,
    ) {
        use std::io::Read;

        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        let mut header_end: Option<usize> = None;
        let mut content_length = 0usize;
        loop {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(read) => {
                    raw.extend_from_slice(&buf[..read]);
                    if header_end.is_none() {
                        if let Some(pos) = find_subsequence(&raw, b"\r\n\r\n") {
                            header_end = Some(pos + 4);
                            let headers = String::from_utf8_lossy(&raw[..pos]).to_string();
                            for line in headers.lines() {
                                if let Some(value) = line
                                    .to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                {
                                    content_length = value.trim().parse().unwrap_or(0);
                                }
                            }
                        }
                    }
                    if let Some(end) = header_end {
                        if raw.len() >= end + content_length {
                            break;
                        }
                    }
                }
                Err(_) => break,
            }
        }
        let Some(end) = header_end else { return };
        let body = String::from_utf8_lossy(&raw[end..]).to_string();
        if let Some(sink) = sink {
            if let Ok(mut list) = sink.lock() {
                list.push(body.clone());
            }
        }
        let script = fake_script(&body);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
            script.len(),
            script
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    }
}
