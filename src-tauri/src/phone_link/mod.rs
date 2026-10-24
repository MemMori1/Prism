//! 手机链接（phone.link 插件）：让手机通过局域网页面对话桌面 Agent，
//! 并把关键事件推送到企业微信群。
//!
//! 结构与 routines 同构：`store` 管配置落盘，`turn` 管引擎回合与事件流，
//! `server` 管 LAN HTTP，`push` 管微信 webhook，`qr` 管配对二维码。
//! 本文件是状态中枢：配置缓存、服务句柄、配对握手与对外状态视图。

pub mod push;
pub mod qr;
pub mod server;
pub mod store;
pub mod turn;

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use store::{PairedDevice, PhoneLinkConfig, PushEvents};

/// 配对码有效期：过了就得在桌面端刷新（二维码跟着换）。
const PAIRING_TTL: Duration = Duration::from_secs(120);
/// 配对尝试限速：每分钟最多 10 次（暴力猜码没有意义）。
const PAIR_ATTEMPTS: usize = 10;

pub const EVENT_NAME: &str = "phone-link-event";

/// 内嵌的配对 / 对话页（单文件，无构建步骤）。
pub static PAGE_HTML: &str = include_str!("page.html");

// ---------------------------------------------------------------- 状态

pub struct PhoneLinkState {
    inner: Mutex<Inner>,
}

struct Inner {
    config: PhoneLinkConfig,
    server: Option<server::ServerHandle>,
    /// 进行中的配对握手（一次性令牌，runtime-only）。
    pairing: Option<PairedHandshake>,
    chat: ChatState,
    /// 配对尝试时间戳（限速用）。
    pair_attempts: VecDeque<Instant>,
}

pub struct PairedHandshake {
    pub token: String,
    /// 过期时刻（毫秒时间戳，与 store 的时间口径一致）。
    pub expires_at_ms: i64,
}

#[derive(Default)]
pub struct ChatState {
    /// 手机会话（进程内有效；重启后首轮会新开会话）。
    pub session_id: Option<String>,
    pub buffer: turn::EventBuffer,
    pub running: bool,
}

/// 长轮询用的聊天快照（取数发生在锁内，拿出去的都是 owned）。
pub struct ChatSnapshot {
    pub session_id: Option<String>,
    pub running: bool,
    pub latest: u64,
    pub events: Vec<Value>,
}

impl PhoneLinkState {
    pub fn new() -> Self {
        let config = match store::store_path() {
            Some(path) => match store::load(&path) {
                Ok(config) => config,
                Err(error) => {
                    eprintln!("[prism] 手机链接配置读取失败，使用默认配置：{error}");
                    PhoneLinkConfig::default()
                }
            },
            None => PhoneLinkConfig::default(),
        };
        Self {
            inner: Mutex::new(Inner {
                config,
                server: None,
                pairing: None,
                chat: ChatState::default(),
                pair_attempts: VecDeque::new(),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 配置快照（回合线程拿引擎名 / webhook 用）。
    pub fn config(&self) -> PhoneLinkConfig {
        self.lock().config.clone()
    }

    pub fn current_session(&self) -> Option<String> {
        self.lock().chat.session_id.clone()
    }

    /// 回合互斥：同一时刻只跑一轮。
    pub fn try_begin_turn(&self) -> bool {
        let mut guard = self.lock();
        if guard.chat.running {
            return false;
        }
        guard.chat.running = true;
        true
    }

    pub fn end_turn(&self, session_id: Option<String>) {
        let mut guard = self.lock();
        guard.chat.running = false;
        if let Some(session_id) = session_id {
            guard.chat.session_id = Some(session_id);
        }
    }

    pub fn push_chat_event(&self, kind: &str, data: Value) {
        self.lock().chat.buffer.push(kind, data);
    }

    /// seq > since 的事件快照（长轮询入口）。
    pub fn chat_snapshot(&self, since: u64) -> ChatSnapshot {
        let guard = self.lock();
        ChatSnapshot {
            session_id: guard.chat.session_id.clone(),
            running: guard.chat.running,
            latest: guard.chat.buffer.latest(),
            events: guard.chat.buffer.since(since),
        }
    }

    pub fn device_token_matches(&self, token: &str) -> bool {
        let guard = self.lock();
        match &guard.config.device {
            Some(device) => !token.is_empty() && device.token == token,
            None => false,
        }
    }

    /// 设备活跃：刷新 last_seen（只动内存，落盘等下次配置保存顺带带走）。
    pub fn touch_device(&self) {
        let mut guard = self.lock();
        if let Some(device) = &mut guard.config.device {
            device.last_seen_at = turn::now_ms();
        }
    }

    /// 配对限速：超过每分钟 PAIR_ATTEMPTS 次就拒绝。
    fn allow_pair_attempt(&self) -> bool {
        let mut guard = self.lock();
        let now = Instant::now();
        while guard
            .pair_attempts
            .front()
            .map(|at| now.duration_since(*at) > Duration::from_secs(60))
            .unwrap_or(false)
        {
            guard.pair_attempts.pop_front();
        }
        if guard.pair_attempts.len() >= PAIR_ATTEMPTS {
            return false;
        }
        guard.pair_attempts.push_back(now);
        true
    }
}

// ---------------------------------------------------------------- 生命周期

/// 插件装配时的入口：按配置决定是否直接起服务。
pub fn boot(app: &AppHandle) -> Result<(), String> {
    let Some(state) = app.try_state::<PhoneLinkState>() else {
        return Ok(());
    };
    if !state.lock().config.enabled {
        return Ok(());
    }
    start_server(app)
}

fn start_server(app: &AppHandle) -> Result<(), String> {
    let (port, already) = {
        let state = app
            .try_state::<PhoneLinkState>()
            .ok_or_else(|| "手机链接插件未启用".to_string())?;
        let guard = state.lock();
        (guard.config.port, guard.server.is_some())
    };
    if already {
        return Ok(());
    }
    let handle = server::start(app.clone(), port)?;
    if let Some(state) = app.try_state::<PhoneLinkState>() {
        state.lock().server = Some(handle);
    }
    Ok(())
}

fn stop_server(app: &AppHandle) {
    if let Some(state) = app.try_state::<PhoneLinkState>() {
        let mut guard = state.lock();
        if let Some(handle) = guard.server.take() {
            handle.stop();
        }
        guard.pairing = None;
    }
}

// ---------------------------------------------------------------- 配对握手

/// 手机端用配对码换设备令牌（server.rs 的 `/api/pair` 调这里）。
pub fn pair_device(app: &AppHandle, token: &str) -> Result<Value, String> {
    let Some(state) = app.try_state::<PhoneLinkState>() else {
        return Err("手机链接插件未启用".to_string());
    };
    if !state.allow_pair_attempt() {
        return Err("配对尝试太频繁，稍后再试".to_string());
    }
    {
        let mut guard = state.lock();
        let valid = guard
            .pairing
            .take()
            .filter(|handshake| {
                turn::now_ms() < handshake.expires_at_ms && handshake.token == token
            });
        match valid {
            Some(_) => {} // 一次性：拿到即作废（take 已移除）。
            // 无效：配对会话已被 take 清掉，桌面端需要重新生成。
            None => return Err("配对码无效或已过期，请在桌面端刷新二维码".to_string()),
        }
    }
    let device = PairedDevice {
        name: "手机".to_string(),
        token: store::new_token(),
        paired_at: turn::now_ms(),
        last_seen_at: turn::now_ms(),
    };
    let device_token = device.token.clone();
    persist(app, |config| config.device = Some(device))?;
    if let Some(state) = app.try_state::<PhoneLinkState>() {
        state.push_chat_event("notice", json!({ "text": "新设备已配对" }));
    }
    emit(app, "paired", None);
    println!("[prism] 手机链接：新设备已配对");
    Ok(json!({ "deviceToken": device_token }))
}

/// 统一的「改配置并落盘」入口：拿锁改，再写盘（写盘不持锁，慢 IO 别堵住 API）。
fn persist(app: &AppHandle, mutate: impl FnOnce(&mut PhoneLinkConfig)) -> Result<(), String> {
    let (path, snapshot) = {
        let state = app
            .try_state::<PhoneLinkState>()
            .ok_or_else(|| "手机链接插件未启用".to_string())?;
        let mut guard = state.lock();
        mutate(&mut guard.config);
        (
            store::store_path().ok_or_else(|| "无法定位 ~/.prism 数据目录".to_string())?,
            guard.config.clone(),
        )
    };
    store::save(&path, &snapshot)
}

fn emit(app: &AppHandle, kind: &str, detail: Option<Value>) {
    let _ = app.emit(EVENT_NAME, json!({ "kind": kind, "detail": detail }));
}

// ---------------------------------------------------------------- 状态视图

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusView {
    pub enabled: bool,
    pub engine: String,
    pub server_running: bool,
    pub port: u16,
    pub lan_ip: Option<String>,
    /// 进行中的配对（未过期才有）：前端据此渲染二维码。
    pub pairing: Option<PairingView>,
    pub qr_svg: Option<String>,
    pub pair_url: Option<String>,
    pub device: Option<DeviceView>,
    pub webhook_configured: bool,
    pub webhook_hint: Option<String>,
    pub events: PushEvents,
    pub chat_running: bool,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingView {
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceView {
    pub name: String,
    pub paired_at: i64,
    pub last_seen_at: i64,
}

/// 探测局域网地址：UDP connect 不发包，只查默认路由的出口网卡。
fn lan_ip() -> Option<String> {
    for probe in ["8.8.8.8:80", "1.1.1.1:80", "223.5.5.5:80"] {
        if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
            if socket.connect(probe).is_ok() {
                if let Ok(addr) = socket.local_addr() {
                    if !addr.ip().is_loopback() {
                        return Some(addr.ip().to_string());
                    }
                }
            }
        }
    }
    None
}

pub fn status(app: &AppHandle) -> Result<StatusView, String> {
    let state = app
        .try_state::<PhoneLinkState>()
        .ok_or_else(|| "手机链接插件未启用".to_string())?;
    let guard = state.lock();
    Ok(build_status(&guard))
}

fn build_status(guard: &MutexGuard<'_, Inner>) -> StatusView {
    let config = &guard.config;
    let ip = lan_ip();
    // 配对只在服务运行且未过期时有效。
    let active_pairing = guard
        .pairing
        .as_ref()
        .filter(|handshake| turn::now_ms() < handshake.expires_at_ms)
        .filter(|_| guard.server.is_some());
    let pairing = active_pairing.map(|handshake| PairingView {
        expires_at: handshake.expires_at_ms,
    });
    let pair_url = pairing
        .as_ref()
        .zip(ip.as_ref())
        .map(|(_, ip)| format!("http://{ip}:{}/p/{}", config.port, active_pairing.expect("刚校验过").token));
    let qr_svg = pair_url.as_deref().and_then(|url| qr::pairing_svg(url).ok());
    StatusView {
        enabled: config.enabled,
        engine: config.engine.clone(),
        server_running: guard.server.is_some(),
        port: config.port,
        lan_ip: ip,
        pairing,
        qr_svg,
        pair_url,
        device: config.device.as_ref().map(|device| DeviceView {
            name: device.name.clone(),
            paired_at: device.paired_at,
            last_seen_at: device.last_seen_at,
        }),
        webhook_configured: config.webhook_url.is_some(),
        webhook_hint: store::webhook_hint(config.webhook_url.as_deref()),
        events: config.events.clone(),
        chat_running: guard.chat.running,
        session_id: guard.chat.session_id.clone(),
    }
}

// ---------------------------------------------------------------- IPC 入口

/// 启用 / 停用：起停服务，启用时顺手生成一轮新配对码。
pub fn set_enabled(app: &AppHandle, enabled: bool) -> Result<StatusView, String> {
    if enabled {
        start_server(app)?;
        refresh_pairing_inner(app)?;
    } else {
        stop_server(app);
    }
    persist(app, |config| config.enabled = enabled)?;
    status(app)
}

/// 生成一轮新配对码（旧的作废）。
pub fn refresh_pairing(app: &AppHandle) -> Result<StatusView, String> {
    refresh_pairing_inner(app)?;
    status(app)
}

fn refresh_pairing_inner(app: &AppHandle) -> Result<(), String> {
    let state = app
        .try_state::<PhoneLinkState>()
        .ok_or_else(|| "手机链接插件未启用".to_string())?;
    let mut guard = state.lock();
    guard.pairing = Some(PairedHandshake {
        token: store::new_token(),
        expires_at_ms: turn::now_ms() + PAIRING_TTL.as_millis() as i64,
    });
    Ok(())
}

/// 解除配对：设备令牌作废，手机端下次请求就会收到 401。
pub fn unpair(app: &AppHandle) -> Result<StatusView, String> {
    persist(app, |config| config.device = None)?;
    if let Some(state) = app.try_state::<PhoneLinkState>() {
        state.push_chat_event("notice", json!({ "text": "设备已解除配对" }));
    }
    emit(app, "unpaired", None);
    status(app)
}

/// 保存增量配置；webhook 传空串 = 保持原值（与 native 密钥同口径）。
pub fn save_config(
    app: &AppHandle,
    engine: Option<String>,
    webhook_url: Option<String>,
    events: Option<PushEvents>,
) -> Result<StatusView, String> {
    {
        let state = app
            .try_state::<PhoneLinkState>()
            .ok_or_else(|| "手机链接插件未启用".to_string())?;
        let guard = state.lock();
        let next_engine = engine.clone().unwrap_or_else(|| guard.config.engine.clone());
        let next_webhook = webhook_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| guard.config.webhook_url.clone());
        store::validate(&next_engine, next_webhook.as_deref())?;
    }
    persist(app, |config| {
        if let Some(engine) = engine {
            config.engine = engine.trim().to_string();
        }
        if let Some(webhook_url) = webhook_url {
            let trimmed = webhook_url.trim();
            if !trimmed.is_empty() {
                config.webhook_url = Some(trimmed.to_string());
            }
        }
        if let Some(events) = events {
            config.events = events;
        }
    })?;
    status(app)
}

/// 给配置的微信群发一条测试消息。
pub fn webhook_test(app: &AppHandle) -> Result<(), String> {
    let state = app
        .try_state::<PhoneLinkState>()
        .ok_or_else(|| "手机链接插件未启用".to_string())?;
    let url = state
        .config()
        .webhook_url
        .ok_or_else(|| "还没有配置 webhook 地址".to_string())?;
    push::send(&url, &push::test_message())
}

/// 退出时停服（插件 disposer 调用）。
pub fn shutdown(app: &AppHandle) {
    stop_server(app);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lan_ip_probe_does_not_panic() {
        // 不 assert 结果（CI 可能没有外网路由），只要求不炸。
        let _ = lan_ip();
    }

    #[test]
    fn status_view_serializes_camel_case() {
        let view = StatusView {
            enabled: true,
            engine: "opencode".into(),
            server_running: false,
            port: 15211,
            lan_ip: Some("192.168.1.5".into()),
            pairing: None,
            qr_svg: None,
            pair_url: None,
            device: None,
            webhook_configured: false,
            webhook_hint: None,
            events: PushEvents::default(),
            chat_running: false,
            session_id: None,
        };
        let text = serde_json::to_string(&view).unwrap();
        assert!(text.contains("serverRunning"), "应输出 camelCase：{text}");
        assert!(text.contains("webhookConfigured"));
    }
}
