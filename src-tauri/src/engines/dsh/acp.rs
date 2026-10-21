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

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
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

struct Pending {
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
        let client = Arc::new(AcpClient {
            stdin: Mutex::new(stdin),
            child: Mutex::new(child),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicI64::new(1),
            alive: alive.clone(),
            incoming: Mutex::new(Some(incoming_rx)),
        });

        spawn_reader(stdout, client.clone(), incoming_tx, alive.clone());
        spawn_stderr_reader(stderr);

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
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(format!("{method} 连接已断开")),
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
            return Err("dsh 连接已断开".to_string());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let key = id.to_string();
        let (sender, receiver) = mpsc::channel();
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(key.clone(), Pending { sender });

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
        let mut line = serde_json::to_string(frame)
            .map_err(|cause| format!("序列化 ACP 帧失败：{cause}"))?;
        line.push('\n');
        let mut stdin = self
            .stdin
            .lock()
            .unwrap_or_else(|error| error.into_inner());
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
        let _ = self.child.lock().map(|mut child| child.kill());
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
            // 进程没了，等待中的请求不能靠超时熬过去。
            let pending = std::mem::take(
                &mut *client
                    .pending
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()),
            );
            for (_, entry) in pending {
                let _ = entry.sender.send(Err("dsh 连接已断开".to_string()));
            }
            let _ = incoming.send(Incoming::Closed {
                reason: "dsh 进程已退出".to_string(),
            });
        });
}

/// stderr 必须有人读：管道写满会让 dsh 卡住，表现为"莫名其妙超时"。
fn spawn_stderr_reader(stderr: std::process::ChildStderr) {
    let _ = thread::Builder::new()
        .name("prism-dsh-err".into())
        .spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                #[cfg(debug_assertions)]
                {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        println!("[prism] dsh: {trimmed}");
                    }
                }
                #[cfg(not(debug_assertions))]
                let _ = line;
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
