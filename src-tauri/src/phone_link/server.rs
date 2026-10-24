//! 手机链接的 LAN HTTP 服务：配对 + 对话。
//!
//! tiny_http + 固定线程池（个人应用，一部手机，4 个 worker 足够：长轮询、
//! 发消息、权限答复可以并行）。路由很小，不值得上框架；鉴权只有一条——
//! 配对成功后发的设备令牌，请求头带上才算数。

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use tauri::{AppHandle, Manager};

use super::turn;
use super::PhoneLinkState;

/// worker 数：长轮询会占住线程，留足并发余量。
const WORKERS: usize = 4;
/// 请求体上限：对话文本用不了 64KB。
const BODY_CAP: u64 = 64 * 1024;
/// 长轮询挂起时长：超时返回空事件，客户端立刻续上。
const LONG_POLL: Duration = Duration::from_secs(20);
/// 长轮询里检查新事件的间隔。
const POLL_TICK: Duration = Duration::from_millis(300);

type Resp = tiny_http::Response<std::io::Cursor<Vec<u8>>>;

/// 运行中的服务句柄：`stop()` 置位 + 自连解阻塞，worker 退出后 join。
pub struct ServerHandle {
    stop: Arc<AtomicBool>,
    port: u16,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl ServerHandle {
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // 每个 worker 都喂一条可解析的空请求，把阻塞在 recv() 的线程顶出来。
        for _ in 0..self.workers.len() {
            if let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", self.port)) {
                let _ = std::io::Write::write_all(&mut stream, b"GET /__shutdown HTTP/1.0\r\n\r\n");
            }
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// 起服务（绑定 0.0.0.0，局域网内可访问；仅在本插件启用时运行）。
pub fn start(app: AppHandle, port: u16) -> Result<ServerHandle, String> {
    let server = tiny_http::Server::http(("0.0.0.0", port))
        .map_err(|error| format!("监听 {port} 端口失败：{error}（可能被占用）"))?;
    let server = Arc::new(server);
    let stop = Arc::new(AtomicBool::new(false));
    let mut workers = Vec::new();
    for index in 0..WORKERS {
        let server = server.clone();
        let stop = stop.clone();
        let app = app.clone();
        workers.push(
            std::thread::Builder::new()
                .name(format!("phone-link-{index}"))
                .spawn(move || loop {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    match server.recv() {
                        Ok(request) => {
                            if stop.load(Ordering::SeqCst) {
                                let _ = request.respond(plain(410, "bye"));
                                break;
                            }
                            handle(&app, request);
                        }
                        // 连接被对端掐断之类：没停就继续收下一个。
                        Err(_) if stop.load(Ordering::SeqCst) => break,
                        Err(_) => continue,
                    }
                })
                .map_err(|error| format!("启动手机链接线程失败：{error}"))?,
        );
    }
    println!("[prism] 手机链接服务已监听 0.0.0.0:{port}");
    Ok(ServerHandle {
        stop,
        port,
        workers,
    })
}

fn handle(app: &AppHandle, mut request: tiny_http::Request) {
    let method = request.method().as_str().to_ascii_uppercase();
    let url = request.url().to_string();
    let path = url.split('?').next().unwrap_or("/").to_string();
    // POST 的请求体先读出来（配对与各 API 都是小 JSON），避免闭包里再借 request。
    let body = if method == "POST" {
        match read_json(&mut request) {
            Ok(body) => Some(body),
            Err(error) => {
                let _ = request.respond(json_err(&error, 400));
                return;
            }
        }
    } else {
        None
    };

    let response = match (method.as_str(), path.as_str()) {
        ("GET", "/") | ("GET", "/index.html") => page(),
        ("GET", path) if path.starts_with("/p/") => page(),
        ("POST", "/api/pair") => {
            let token = body
                .as_ref()
                .and_then(|body| body.get("token"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            match super::pair_device(app, &token) {
                Ok(value) => json_ok(&value),
                Err(error) => json_err(&error, 403),
            }
        }
        ("POST", "/api/message") => require_device(app, &request, || {
            let text = body
                .as_ref()
                .and_then(|body| body.get("text"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if text.is_empty() {
                return Err("消息不能为空".to_string());
            }
            turn::run_turn(app, text)?;
            Ok(json!({ "ok": true }))
        }),
        ("GET", "/api/events") => {
            let since = query_value(&url, "since")
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            // 长轮询的响应不是 JSON 快答，鉴权单独做（状态检查在轮询里兜底）。
            if let Some(error) = device_error(app, &request) {
                error
            } else {
                long_poll_events(app, since)
            }
        }
        ("POST", "/api/permission") => require_device(app, &request, || {
            let id = body
                .as_ref()
                .and_then(|body| body.get("id"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let decision = body
                .as_ref()
                .and_then(|body| body.get("decision"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("once")
                .to_string();
            if id.is_empty() {
                return Err("缺少权限请求 id".to_string());
            }
            turn::reply_permission(app, &id, &decision)?;
            Ok(json!({ "ok": true }))
        }),
        ("POST", "/api/interrupt") => require_device(app, &request, || {
            turn::interrupt(app)?;
            Ok(json!({ "ok": true }))
        }),
        // 停服用的自连探针：响应 410，worker 看到停止位自行退出。
        (_, "/__shutdown") => plain(410, "bye"),
        _ => plain(404, "not found"),
    };
    let _ = request.respond(response);
}

fn query_value<'a>(url: &'a str, key: &str) -> Option<&'a str> {
    let (_, query) = url.split_once('?')?;
    query
        .split('&')
        .find_map(|item| item.split_once('=').filter(|(name, _)| *name == key))
        .map(|(_, value)| value)
}

/// 有新事件立刻返回；没有就挂到 LONG_POLL，客户端拿到 latest 续传。
fn long_poll_events(app: &AppHandle, since: u64) -> Resp {
    let deadline = Instant::now() + LONG_POLL;
    loop {
        let Some(state) = app.try_state::<PhoneLinkState>() else {
            return json_err("手机链接插件未启用", 503);
        };
        let snapshot = state.chat_snapshot(since);
        let events = snapshot.events;
        if !events.is_empty() || Instant::now() >= deadline {
            let body = json!({
                "events": events,
                "latest": snapshot.latest,
                "session": {
                    "sessionId": snapshot.session_id,
                    "running": snapshot.running,
                },
            });
            state.touch_device();
            return json_ok(&body);
        }
        std::thread::sleep(POLL_TICK);
    }
}

/// 校验设备令牌；通过后顺带刷新活跃时间。
fn require_device(
    app: &AppHandle,
    request: &tiny_http::Request,
    handle: impl FnOnce() -> Result<serde_json::Value, String>,
) -> Resp {
    if let Some(error) = device_error(app, request) {
        return error;
    }
    match handle() {
        Ok(value) => json_ok(&value),
        Err(error) => json_err(&error, 400),
    }
}

/// 鉴权检查：通过返回 None，不通过返回现成的错误响应。
fn device_error(app: &AppHandle, request: &tiny_http::Request) -> Option<Resp> {
    let token = header_value(request, "x-device-token")
        .or_else(|| header_value(request, "authorization"))
        .map(|value| value.strip_prefix("Bearer ").unwrap_or(&value).trim().to_string())
        .unwrap_or_default();
    let Some(state) = app.try_state::<PhoneLinkState>() else {
        return Some(json_err("手机链接插件未启用", 503));
    };
    if state.device_token_matches(&token) {
        None
    } else {
        Some(json_err("设备未配对或令牌已失效，请重新扫码", 401))
    }
}

fn header_value(request: &tiny_http::Request, name: &str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|header| header.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|header| header.value.as_str().to_string())
}

fn read_json(request: &mut tiny_http::Request) -> Result<serde_json::Value, String> {
    let mut buffer = Vec::new();
    request
        .as_reader()
        .take(BODY_CAP)
        .read_to_end(&mut buffer)
        .map_err(|error| format!("读取请求体失败：{error}"))?;
    serde_json::from_slice(&buffer).map_err(|error| format!("请求体不是合法 JSON：{error}"))
}

/// 配对 / 对话共用一张页面：没有设备令牌时先走配对，有就直接进对话。
fn page() -> Resp {
    tiny_http::Response::from_string(super::PAGE_HTML)
        .with_header(header("Content-Type", "text/html; charset=utf-8"))
        .with_status_code(200)
}

fn plain(status: u16, text: &str) -> Resp {
    tiny_http::Response::from_string(text)
        .with_header(header("Content-Type", "text/plain; charset=utf-8"))
        .with_status_code(status)
}

fn json_ok(value: &serde_json::Value) -> Resp {
    tiny_http::Response::from_string(value.to_string())
        .with_header(header("Content-Type", "application/json; charset=utf-8"))
        .with_status_code(200)
}

fn json_err(message: &str, status: u16) -> Resp {
    let body = json!({ "error": message });
    tiny_http::Response::from_string(body.to_string())
        .with_header(header("Content-Type", "application/json; charset=utf-8"))
        .with_status_code(status)
}

fn header(name: &'static str, value: &'static str) -> tiny_http::Header {
    tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("静态响应头必然合法")
}
