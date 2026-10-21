//! ACP v1（Agent Client Protocol）stdio 客户端。
//!
//! 为什么走 ACP 而不是 headless：headless 是"一个任务一次进程"，续聊要
//! `--session-id` 重新起进程；ACP 是官方给"外部控制器"的常驻入口，一个连接
//! 能同时跑多个会话，还能 `session/list` / `session/resume` / `session/cancel`。
//! 对 Prism 来说，它就是 opencode 的 `serve` 那个位置。
//!
//! 协议形态是标准 JSON-RPC 2.0，按行分帧（stdout 只跑协议，日志走 stderr）：
//! - 客户端发请求（`initialize` / `session/new` / `session/prompt` …），按 id 配对响应；
//! - 服务端发通知（`session/update`），没有 id，不需要应答；
//! - 服务端也会**反向发请求**（`session/request_permission`），必须由客户端应答，
//!   否则那一轮会一直挂着。
//!
//! 设计上的两个取舍：
//! - 分帧按行而不是按字节流长度，因为 ACP 的 stdio 传输明确是 NDJSON；
//! - 反向请求不在读线程里处理（读线程一阻塞，所有响应都堵住），而是推给
//!   `Incoming` 通道，由调用方决定何时应答。

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use super::locator;

/// ACP 协议版本：v1 是目前唯一的稳定版。
pub const PROTOCOL_VERSION: i64 = 1;

/// 服务端发来的非响应帧。
#[derive(Debug, Clone)]
pub enum Incoming {
    /// 通知：不需要应答（`session/update`、`session/cancel` 之类的回执）。
    Notification { method: String, params: Value },
    /// 反向请求：必须回 `respond()`，否则发起的那个操作会等下去。
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    /// 连接断了。等待中的请求会各自超时/失败，这里只是让上层知道原因。
    Closed { reason: String },
}

/// stderr 最后保留多少行「上下文」。
///
/// 只留尾部是不够的：dsh 崩溃时会先打一行真正的原因，再跟十几层栈帧。
/// 一个纯尾部的环形缓冲恰好会把原因挤掉，只留下一堆 `at ...index.js:1070`。
const STDERR_TAIL_LINES: usize = 8;
/// 「像原因」的行最多留几条（`Error:` / `EACCES` / `failed to …`）。
const STDERR_CAUSE_LINES: usize = 3;
/// 单行截断长度。栈帧路径动辄两百字符，整段拼进错误提示会撑爆界面。
const STDERR_LINE_LIMIT: usize = 180;

/// stderr 的两段式缓冲：原因 + 尾巴。
#[derive(Default)]
struct StderrLog {
    causes: VecDeque<String>,
    tail: VecDeque<String>,
}

impl StderrLog {
    fn push(&mut self, line: &str) {
        let line: String = line.chars().take(STDERR_LINE_LIMIT).collect();
        push_bounded(&mut self.tail, line.clone(), STDERR_TAIL_LINES);
        if looks_like_cause(&line) {
            push_bounded(&mut self.causes, line, STDERR_CAUSE_LINES);
        }
    }

    /// 原因在前、尾巴在后，去重。原因没了就只剩尾巴，不会报空。
    fn render(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        for line in self.causes.iter().chain(self.tail.iter()) {
            if !parts.contains(&line.as_str()) {
                parts.push(line);
            }
        }
        parts.join(" · ")
    }
}

fn push_bounded(queue: &mut VecDeque<String>, line: String, limit: usize) {
    if queue.len() == limit {
        queue.pop_front();
    }
    queue.push_back(line);
}

/// 这行是不是「真正的原因」。
///
/// 刻意宽松：多留一行噪音的代价，远小于把唯一有用的那行挤掉。
fn looks_like_cause(line: &str) -> bool {
    let lower = line.to_lowercase();
    [
        "error:",
        "failed to",
        "permission denied",
        "eacces",
        "enotdir",
        "enoent",
        "cannot find",
        "not found",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

struct Pending {
    /// 请求方法名。断开时要靠它说清"是哪一步"失败的——一律报
    /// "连接已断开"的话，用户看不出是握手就没起来还是发消息时掉的。
    method: String,
    sender: Sender<Result<Value, String>>,
}

pub struct AcpClient {
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
    pending: Mutex<HashMap<String, Pending>>,
    next_id: AtomicI64,
    alive: Arc<AtomicBool>,
    /// 读线程产出的非响应帧。只能被取走一次（运行轴独占消费）。
    incoming: Mutex<Option<Receiver<Incoming>>>,
    /// stderr 的尾部若干行，断线时拼进错误信息。
    stderr_tail: Arc<Mutex<StderrLog>>,
}

impl AcpClient {
    /// 起一个 `dsh --profile acp` 进程并完成握手前的准备。
    ///
    /// 不在这里发 `initialize`：调用方可能需要带自己的能力声明，而且失败信息
    /// 由运行轴统一翻译成用户能读的句子。
    pub fn spawn(home: Option<&Path>) -> Result<Arc<AcpClient>, String> {
        let cli = locator::cli_path().ok_or_else(|| {
            "未找到 dsh 命令。装好 DeepSeek Harness 后重启 Prism（`npm i -g @deepseek-ai/dsh`）"
                .to_string()
        })?;

        let mut command = Command::new(&cli);
        command
            .arg("--profile")
            .arg(locator::PROFILE)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // DSH_HOME 显式传下去：Prism 读哪份日志，ACP 就写哪份，避免两边
        // 因为环境变量差异看不同的 home。
        if let Some(home) = home {
            command.env("DSH_HOME", home);
        }

        let mut child = command
            .spawn()
            .map_err(|cause| format!("启动 dsh --profile acp 失败：{cause}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "dsh stdin 不可用".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "dsh stdout 不可用".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "dsh stderr 不可用".to_string())?;

        let (incoming_tx, incoming_rx) = mpsc::channel::<Incoming>();
        let alive = Arc::new(AtomicBool::new(true));
        let stderr_tail = Arc::new(Mutex::new(StderrLog::default()));
        let client = Arc::new(AcpClient {
            stdin: Mutex::new(stdin),
            child: Mutex::new(child),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicI64::new(1),
            alive: alive.clone(),
            incoming: Mutex::new(Some(incoming_rx)),
            stderr_tail: stderr_tail.clone(),
        });

        spawn_reader(stdout, client.clone(), incoming_tx, alive.clone());
        spawn_stderr_reader(stderr, stderr_tail);

        Ok(client)
    }

    /// 取走非响应帧通道（只能取一次；运行轴在启动时调用）。
    pub fn take_incoming(&self) -> Option<Receiver<Incoming>> {
        self.incoming
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    /// stderr 的最后若干行（拼成一段，供错误信息用）。没有输出时返回空串。
    pub fn last_stderr(&self) -> String {
        self.stderr_tail
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .render()
    }

    /// 连接为什么断了——一句能直接给用户看的话。
    ///
    /// 顺序是"退出码 + stderr 尾部"。只报 `code=1` 用户没法处置；只报 stderr
    /// 又可能看不出是哪一步。两条都给，才够定位。
    fn close_detail(&self, status: Option<String>) -> String {
        let stderr = self.last_stderr();
        match (status, stderr.is_empty()) {
            (Some(status), false) => format!("{status}：{stderr}"),
            (Some(status), true) => status,
            (None, false) => stderr,
            (None, true) => "dsh 进程已退出（没有留下错误输出）".to_string(),
        }
    }

    /// 发一个请求，等着响应（带超时）。
    pub fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let (id, receiver) = self.send_request(method, params)?;
        match receiver.recv_timeout(timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // 超时要把挂起项清掉，否则后续同名响应对不上号。
                self.pending
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .remove(&id);
                Err(format!("{method} 超时（{}s）", timeout.as_secs()))
            }
            // 通道被丢掉说明读线程收摊时没来得及配对：把原因补上。
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(format!("{method} 失败：{}", self.close_detail(None)))
            }
        }
    }

    /// 发请求但不等待——`session/prompt` 可能跑几分钟，调用方要自己决定
    /// 是阻塞等还是放到工作线程里等。
    pub fn send_request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<(String, Receiver<Result<Value, String>>), String> {
        if !self.is_alive() {
            return Err(self.close_detail(None));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let key = id.to_string();
        let (sender, receiver) = mpsc::channel();
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(
                key.clone(),
                Pending {
                    method: method.to_string(),
                    sender,
                },
            );

        let frame = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if let Err(error) = self.write_frame(&frame) {
            self.pending
                .lock()
                .unwrap_or_else(|inner| inner.into_inner())
                .remove(&id.to_string());
            return Err(error);
        }
        Ok((key, receiver))
    }

    /// 应答服务端的反向请求（权限询问）。
    pub fn respond(&self, id: &Value, result: Result<Value, Value>) -> Result<(), String> {
        let frame = match result {
            Ok(value) => json!({ "jsonrpc": "2.0", "id": id, "result": value }),
            Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
        };
        self.write_frame(&frame)
    }

    /// 发通知（`session/cancel` 就是通知，不期待响应）。
    pub fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        self.write_frame(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    fn write_frame(&self, frame: &Value) -> Result<(), String> {
        let mut line =
            serde_json::to_string(frame).map_err(|cause| format!("序列化 ACP 帧失败：{cause}"))?;
        line.push('\n');
        let mut stdin = self.stdin.lock().unwrap_or_else(|error| error.into_inner());
        stdin
            .write_all(line.as_bytes())
            .and_then(|_| stdin.flush())
            .map_err(|cause| format!("写入 dsh 失败：{cause}"))
    }

    /// 结束进程，并让所有等待中的请求立刻失败。
    pub fn shutdown(&self) {
        if !self.alive.swap(false, Ordering::SeqCst) {
            return;
        }
        let pending = std::mem::take(
            &mut *self
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        );
        for (_, entry) in pending {
            let _ = entry.sender.send(Err("dsh 已停止".to_string()));
        }
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for AcpClient {
    fn drop(&mut self) {
        // 直接 kill：会话持久化由 dsh 自己负责，Prism 不替它 flush。
        // 顺手 wait 一下，否则子进程会以僵尸态挂着（`initialize` 失败时
        // 每次重试都留一个）。
        let _ = self.child.lock().map(|mut child| {
            let _ = child.kill();
            let _ = child.wait();
        });
    }
}

fn spawn_reader(
    stdout: std::process::ChildStdout,
    client: Arc<AcpClient>,
    incoming: Sender<Incoming>,
    alive: Arc<AtomicBool>,
) {
    let _ = thread::Builder::new()
        .name("prism-dsh-acp".into())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Some(frame) = decode_frame(trimmed) {
                    route_frame(frame, &client, &incoming);
                }
            }
            alive.store(false, Ordering::SeqCst);
            // 进程没了，等待中的请求不能靠超时熬过去。退出码与 stderr 一起带上：
            // dsh 崩在 stdout 上是不留话的，真正的原因只在 stderr 里。
            let detail = client.close_detail(exit_status(&client));
            let pending = std::mem::take(
                &mut *client
                    .pending
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()),
            );
            for (_, entry) in pending {
                let _ = entry
                    .sender
                    .send(Err(format!("{} 失败：{detail}", entry.method)));
            }
            let _ = incoming.send(Incoming::Closed { reason: detail });
        });
}

/// 取子进程的退出状态。
///
/// stdout 读到 EOF 之后进程通常已经（或正在）退出，但可能会晚几十毫秒。
/// 用 `try_wait` 短轮询而不是 `wait`：`wait` 要独占 `child` 锁，而
/// `shutdown()` 也在抢这把锁，两边撞上会互相等到对方放锁。
fn exit_status(client: &AcpClient) -> Option<String> {
    for _ in 0..40 {
        if let Ok(mut child) = client.child.try_lock() {
            match child.try_wait() {
                Ok(Some(status)) => return Some(describe_status(status)),
                // 还没退出：stdout 先关了而已，再等一轮。
                Ok(None) => {}
                Err(_) => return None,
            }
        }
        thread::sleep(Duration::from_millis(25));
    }
    None
}

fn describe_status(status: ExitStatus) -> String {
    match (status.code(), status_signal(&status)) {
        (Some(code), _) => format!("dsh 退出（code {code}）"),
        (None, Some(signal)) => format!("dsh 被信号 {signal} 终止"),
        (None, None) => "dsh 异常退出".to_string(),
    }
}

#[cfg(unix)]
fn status_signal(status: &ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn status_signal(_status: &ExitStatus) -> Option<i32> {
    None
}

/// stderr 必须有人读：管道写满会让 dsh 卡住，表现为"莫名其妙超时"。
///
/// 同时留一份尾部在内存里——dsh 起不来时，这几十个字是用户唯一的线索。
fn spawn_stderr_reader(stderr: std::process::ChildStderr, tail: Arc<Mutex<StderrLog>>) {
    let _ = thread::Builder::new()
        .name("prism-dsh-err".into())
        .spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                #[cfg(debug_assertions)]
                println!("[prism] dsh: {trimmed}");

                tail.lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(trimmed);
            }
        });
}

/// 一行文本 → 一帧 JSON。非协议输出（比如 npm 的提示）一律忽略。
fn decode_frame(line: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(line).ok()?;
    value.is_object().then_some(value)
}

fn route_frame(frame: Value, client: &AcpClient, incoming: &Sender<Incoming>) {
    let id = frame.get("id").cloned();
    let method = frame
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_string);

    match (id, method) {
        // 响应：有 id、没有 method。
        (Some(id), None) => {
            let key = id_key(&id);
            let entry = client
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&key);
            if let Some(entry) = entry {
                let outcome = match frame.get("error") {
                    Some(error) => Err(describe_error(error)),
                    None => Ok(frame.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = entry.sender.send(outcome);
            }
        }
        // 反向请求：有 id 也有 method。
        (Some(id), Some(method)) => {
            let _ = incoming.send(Incoming::Request {
                id,
                method,
                params: frame.get("params").cloned().unwrap_or(Value::Null),
            });
        }
        // 通知：只有 method。
        (None, Some(method)) => {
            let _ = incoming.send(Incoming::Notification {
                method,
                params: frame.get("params").cloned().unwrap_or(Value::Null),
            });
        }
        // 既没 id 也没 method：不是协议帧。
        (None, None) => {}
    }
}

/// id 可能是数字也可能是字符串，统一成字符串键。
fn id_key(id: &Value) -> String {
    match id {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn describe_error(error: &Value) -> String {
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("未知错误");
    match error.get("code").and_then(Value::as_i64) {
        Some(code) => format!("{message}（{code}）"),
        None => message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_frames() {
        // 响应用 id 配对；通知只有 method；反向请求两者都有。
        let response = decode_frame(r#"{"jsonrpc":"2.0","id":3,"result":{"ok":true}}"#).unwrap();
        assert_eq!(route_kind(&response), RouteKind::Response);
        let notification = decode_frame(
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-1"}}"#,
        )
        .unwrap();
        assert_eq!(route_kind(&notification), RouteKind::Notification);
        let request = decode_frame(
            r#"{"jsonrpc":"2.0","id":7,"method":"session/request_permission","params":{}}"#,
        )
        .unwrap();
        assert_eq!(route_kind(&request), RouteKind::Request);
    }

    #[test]
    fn ignores_non_protocol_lines() {
        assert!(decode_frame("npm warn deprecated foo").is_none());
        assert!(decode_frame("").is_none());
        assert!(decode_frame("[1,2,3]").is_none());
    }

    #[test]
    fn error_message_is_readable() {
        let error = json!({ "code": -32602, "message": "Invalid params" });
        assert_eq!(describe_error(&error), "Invalid params（-32602）");
    }

    #[test]
    fn id_key_normalizes_numbers_and_strings() {
        assert_eq!(id_key(&json!(5)), "5");
        assert_eq!(id_key(&json!("5")), "5");
    }

    /// 真机踩过的形状：一行原因 + 二十层栈帧 + 一行 Node 版本。
    /// 纯尾部缓冲会只留下栈帧——所以原因必须单独留。
    #[test]
    fn stderr_keeps_the_cause_when_frames_flood_the_tail() {
        let mut log = StderrLog::default();
        log.push("Error: EACCES: permission denied, mkdir '/Users/x/.dsh/profiles/acp'");
        for index in 0..20 {
            log.push(&format!("    at frame{index} (/long/path/index.js:1:1)"));
        }
        log.push("Node.js v22.22.2");

        let rendered = log.render();
        assert!(rendered.contains("EACCES"), "原因被栈帧挤掉了：{rendered}");
        assert!(rendered.contains("Node.js"), "尾巴也没了：{rendered}");
        // 中间那些栈帧不该整体搬进来。
        assert!(!rendered.contains("frame2 "), "带上了太多噪音：{rendered}");
    }

    #[test]
    fn stderr_lines_are_truncated_and_deduped() {
        let mut log = StderrLog::default();
        let long = format!("Error: {}", "x".repeat(500));
        log.push(&long);
        log.push("Error: boom");
        let rendered = log.render();
        // 单行截断，整段不该超过两条截断行 + 分隔符。
        assert!(rendered.len() < 500, "没截断：{}", rendered.len());
        assert!(rendered.contains("boom"));
    }

    #[test]
    fn cause_detection_covers_common_failure_lines() {
        assert!(looks_like_cause(
            "Error: EACCES: permission denied, mkdir '/x'"
        ));
        assert!(looks_like_cause("dsh: failed to load .env"));
        assert!(looks_like_cause("ENOTDIR: not a directory"));
        // 普通日志不该被当成原因。
        assert!(!looks_like_cause("    at frame0 (/a/b.js:1:1)"));
        assert!(!looks_like_cause("Node.js v22.22.2"));
    }

    /// 与 `route_frame` 同构的分类，只用于测试断言。
    #[derive(Debug, PartialEq)]
    enum RouteKind {
        Response,
        Request,
        Notification,
    }

    fn route_kind(frame: &Value) -> RouteKind {
        match (frame.get("id").is_some(), frame.get("method").is_some()) {
            (true, false) => RouteKind::Response,
            (true, true) => RouteKind::Request,
            (false, true) => RouteKind::Notification,
            (false, false) => panic!("不是协议帧"),
        }
    }
}
