//! OpenCode 运行轴：serve 进程、HTTP 调用与 SSE 事件流。
//!
//! 生命周期：
//! - 首次需要时惰性启动 `opencode-cli serve --port 0`（cwd = $HOME），
//!   从 stdout 解析 `server listening on http://127.0.0.1:<port>` 与
//!   `server password <pw>`，之后所有请求走 Basic 认证；
//! - 单一后台线程消费 `GET /api/event` 的 SSE，把事件原样广播
//!   （Tauri 事件 + 进程内订阅者）；
//! - 看门狗脚本在 Prism 退出（含崩溃）后兜底回收 serve 进程。

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::catalog::{self, EngineCatalog};
use crate::engines::core::EngineHealth;
use crate::engines::engine::{CreatedSession as EngineCreatedSession, Engine};

use super::locator;

/// 前端订阅的事件名（Tauri 事件）。
pub const EVENT_NAME: &str = "engine-event";

const SERVER_START_TIMEOUT: Duration = Duration::from_secs(20);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const STREAM_RETRY_DELAY: Duration = Duration::from_secs(2);

/// 运行轴状态（不触发启动）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatus {
    pub running: bool,
    pub base_url: Option<String>,
    pub pid: Option<u32>,
}

/// 新建会话的结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedSession {
    pub session_id: String,
    pub directory: Option<String>,
}

/// 挂起的权限请求（`permission.asked` / 列表端点同形状）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRequest {
    pub id: String,
    pub session_id: String,
    pub action: String,
    pub resources: Vec<String>,
    pub message: Option<String>,
    pub source_message_id: Option<String>,
    pub source_call_id: Option<String>,
}

/// 宽松解析权限请求（缺字段不报错）。
fn parse_permission(value: &Value) -> Option<PermissionRequest> {
    let id = string_field(value, "id")?;
    Some(PermissionRequest {
        id,
        session_id: string_field(value, "sessionID").unwrap_or_default(),
        action: string_field(value, "action").unwrap_or_else(|| "unknown".to_string()),
        resources: value
            .get("resources")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        message: string_field(value, "message").filter(|text| !text.trim().is_empty()),
        source_message_id: value
            .pointer("/source/messageID")
            .and_then(Value::as_str)
            .map(str::to_string),
        source_call_id: value
            .pointer("/source/id")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// 启动横幅的解析结果。
#[derive(Debug, PartialEq)]
enum StartupLine {
    Url(String),
    Password(String),
}

fn parse_startup_line(line: &str) -> Option<StartupLine> {
    let trimmed = line.trim();
    if let Some(url) = trimmed.strip_prefix("server listening on ") {
        return Some(StartupLine::Url(url.trim().to_string()));
    }
    if let Some(password) = trimmed.strip_prefix("server password ") {
        return Some(StartupLine::Password(password.trim().to_string()));
    }
    None
}

/// SSE 行 → JSON 负载（`data:` 前缀）。
fn event_payload(line: &str) -> Option<&str> {
    let payload = line.trim().strip_prefix("data:")?.trim();
    (!payload.is_empty()).then_some(payload)
}

/// 响应外层是 `{ data: … }` 时取 data。
fn unwrap_data(value: Value) -> Value {
    match value.get("data") {
        Some(data) if !data.is_null() => data.clone(),
        _ => value,
    }
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

/// 附件 URI 归一化：绝对路径 → `file://` URI（引擎侧校验要求带 scheme）。
fn to_file_uri(uri: &str) -> String {
    if uri.contains("://") {
        uri.to_string()
    } else {
        format!("file://{uri}")
    }
}

fn truncate(text: &str, max: usize) -> String {
    let mut result: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        result.push('…');
    }
    result
}

// ---------------------------------------------------------------- HTTP

/// 与 serve 通信所需的最小集合（克隆开销小，Client 内部是 Arc）。
#[derive(Clone)]
struct ServerTarget {
    base_url: String,
    password: String,
    client: reqwest::blocking::Client,
    stream_client: reqwest::blocking::Client,
}

impl ServerTarget {
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::blocking::RequestBuilder {
        self.client
            .request(method, format!("{}{}", self.base_url, path))
            .basic_auth("opencode", Some(self.password.clone()))
    }

    fn post_json(&self, path: &str, body: Value) -> Result<Value, String> {
        send_json(self.request(reqwest::Method::POST, path).json(&body), path)
    }

    fn get_json(&self, path: &str) -> Result<Value, String> {
        send_json(self.request(reqwest::Method::GET, path), path)
    }

    #[allow(dead_code)] // 测试清理用
    fn delete(&self, path: &str) -> Result<Value, String> {
        send_json(self.request(reqwest::Method::DELETE, path), path)
    }
}

fn send_json(request: reqwest::blocking::RequestBuilder, path: &str) -> Result<Value, String> {
    let response = request
        .send()
        .map_err(|cause| format!("请求 {path} 失败：{cause}"))?;
    let status = response.status();
    let text = response.text().unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "{path} 返回 {status}：{}",
            truncate(text.trim(), 300)
        ));
    }
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).map_err(|cause| format!("解析 {path} 响应失败：{cause}"))
}

/// 带 `location[directory]` 查询参数的 GET（引擎按目录解析模型/命令/文件）。
fn fetch_location_json(
    target: &ServerTarget,
    path: &str,
    directory: &str,
) -> Result<Value, String> {
    let request = target
        .request(reqwest::Method::GET, path)
        .query(&[("location[directory]", directory)]);
    send_json(request, path)
}

// ---------------------------------------------------------------- 进程

struct ServerHandle {
    child: Child,
    watchdog: Option<Child>,
    /// 该实例的事件线程退出信号。
    stream_shutdown: Arc<AtomicBool>,
    /// 事件流是否已成功连接（冷启动时用于等连接就绪）。
    stream_ready: Arc<AtomicBool>,
    target: ServerTarget,
}

struct RuntimeInner {
    app: Option<AppHandle>,
    server: Mutex<Option<ServerHandle>>,
    /// 进程内事件订阅者（调试与测试用；前端走 Tauri 事件）。
    subscribers: Mutex<Vec<mpsc::Sender<Value>>>,
    shutting_down: AtomicBool,
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

/// 运行轴句柄：Tauri 管理为全局单例，App 内克隆即可跨线程使用。
#[derive(Clone)]
pub struct Runtime {
    inner: Arc<RuntimeInner>,
}

impl Runtime {
    pub fn new(app: AppHandle) -> Self {
        Self::build(Some(app))
    }

    fn build(app: Option<AppHandle>) -> Self {
        Self {
            inner: Arc::new(RuntimeInner {
                app,
                server: Mutex::new(None),
                subscribers: Mutex::new(Vec::new()),
                shutting_down: AtomicBool::new(false),
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn unattached() -> Self {
        Self::build(None)
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

    /// 当前状态（不启动 serve）。
    pub fn status(&self) -> RuntimeStatus {
        let mut slot = self
            .inner
            .server
            .lock()
            .unwrap_or_else(|error| error.into_inner());

        let alive = match slot.as_mut() {
            Some(handle) => match handle.child.try_wait() {
                Ok(None) => {
                    return RuntimeStatus {
                        running: true,
                        base_url: Some(handle.target.base_url.clone()),
                        pid: Some(handle.child.id()),
                    }
                }
                _ => false,
            },
            None => false,
        };
        if !alive {
            if let Some(mut dead) = slot.take() {
                terminate_server(&mut dead);
            }
        }
        RuntimeStatus {
            running: false,
            base_url: None,
            pid: None,
        }
    }

    /// 确保 serve 在跑（必要时启动）；返回可直接发请求的目标。
    fn ensure_server(&self) -> Result<ServerTarget, String> {
        if self.inner.shutting_down.load(Ordering::Relaxed) {
            return Err("运行轴已关闭".to_string());
        }
        let mut slot = self
            .inner
            .server
            .lock()
            .unwrap_or_else(|error| error.into_inner());

        let alive = match slot.as_mut() {
            Some(handle) => handle
                .child
                .try_wait()
                .map(|status| status.is_none())
                .unwrap_or(false),
            None => false,
        };
        if alive {
            return Ok(slot.as_ref().expect("server slot").target.clone());
        }
        if let Some(mut dead) = slot.take() {
            #[cfg(debug_assertions)]
            println!("[prism] serve 进程已退出，重新启动");
            terminate_server(&mut dead);
        }

        let handle = spawn_server()?;
        let target = handle.target.clone();
        let shutdown = handle.stream_shutdown.clone();
        let ready = handle.stream_ready.clone();
        *slot = Some(handle);

        let thread_target = target.clone();
        let inner = self.inner.clone();
        let thread_ready = ready.clone();
        thread::Builder::new()
            .name("prism-events".into())
            .spawn(move || event_loop(inner, thread_target, shutdown, thread_ready))
            .map_err(|cause| format!("启动事件线程失败：{cause}"))?;

        // 等事件流连上，避免冷启动时漏掉紧跟着发起的执行事件。
        let wait_until = Instant::now() + Duration::from_secs(3);
        while !ready.load(Ordering::Relaxed) && Instant::now() < wait_until {
            thread::sleep(Duration::from_millis(25));
        }

        #[cfg(debug_assertions)]
        println!("[prism] serve 已启动：{}", target.base_url);
        Ok(target)
    }

    /// 在指定目录新建会话。
    pub fn create_session(&self, directory: &str) -> Result<CreatedSession, String> {
        let directory = directory.trim();
        if directory.is_empty() {
            return Err("会话目录为空".to_string());
        }
        let target = self.ensure_server()?;
        let response = target.post_json(
            "/api/session",
            json!({ "location": { "directory": directory } }),
        )?;
        let data = unwrap_data(response);
        let session_id =
            string_field(&data, "id").ok_or_else(|| "会话创建响应缺少 id".to_string())?;
        let actual = data
            .pointer("/location/directory")
            .and_then(Value::as_str)
            .map(str::to_string);
        Ok(CreatedSession {
            session_id,
            directory: actual,
        })
    }

    /// 发送一条用户消息，返回用户消息 id。
    pub fn send_prompt(&self, session_id: &str, text: &str) -> Result<String, String> {
        self.send_prompt_rich(session_id, text, &[])
    }

    /// 发送一条用户消息（可带文件附件：uri 为文件绝对路径，name 为展示名）。
    pub fn send_prompt_rich(
        &self,
        session_id: &str,
        text: &str,
        files: &[(String, String)],
    ) -> Result<String, String> {
        let text = text.trim();
        if text.is_empty() && files.is_empty() {
            return Err("消息内容为空".to_string());
        }
        let target = self.ensure_server()?;
        let mut body = json!({ "text": text });
        if !files.is_empty() {
            let attachments: Vec<Value> = files
                .iter()
                .map(|(uri, name)| json!({ "uri": to_file_uri(uri), "name": name }))
                .collect();
            body["files"] = Value::Array(attachments);
        }
        let response = target.post_json(&format!("/api/session/{session_id}/prompt"), body)?;
        let data = unwrap_data(response);
        string_field(&data, "id").ok_or_else(|| "prompt 响应缺少消息 id".to_string())
    }

    /// 可用模型目录（含默认模型标记与推理档位）。
    pub fn list_models(&self, directory: &str) -> Result<Vec<catalog::EngineModel>, String> {
        let target = self.ensure_server()?;
        let value = fetch_location_json(&target, "/api/model", directory)?;
        let data = unwrap_data(value);
        let default_ref = fetch_location_json(&target, "/api/model/default", directory)
            .ok()
            .map(|value| unwrap_data(value));
        Ok(catalog::parse_opencode_models(&data, default_ref.as_ref()))
    }

    /// 斜杠命令列表（内置 + 项目自定义）。
    pub fn list_commands(&self, directory: &str) -> Result<Vec<catalog::EngineCommand>, String> {
        let target = self.ensure_server()?;
        let value = fetch_location_json(&target, "/api/command", directory)?;
        Ok(catalog::parse_opencode_commands(&unwrap_data(value)))
    }

    /// 设置会话模型；variant 为推理档位（空串 / "default" 视为不指定）。
    pub fn set_session_model(
        &self,
        session_id: &str,
        provider: &str,
        model: &str,
        variant: Option<&str>,
    ) -> Result<(), String> {
        let target = self.ensure_server()?;
        let mut reference = json!({ "id": model, "providerID": provider });
        if let Some(variant) = variant.filter(|value| !value.is_empty() && *value != "default") {
            reference["variant"] = json!(variant);
        }
        target.post_json(
            &format!("/api/session/{session_id}/model"),
            json!({ "model": reference }),
        )?;
        Ok(())
    }

    /// 在会话里直接跑一条 shell 命令（不发给模型）。
    pub fn session_shell(&self, session_id: &str, command: &str) -> Result<(), String> {
        let command = command.trim();
        if command.is_empty() {
            return Err("shell 命令为空".to_string());
        }
        let target = self.ensure_server()?;
        target.post_json(
            &format!("/api/session/{session_id}/shell"),
            json!({ "command": command }),
        )?;
        Ok(())
    }

    /// 运行一条斜杠命令（如 init / review），text 为命令参数。
    pub fn session_command(
        &self,
        session_id: &str,
        name: &str,
        text: &str,
    ) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("命令名为空".to_string());
        }
        let target = self.ensure_server()?;
        let response = target.post_json(
            &format!("/api/session/{session_id}/command"),
            json!({ "name": name, "text": text }),
        )?;
        let data = unwrap_data(response);
        Ok(string_field(&data, "id").unwrap_or_default())
    }

    /// 中断当前执行。
    pub fn interrupt(&self, session_id: &str) -> Result<(), String> {
        let target = self.ensure_server()?;
        target.post_json(&format!("/api/session/{session_id}/interrupt"), json!({}))?;
        Ok(())
    }

    /// 当前已在跑的 serve（不触发启动）；进程已死时顺手清理。
    fn current_server(&self) -> Option<ServerTarget> {
        let mut slot = self
            .inner
            .server
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match slot.as_mut() {
            Some(handle) => match handle.child.try_wait() {
                Ok(None) => Some(handle.target.clone()),
                _ => {
                    if let Some(mut dead) = slot.take() {
                        terminate_server(&mut dead);
                    }
                    None
                }
            },
            None => None,
        }
    }

    /// 会话当前挂起的权限请求（选择会话时补拉；运行轴没开就返回空）。
    pub fn list_permissions(&self, session_id: &str) -> Result<Vec<PermissionRequest>, String> {
        let Some(target) = self.current_server() else {
            return Ok(Vec::new());
        };
        let value = target.get_json(&format!("/api/session/{session_id}/permission"))?;
        let data = unwrap_data(value);
        Ok(data
            .as_array()
            .map(|items| items.iter().filter_map(parse_permission).collect())
            .unwrap_or_default())
    }

    /// 回复权限请求：once / always / reject。
    pub fn permission_reply(
        &self,
        session_id: &str,
        request_id: &str,
        decision: &str,
    ) -> Result<(), String> {
        if !matches!(decision, "once" | "always" | "reject") {
            return Err(format!("不支持的权限决定：{decision}"));
        }
        let target = self.ensure_server()?;
        target.post_json(
            &format!("/api/session/{session_id}/permission/{request_id}/reply"),
            json!({ "decision": decision }),
        )?;
        Ok(())
    }

    /// 删除会话（测试清理用）。
    #[allow(dead_code)]
    pub fn delete_session(&self, session_id: &str) -> Result<(), String> {
        let target = self.ensure_server()?;
        target.delete(&format!("/api/session/{session_id}"))?;
        Ok(())
    }

    /// 关停：结束 serve 与事件线程（在 App 退出时调用）。
    pub fn shutdown(&self) {
        self.inner.shutting_down.store(true, Ordering::Relaxed);
        let mut slot = self
            .inner
            .server
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(mut handle) = slot.take() {
            terminate_server(&mut handle);
        }
        self.inner
            .subscribers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
    }
}

impl Engine for Runtime {
    fn id(&self) -> &'static str {
        locator::ENGINE_ID
    }

    fn display_name(&self) -> &'static str {
        locator::DISPLAY_NAME
    }

    fn health(&self) -> EngineHealth {
        let db = locator::database_path();
        let cli = locator::cli_path();
        EngineHealth {
            engine_id: locator::ENGINE_ID.to_string(),
            display_name: locator::DISPLAY_NAME.to_string(),
            ready: db.is_some() && cli.is_some(),
            database_path: db.map(|path| path.display().to_string()),
            cli_path: cli.map(|path| path.display().to_string()),
        }
    }

    fn create_session(&self, directory: &str) -> Result<EngineCreatedSession, String> {
        let created = Runtime::create_session(self, directory)?;
        Ok(EngineCreatedSession::new(
            created.session_id,
            created.directory,
        ))
    }

    fn catalog(&self, directory: &str) -> Result<EngineCatalog, String> {
        let models = Runtime::list_models(self, directory)?;
        let commands = Runtime::list_commands(self, directory).unwrap_or_default();
        Ok(EngineCatalog {
            engine: "opencode".to_string(),
            available: true,
            reason: None,
            models,
            commands,
        })
    }

    fn subscribe(&self) -> mpsc::Receiver<Value> {
        Runtime::subscribe(self)
    }

    fn send_prompt(&self, session_id: &str, text: &str) -> Result<String, String> {
        Runtime::send_prompt(self, session_id, text)
    }

    /// OpenCode 的模型挂在会话上（`/api/session/{id}/model`），所以要先设再发。
    ///
    /// 模型值约定写成 `provider/model`：OpenCode 内部是两级引用，只给 model id
    /// 时无法确定 provider（不同 provider 有同名模型），这里不做猜测。
    fn send_prompt_with_model(
        &self,
        session_id: &str,
        text: &str,
        model: Option<&str>,
    ) -> Result<String, String> {
        if let Some(model) = model.map(str::trim).filter(|value| !value.is_empty()) {
            let (provider, id) = model.split_once('/').ok_or_else(|| {
                format!("OpenCode 的模型要写成 provider/model（收到的是 {model}）")
            })?;
            self.set_session_model(session_id, provider.trim(), id.trim(), None)?;
        }
        self.send_prompt_rich(session_id, text, &[])
    }

    fn interrupt(&self, session_id: &str) -> Result<(), String> {
        Runtime::interrupt(self, session_id)
    }

    fn shutdown(&self) {
        Runtime::shutdown(self);
    }
}

fn spawn_server() -> Result<ServerHandle, String> {
    let cli =
        locator::cli_path().ok_or_else(|| "未找到 opencode-cli，无法启动运行轴".to_string())?;

    let mut command = Command::new(&cli);
    command
        .arg("serve")
        .arg("--port")
        .arg("0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(home) = std::env::var_os("HOME") {
        command.current_dir(home);
    }
    let mut child = command
        .spawn()
        .map_err(|cause| format!("启动 opencode serve 失败：{cause}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "serve stdout 不可用".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "serve stderr 不可用".to_string())?;

    let (sender, receiver) = mpsc::channel::<StartupLine>();
    thread::Builder::new()
        .name("prism-serve-out".into())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some(parsed) = parse_startup_line(&line) {
                    let _ = sender.send(parsed);
                }
                #[cfg(debug_assertions)]
                {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        println!("[prism] serve: {trimmed}");
                    }
                }
            }
        })
        .map_err(|cause| format!("启动输出线程失败：{cause}"))?;
    thread::Builder::new()
        .name("prism-serve-err".into())
        .spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                #[cfg(debug_assertions)]
                {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        println!("[prism] serve (stderr): {trimmed}");
                    }
                }
                let _ = line;
            }
        })
        .map_err(|cause| format!("启动错误输出线程失败：{cause}"))?;

    let mut base_url: Option<String> = None;
    let mut password: Option<String> = None;
    let deadline = Instant::now() + SERVER_START_TIMEOUT;
    while (base_url.is_none() || password.is_none()) && Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok(StartupLine::Url(url)) => base_url = Some(url),
            Ok(StartupLine::Password(value)) => password = Some(value),
            Err(_) => break,
        }
    }
    let (Some(base_url), Some(password)) = (base_url, password) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("OpenCode serve 启动超时（未解析到监听地址）".to_string());
    };

    let client = reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|cause| format!("构建 HTTP 客户端失败：{cause}"))?;
    let stream_client = reqwest::blocking::Client::builder()
        .build()
        .map_err(|cause| format!("构建事件流客户端失败：{cause}"))?;

    let watchdog = spawn_watchdog(child.id());
    Ok(ServerHandle {
        child,
        watchdog,
        stream_shutdown: Arc::new(AtomicBool::new(false)),
        stream_ready: Arc::new(AtomicBool::new(false)),
        target: ServerTarget {
            base_url,
            password,
            client,
            stream_client,
        },
    })
}

/// 看门狗：Prism 进程消失后回收 serve，避免异常退出时泄漏。
fn spawn_watchdog(server_pid: u32) -> Option<Child> {
    let prism_pid = std::process::id();
    let script = format!(
        "while kill -0 {prism_pid} 2>/dev/null; do sleep 2; done; kill {server_pid} 2>/dev/null"
    );
    Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

fn terminate_server(handle: &mut ServerHandle) {
    handle.stream_shutdown.store(true, Ordering::Relaxed);
    if let Some(watchdog) = handle.watchdog.as_mut() {
        let _ = watchdog.kill();
        let _ = watchdog.wait();
    }
    let _ = handle.child.kill();
    let _ = handle.child.wait();
}

// ---------------------------------------------------------------- 事件流

fn event_loop(
    inner: Arc<RuntimeInner>,
    target: ServerTarget,
    shutdown: Arc<AtomicBool>,
    ready: Arc<AtomicBool>,
) {
    while !shutdown.load(Ordering::Relaxed) && !inner.shutting_down.load(Ordering::Relaxed) {
        let request = target
            .stream_client
            .get(format!("{}/api/event", target.base_url))
            .basic_auth("opencode", Some(target.password.clone()));
        match request.send() {
            Ok(response) if response.status().is_success() => {
                ready.store(true, Ordering::Relaxed);
                read_event_stream(&inner, response, &shutdown);
                thread::sleep(STREAM_RETRY_DELAY);
            }
            Ok(_response) => {
                #[cfg(debug_assertions)]
                println!("[prism] 事件流被拒绝：{}", _response.status());
                thread::sleep(STREAM_RETRY_DELAY);
            }
            Err(_cause) => {
                #[cfg(debug_assertions)]
                println!("[prism] 事件流连接失败：{_cause}");
                thread::sleep(STREAM_RETRY_DELAY);
            }
        }
    }
}

fn read_event_stream(
    inner: &RuntimeInner,
    response: reqwest::blocking::Response,
    shutdown: &AtomicBool,
) {
    let reader = BufReader::new(response);
    for line in reader.lines() {
        if shutdown.load(Ordering::Relaxed) || inner.shutting_down.load(Ordering::Relaxed) {
            break;
        }
        let Ok(line) = line else { break };
        let Some(payload) = event_payload(&line) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(payload) else {
            continue;
        };
        #[cfg(debug_assertions)]
        log_event(&value);
        inner.broadcast(&value);
    }
}

/// 只打印会话级事件，避免 delta 刷屏。
#[cfg(debug_assertions)]
fn log_event(value: &Value) {
    const LOGGED: [&str; 8] = [
        "session.execution.started",
        "session.execution.succeeded",
        "session.execution.interrupted",
        "session.execution.failed",
        "session.step.failed",
        "session.tool.called",
        "session.tool.success",
        "session.inbox.enqueued",
    ];
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("?");
    if !LOGGED.contains(&kind) {
        return;
    }
    let session = value
        .pointer("/data/sessionID")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let extra = value
        .pointer("/data/name")
        .and_then(Value::as_str)
        .or_else(|| value.pointer("/data/id").and_then(Value::as_str))
        .unwrap_or("");
    println!("[prism] 事件 {kind} session={session} {extra}");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试收尾：引擎没有"删除项目"API，这里把临时目录的项目行一并清掉，
    /// 避免测试在用户引擎库里留下孤儿项目（仅开发期测试的清理，产品代码不写引擎数据）。
    fn cleanup_project(worktree: &std::path::Path) -> usize {
        let Some(db_path) = locator::database_path() else {
            return 0;
        };
        let canonical = std::fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_path_buf());
        let Ok(connection) = rusqlite::Connection::open(&db_path) else {
            return 0;
        };
        let _ = connection.execute_batch("PRAGMA foreign_keys = ON");
        connection
            .execute(
                "DELETE FROM project WHERE worktree = ?1",
                [canonical.to_string_lossy().to_string()],
            )
            .unwrap_or(0)
    }

    #[test]
    fn parses_startup_lines() {
        assert_eq!(
            parse_startup_line("server listening on http://127.0.0.1:41733"),
            Some(StartupLine::Url("http://127.0.0.1:41733".to_string()))
        );
        assert_eq!(
            parse_startup_line("server password s3cret-pw"),
            Some(StartupLine::Password("s3cret-pw".to_string()))
        );
        assert_eq!(parse_startup_line("something else"), None);
    }

    #[test]
    fn extracts_event_payload() {
        assert_eq!(
            event_payload("data: {\"type\":\"x\"}"),
            Some("{\"type\":\"x\"}")
        );
        assert_eq!(
            event_payload("data:{\"type\":\"x\"}"),
            Some("{\"type\":\"x\"}")
        );
        assert_eq!(event_payload("event: ping"), None);
        assert_eq!(event_payload("data:   "), None);
    }

    #[test]
    fn unwrap_prefers_data_and_falls_back() {
        let value = json!({ "data": { "id": "ses_1" } });
        assert_eq!(
            string_field(&unwrap_data(value), "id").as_deref(),
            Some("ses_1")
        );

        let value = json!({ "id": "ses_2" });
        assert_eq!(
            string_field(&unwrap_data(value), "id").as_deref(),
            Some("ses_2")
        );

        let value = json!({ "data": null, "id": "ses_3" });
        assert_eq!(
            string_field(&unwrap_data(value), "id").as_deref(),
            Some("ses_3")
        );
    }

    #[test]
    fn truncate_behaviour() {
        assert_eq!(truncate("abc", 5), "abc");
        assert_eq!(truncate("abcdef", 3), "abc…");
    }

    #[test]
    fn normalizes_file_uri() {
        assert_eq!(to_file_uri("/a/b.txt"), "file:///a/b.txt");
        assert_eq!(to_file_uri("file:///x/y"), "file:///x/y");
    }

    #[test]
    fn parses_permission_request() {
        // permission.asked 的真实字段形状；id 一律用假值，别贴真机会话
        let value = json!({
            "action": "read",
            "id": "per_test0000aaaa0000bbbb0000",
            "resources": [".env"],
            "save": ["*"],
            "sessionID": "ses_test0000aaaa0000bbbb0000",
            "source": {
                "id": "call-00000000-0000-4000-8000-000000000000",
                "messageID": "msg_test0000aaaa0000bbbb0000",
                "type": "tool"
            }
        });
        let request = parse_permission(&value).expect("应可解析");
        assert_eq!(request.id, "per_test0000aaaa0000bbbb0000");
        assert_eq!(request.action, "read");
        assert_eq!(request.resources, vec![".env".to_string()]);
        assert!(request.message.is_none());
        assert_eq!(
            request.source_call_id.as_deref(),
            Some("call-00000000-0000-4000-8000-000000000000")
        );
        assert_eq!(
            request.source_message_id.as_deref(),
            Some("msg_test0000aaaa0000bbbb0000")
        );

        // 缺 id 无法成请求；缺字段则宽松兜底
        assert!(parse_permission(&json!({ "action": "read" })).is_none());
        let minimal = parse_permission(&json!({ "id": "per_1" })).expect("应可解析");
        assert!(minimal.resources.is_empty());
        assert_eq!(minimal.action, "unknown");
    }

    /// 真机端到端（需本机 CLI 与可用模型）：
    /// `cargo test --lib -- --ignored --nocapture drives_real_session`
    #[test]
    #[ignore]
    fn drives_real_session() {
        let runtime = Runtime::unattached();
        let events = runtime.subscribe();

        let dir = std::env::temp_dir().join(format!("prism-m3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        let created = runtime
            .create_session(dir.to_str().expect("临时目录路径"))
            .expect("create_session");
        println!("会话: {}", created.session_id);

        let message_id = runtime
            .send_prompt(&created.session_id, "请只回复 PONG，不要输出其他任何内容。")
            .expect("send_prompt");
        println!("用户消息: {message_id}");

        let deadline = Instant::now() + Duration::from_secs(120);
        let mut seen: Vec<String> = Vec::new();
        let mut succeeded = false;
        let mut interrupted = false;
        while Instant::now() < deadline {
            let Ok(event) = events.recv_timeout(Duration::from_millis(500)) else {
                continue;
            };
            let kind = event
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let session = event
                .pointer("/data/sessionID")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if session != created.session_id {
                continue;
            }
            if !seen.contains(&kind) {
                println!("  事件: {kind}");
                seen.push(kind.clone());
            }
            match kind.as_str() {
                "session.execution.succeeded" => {
                    succeeded = true;
                    break;
                }
                "session.execution.interrupted" => {
                    interrupted = true;
                    break;
                }
                "session.execution.failed" | "session.step.failed" => break,
                _ => {}
            }
        }
        assert!(!interrupted, "执行意外中断，已见事件：{seen:?}");
        assert!(succeeded, "未见 execution.succeeded，已见事件：{seen:?}");
        assert!(
            seen.iter().any(|kind| kind.starts_with("session.text.")),
            "未见文本流事件：{seen:?}"
        );

        // DB 侧核验：助手消息已完成且包含 PONG（写入可能略有延迟，重试）。
        let db_path = locator::database_path().expect("opencode.db");
        let mut assistant_text = None;
        for _ in 0..20 {
            let page = crate::engines::opencode::db::read_timeline(
                &db_path,
                &created.session_id,
                None,
                50,
            )
            .expect("read_timeline");
            let found = page.records.iter().rev().find_map(|record| {
                (record.role == "assistant" && record.completed).then(|| {
                    record
                        .parts
                        .iter()
                        .filter_map(|part| match part {
                            crate::engines::core::TimelinePart::Text { text } => {
                                Some(text.as_str())
                            }
                            _ => None,
                        })
                        .collect::<String>()
                })
            });
            if let Some(text) = found.filter(|text| !text.trim().is_empty()) {
                assistant_text = Some(text);
                break;
            }
            thread::sleep(Duration::from_millis(300));
        }
        let assistant_text = assistant_text.expect("未在 DB 中看到已完成的助手消息");
        println!("助手回复: {}", assistant_text.trim());
        assert!(
            assistant_text.contains("PONG"),
            "回复不含 PONG：{assistant_text}"
        );

        runtime
            .delete_session(&created.session_id)
            .expect("delete_session");
        runtime.shutdown();
        let removed = cleanup_project(&dir);
        println!("已清理测试项目行：{removed}");
        assert_eq!(removed, 1, "应恰好清理一条测试项目行");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 中断路径真机验证：发一个长输出任务，等 execution.started 后调 interrupt，
    /// 断言中断请求被接受且最终落在一个终态事件上。
    /// `cargo test --lib -- --ignored --nocapture interrupts_running_execution`
    #[test]
    #[ignore]
    fn interrupts_running_execution() {
        let runtime = Runtime::unattached();
        let events = runtime.subscribe();

        let dir = std::env::temp_dir().join(format!("prism-m3-stop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        let created = runtime
            .create_session(dir.to_str().expect("临时目录路径"))
            .expect("create_session");

        runtime
            .send_prompt(
                &created.session_id,
                "请从 1 数到 300，每行一个数字，不要省略任何数字。",
            )
            .expect("send_prompt");

        let matches_session = |event: &Value| {
            event.pointer("/data/sessionID").and_then(Value::as_str)
                == Some(created.session_id.as_str())
        };

        // 等执行开始（给中断留出真实窗口）。
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut started = false;
        while Instant::now() < deadline {
            let Ok(event) = events.recv_timeout(Duration::from_millis(300)) else {
                continue;
            };
            if !matches_session(&event) {
                continue;
            }
            if event.get("type").and_then(Value::as_str) == Some("session.execution.started") {
                started = true;
                break;
            }
        }
        assert!(started, "未见 execution.started");

        runtime
            .interrupt(&created.session_id)
            .expect("interrupt 请求失败");

        let deadline = Instant::now() + Duration::from_secs(60);
        let mut outcome = String::new();
        while Instant::now() < deadline {
            let Ok(event) = events.recv_timeout(Duration::from_millis(300)) else {
                continue;
            };
            if !matches_session(&event) {
                continue;
            }
            let kind = event
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if matches!(
                kind,
                "session.execution.interrupted"
                    | "session.execution.succeeded"
                    | "session.execution.failed"
            ) {
                outcome = kind.to_string();
                break;
            }
        }
        println!("中断后终态：{outcome}");
        assert!(!outcome.is_empty(), "中断后未见终态事件");

        // 中断后 DB 仍可读且包含用户消息。
        let db_path = locator::database_path().expect("opencode.db");
        let page =
            crate::engines::opencode::db::read_timeline(&db_path, &created.session_id, None, 50)
                .expect("read_timeline");
        println!("中断后记录数：{}", page.records.len());
        assert!(!page.records.is_empty(), "中断后应仍有历史记录");

        runtime
            .delete_session(&created.session_id)
            .expect("delete_session");
        runtime.shutdown();
        let removed = cleanup_project(&dir);
        println!("已清理测试项目行：{removed}");
        assert_eq!(removed, 1, "应恰好清理一条测试项目行");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 真机权限流：读 .env 触发 permission.asked → 列表端点可见 → 回复 once →
    /// 引擎继续执行读出内容；再读一次回复 reject 验证拒绝路径可收口。
    /// `cargo test --lib -- --ignored --nocapture permission_flow_real`
    #[test]
    #[ignore]
    fn permission_flow_real() {
        let runtime = Runtime::unattached();
        let events = runtime.subscribe();

        let dir = std::env::temp_dir().join(format!("prism-m4-perm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        // 引擎默认权限规则：读 *.env 会触发 ask（其余默认 allow）。
        std::fs::write(dir.join(".env"), "SECRET=42\n").expect("写 .env 探测文件");
        let created = runtime
            .create_session(dir.to_str().expect("临时目录路径"))
            .expect("create_session");
        let sid = created.session_id.clone();
        println!("会话: {sid}");

        runtime
            .send_prompt(
                &sid,
                "请用 read 工具读取当前目录下的 .env 文件，然后告诉我 SECRET 的值。",
            )
            .expect("send_prompt");

        // 等权限请求（触发后引擎会阻塞在这一步，直到回复）
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut asked: Option<PermissionRequest> = None;
        while Instant::now() < deadline {
            let Ok(event) = events.recv_timeout(Duration::from_millis(500)) else {
                continue;
            };
            if event.pointer("/data/sessionID").and_then(Value::as_str) != Some(sid.as_str()) {
                continue;
            }
            match event.get("type").and_then(Value::as_str) {
                Some("permission.asked") => {
                    asked = parse_permission(&event["data"]);
                    break;
                }
                Some("session.execution.succeeded") | Some("session.execution.failed") => break,
                _ => {}
            }
        }
        let asked = asked.expect("读 .env 应触发 permission.asked");
        println!("权限请求: {asked:?}");
        assert_eq!(asked.action, "read");
        assert_eq!(asked.session_id, sid);
        assert!(asked.resources.iter().any(|item| item.ends_with(".env")));
        assert!(asked.source_call_id.is_some(), "应带来源工具调用 id");

        // 列表端点（前端选择会话时的补拉路径）应能看到同一条
        let pending = runtime.list_permissions(&sid).expect("list_permissions");
        assert!(
            pending.iter().any(|item| item.id == asked.id),
            "列表端点应包含刚触发的请求：{pending:?}"
        );
        // 非法决定值应在本地被拦下
        assert!(runtime.permission_reply(&sid, &asked.id, "maybe").is_err());

        // 回复 once → 引擎继续：工具成功、执行收口
        runtime
            .permission_reply(&sid, &asked.id, "once")
            .expect("permission_reply once");

        let deadline = Instant::now() + Duration::from_secs(120);
        let mut tool_success = false;
        let mut terminal: Option<String> = None;
        while Instant::now() < deadline {
            let Ok(event) = events.recv_timeout(Duration::from_millis(500)) else {
                continue;
            };
            if event.pointer("/data/sessionID").and_then(Value::as_str) != Some(sid.as_str()) {
                continue;
            }
            match event.get("type").and_then(Value::as_str) {
                Some("session.tool.success") => tool_success = true,
                Some(
                    kind @ ("session.execution.succeeded"
                    | "session.execution.failed"
                    | "session.execution.interrupted"),
                ) => {
                    terminal = Some(kind.to_string());
                    break;
                }
                _ => {}
            }
        }
        assert_eq!(terminal.as_deref(), Some("session.execution.succeeded"));
        assert!(tool_success, "授权后读工具应执行成功");

        // DB 核验：助手最终答出 SECRET 的值
        let db_path = locator::database_path().expect("opencode.db");
        let mut answer: Option<String> = None;
        for _ in 0..20 {
            let page = crate::engines::opencode::db::read_timeline(&db_path, &sid, None, 50)
                .expect("read_timeline");
            let found = page.records.iter().rev().find_map(|record| {
                (record.role == "assistant" && record.completed).then(|| {
                    record
                        .parts
                        .iter()
                        .filter_map(|part| match part {
                            crate::engines::core::TimelinePart::Text { text } => {
                                Some(text.as_str())
                            }
                            _ => None,
                        })
                        .collect::<String>()
                })
            });
            if let Some(text) = found.filter(|text| text.contains("42")) {
                answer = Some(text);
                break;
            }
            thread::sleep(Duration::from_millis(300));
        }
        println!(
            "助手回复: {}",
            answer.clone().unwrap_or_default().trim().replace('\n', " ")
        );
        assert!(answer.is_some(), "授权后应读到 SECRET=42");

        // 再读一次（once 不落盘，会再次 ask）→ 回复 reject，验证拒绝路径能收口。
        runtime
            .send_prompt(&sid, "请再次用 read 工具读取 .env 文件，告诉我里面有几行。")
            .expect("send_prompt 2");

        let deadline = Instant::now() + Duration::from_secs(120);
        let mut rejections = 0;
        let mut second_asked = false;
        let mut terminal2: Option<String> = None;
        while Instant::now() < deadline {
            let Ok(event) = events.recv_timeout(Duration::from_millis(500)) else {
                continue;
            };
            if event.pointer("/data/sessionID").and_then(Value::as_str) != Some(sid.as_str()) {
                continue;
            }
            match event.get("type").and_then(Value::as_str) {
                Some("permission.asked") => {
                    let request_id = event
                        .pointer("/data/id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    second_asked = true;
                    rejections += 1;
                    println!("第 {rejections} 次权限请求，回复 reject");
                    runtime
                        .permission_reply(&sid, &request_id, "reject")
                        .expect("permission_reply reject");
                    // 模型可能再试；回复数封顶，避免测试卡死
                    assert!(rejections <= 5, "拒绝后不应无限重试");
                }
                Some(
                    kind @ ("session.execution.succeeded"
                    | "session.execution.failed"
                    | "session.execution.interrupted"),
                ) => {
                    terminal2 = Some(kind.to_string());
                    break;
                }
                _ => {}
            }
        }
        assert!(second_asked, "第二次读取应再次申请权限（once 不落盘）");
        println!("拒绝路径终态: {terminal2:?}");
        assert!(terminal2.is_some(), "拒绝后引擎应仍能走到终态");
        if let Some(db_path) = locator::database_path() {
            if let Ok(page) = crate::engines::opencode::db::read_timeline(&db_path, &sid, None, 50)
            {
                let brief = page
                    .records
                    .iter()
                    .rev()
                    .find(|record| record.role == "assistant")
                    .map(|record| {
                        record
                            .parts
                            .iter()
                            .filter_map(|part| match part {
                                crate::engines::core::TimelinePart::Text { text } => {
                                    Some(text.as_str())
                                }
                                _ => None,
                            })
                            .collect::<String>()
                    })
                    .unwrap_or_default();
                let short: String = brief.chars().take(80).collect();
                println!("拒绝后助手回复: {}", short.replace('\n', " "));
            }
        }

        runtime.delete_session(&sid).expect("delete_session");
        runtime.shutdown();
        let removed = cleanup_project(&dir);
        assert_eq!(removed, 1, "应恰好清理一条测试项目行");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 真机插话：执行中追发消息 → 同一执行内下一步送达（inbox.enqueued →
    /// inbox.delivered），插话与答复在 DB 各成记录。
    /// `cargo test --lib -- --ignored --nocapture steers_running_execution`
    #[test]
    #[ignore]
    fn steers_running_execution() {
        let runtime = Runtime::unattached();
        let events = runtime.subscribe();

        let dir = std::env::temp_dir().join(format!("prism-m4-steer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        let created = runtime
            .create_session(dir.to_str().expect("临时目录路径"))
            .expect("create_session");
        let sid = created.session_id.clone();
        println!("会话: {sid}");

        runtime
            .send_prompt(&sid, "请从 1 数到 150，每行一个数字，不要省略任何数字。")
            .expect("send_prompt 1");

        // 等执行开始 + 已有流式输出
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut started = false;
        let mut deltas = 0;
        while Instant::now() < deadline {
            let Ok(event) = events.recv_timeout(Duration::from_millis(300)) else {
                continue;
            };
            if event.pointer("/data/sessionID").and_then(Value::as_str) != Some(sid.as_str()) {
                continue;
            }
            let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
            if kind == "session.execution.started" {
                started = true;
            }
            if kind == "session.text.delta" {
                deltas += 1;
            }
            if started && deltas > 15 {
                break;
            }
        }
        assert!(started, "未见执行开始");
        println!("执行中（已收 {deltas} 个 delta），现在插话");

        let steered_id = runtime
            .send_prompt(&sid, "插话：不用数了，请立刻停下，只回复『收到』两个字。")
            .expect("send_prompt 2");
        println!("插话消息 id: {steered_id}");

        // 观察：插话应由当前执行在下一步边界消化（不应另起 execution.started）
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut executions = 0;
        let mut enqueued = false;
        let mut delivered = false;
        let mut terminal = String::new();
        while Instant::now() < deadline {
            let Ok(event) = events.recv_timeout(Duration::from_millis(500)) else {
                continue;
            };
            if event.pointer("/data/sessionID").and_then(Value::as_str) != Some(sid.as_str()) {
                continue;
            }
            let kind = event.get("type").and_then(Value::as_str).unwrap_or("?");
            let inbox = event
                .pointer("/data/inboxID")
                .and_then(Value::as_str)
                .unwrap_or("");
            match kind {
                "session.execution.started" => executions += 1,
                "session.inbox.enqueued" if inbox == steered_id => enqueued = true,
                "session.inbox.delivered" if inbox == steered_id => delivered = true,
                "session.execution.succeeded"
                | "session.execution.failed"
                | "session.execution.interrupted" => {
                    terminal = kind.to_string();
                    break;
                }
                _ => {}
            }
        }
        assert!(enqueued, "插话应入队（inbox.enqueued）");
        assert!(delivered, "插话应送达（inbox.delivered）");
        assert_eq!(terminal, "session.execution.succeeded");
        assert_eq!(executions, 0, "插话应由当前执行消化，不应另起执行");
        println!("终态: {terminal}");

        // DB 侧：插话消息与答复各成记录（用户/助手 × 2 = 4 条）
        let db_path = locator::database_path().expect("opencode.db");
        let mut records = Vec::new();
        for _ in 0..20 {
            let page = crate::engines::opencode::db::read_timeline(&db_path, &sid, None, 50)
                .expect("read_timeline");
            if page.records.len() >= 4 {
                records = page.records;
                break;
            }
            thread::sleep(Duration::from_millis(300));
        }
        assert!(
            records.len() >= 4,
            "应有 4 条记录（用户/助手 × 2），实得 {}",
            records.len()
        );
        let steered_record = records
            .iter()
            .find(|record| record.id == steered_id)
            .expect("插话消息应落库");
        assert_eq!(steered_record.role, "user");
        let last_assistant = records
            .iter()
            .rev()
            .find(|record| record.role == "assistant")
            .expect("应有助手答复");
        let text = last_assistant
            .parts
            .iter()
            .filter_map(|part| match part {
                crate::engines::core::TimelinePart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert!(text.contains("收到"), "插话后助手应回应『收到』：{text}");

        runtime.delete_session(&sid).expect("delete_session");
        runtime.shutdown();
        let removed = cleanup_project(&dir);
        assert_eq!(removed, 1, "应恰好清理一条测试项目行");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 真机目录：模型/命令列表接口（组合器数据源）。
    /// `cargo test --lib -- --ignored --nocapture lists_models_and_commands_real`
    #[test]
    #[ignore]
    fn lists_models_and_commands_real() {
        let runtime = Runtime::unattached();
        let dir = std::env::var("HOME").expect("HOME");
        let models = runtime.list_models(&dir).expect("list_models");
        println!("模型 {} 个：", models.len());
        for model in models.iter().take(8) {
            println!(
                "  {}/{} v={:?} default={}",
                model.provider.clone().unwrap_or_default(),
                model.id,
                model.variants,
                model.is_default
            );
        }
        assert!(!models.is_empty(), "应有模型");
        assert!(
            models.iter().any(|model| model.is_default),
            "应有一个默认模型"
        );
        let commands = runtime.list_commands(&dir).expect("list_commands");
        println!("命令：{commands:?}");
        assert!(
            commands.iter().any(|command| command.name == "init"),
            "内置 init 命令应可见：{commands:?}"
        );
        runtime.shutdown();
    }

    /// 真机附件：带 files 的 prompt 能收口成功，助手能答出附件内容。
    /// `cargo test --lib -- --ignored --nocapture sends_file_attachment_real`
    #[test]
    #[ignore]
    fn sends_file_attachment_real() {
        let runtime = Runtime::unattached();
        let events = runtime.subscribe();

        let dir = std::env::temp_dir().join(format!("prism-m20-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        let note_path = dir.join("note.txt");
        std::fs::write(&note_path, "MAGIC-WORD-2026\n").expect("写附件");
        let created = runtime
            .create_session(dir.to_str().expect("临时目录路径"))
            .expect("create_session");
        let sid = created.session_id.clone();

        let files = vec![(
            note_path.to_string_lossy().to_string(),
            "note.txt".to_string(),
        )];
        runtime
            .send_prompt_rich(
                &sid,
                "请读取附件 note.txt，告诉我里面的 MAGIC-WORD 是什么；只回复这一个词。",
                &files,
            )
            .expect("send_prompt_rich");

        let deadline = Instant::now() + Duration::from_secs(120);
        let mut terminal = None;
        while Instant::now() < deadline {
            let Ok(event) = events.recv_timeout(Duration::from_millis(500)) else {
                continue;
            };
            if event.pointer("/data/sessionID").and_then(Value::as_str) != Some(sid.as_str()) {
                continue;
            }
            match event.get("type").and_then(Value::as_str) {
                Some(
                    kind @ ("session.execution.succeeded"
                    | "session.execution.failed"
                    | "session.execution.interrupted"),
                ) => {
                    terminal = Some(kind.to_string());
                    break;
                }
                _ => {}
            }
        }
        assert_eq!(
            terminal.as_deref(),
            Some("session.execution.succeeded"),
            "带附件的 prompt 应成功收口"
        );

        // DB 侧：用户消息应保留附件形态（打印供人工核对），助手应答出内容。
        let db_path = locator::database_path().expect("opencode.db");
        let mut assistant_ok = false;
        for _ in 0..20 {
            let page = crate::engines::opencode::db::read_timeline(&db_path, &sid, None, 50)
                .expect("read_timeline");
            if let Some(user) = page.records.iter().find(|record| record.role == "user") {
                println!("用户消息 parts：{:?}", user.parts);
            }
            let text = page
                .records
                .iter()
                .rev()
                .find(|record| record.role == "assistant")
                .map(|record| {
                    record
                        .parts
                        .iter()
                        .filter_map(|part| match part {
                            crate::engines::core::TimelinePart::Text { text } => {
                                Some(text.as_str())
                            }
                            _ => None,
                        })
                        .collect::<String>()
                })
                .unwrap_or_default();
            if text.contains("MAGIC-WORD-2026") {
                assistant_ok = true;
                break;
            }
            thread::sleep(Duration::from_millis(300));
        }
        assert!(assistant_ok, "助手应能读到附件内容");

        runtime.delete_session(&sid).expect("delete_session");
        runtime.shutdown();
        let removed = cleanup_project(&dir);
        assert_eq!(removed, 1, "应恰好清理一条测试项目行");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 真机组会话 shell：接口可调用（命令落引擎侧，不发给模型）。
    /// `cargo test --lib -- --ignored --nocapture session_shell_real`
    #[test]
    #[ignore]
    fn session_shell_real() {
        let runtime = Runtime::unattached();
        let dir = std::env::temp_dir().join(format!("prism-m20-shell-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        let created = runtime
            .create_session(dir.to_str().expect("临时目录路径"))
            .expect("create_session");
        let sid = created.session_id.clone();

        runtime
            .session_shell(&sid, "echo prism-shell-ok")
            .expect("session_shell");
        // 空命令应被本地拦下
        assert!(runtime.session_shell(&sid, "   ").is_err());
        println!("shell 已执行（输出落引擎侧）");

        runtime.delete_session(&sid).expect("delete_session");
        runtime.shutdown();
        let removed = cleanup_project(&dir);
        assert_eq!(removed, 1, "应恰好清理一条测试项目行");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
