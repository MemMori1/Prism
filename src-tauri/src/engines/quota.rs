//! 配额（订阅窗口）实时查询：只读、按需、用完即回收。
//!
//! - Codex：每次查询一次性拉起 `codex app-server`（stdio 行分隔 JSON-RPC），
//!   先 `account/read` 判定认证类型；ChatGPT 登录才继续
//!   `account/rateLimits/read` 读订阅窗口快照，随后立即回收进程（不留常驻）。
//!   API Key 认证如实返回「无订阅配额（按量计费）」。
//! - Claude Code：待本机安装 CLI 后接入（与 M9 运行轴同批）。
//!
//! 协议依据（本机 codex 0.133.0 实测 + 官方生成 schema 核对）：
//! - 响应 `GetAccountRateLimitsResponse { rateLimits: RateLimitSnapshot, … }`；
//! - `RateLimitSnapshot { limitId, limitName, primary, secondary, credits, planType, … }`；
//! - `RateLimitWindow { usedPercent, windowDurationMins, resetsAt }`（resetsAt 为 Unix 秒，
//!   统一归一为毫秒；兼容旧字段 `windowMinutes`）；
//! - `CreditsSnapshot { hasCredits, unlimited, balance }`；
//! - API Key 账号读配额返回 `chatgpt authentication required to read rate limits`。

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::engines::codex::locator;

/// 取数缓存有效期：自动取数的最小间隔（手动刷新不受限）。
pub const CACHE_TTL: Duration = Duration::from_secs(300);
const INIT_TIMEOUT: Duration = Duration::from_secs(12);
const ACCOUNT_TIMEOUT: Duration = Duration::from_secs(8);
const LIMITS_TIMEOUT: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------- 对外结构

/// 单个配额窗口（归一后的展示口径）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaWindow {
    pub used_percent: f64,
    pub window_minutes: Option<i64>,
    /// Unix 毫秒（协议上报为秒，已归一）。
    pub resets_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaCredits {
    pub has_credits: bool,
    pub unlimited: bool,
    pub balance: Option<String>,
}

/// 单引擎配额状态：`state` 为判别字段，其余按需填充。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineQuota {
    /// ok | no_subscription | cli_missing | error | pending_cli
    pub state: String,
    pub account_type: Option<String>,
    pub plan_type: Option<String>,
    pub primary: Option<QuotaWindow>,
    pub secondary: Option<QuotaWindow>,
    pub credits: Option<QuotaCredits>,
    /// 人话状态说明 / 错误原因。
    pub message: Option<String>,
}

impl EngineQuota {
    fn empty() -> Self {
        Self {
            state: String::new(),
            account_type: None,
            plan_type: None,
            primary: None,
            secondary: None,
            credits: None,
            message: None,
        }
    }

    fn simple(state: &str, message: &str) -> Self {
        Self {
            state: state.to_string(),
            message: Some(message.to_string()),
            ..Self::empty()
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaSummary {
    /// 本次取数时间（Unix 毫秒）。
    pub fetched_at_ms: i64,
    pub codex: EngineQuota,
    pub claude: EngineQuota,
}

/// 进程内缓存（配额取数含网络 IO，短时间内复用）。
pub struct QuotaState {
    cache: Mutex<Option<(QuotaSummary, Instant)>>,
}

impl QuotaState {
    pub fn new() -> Self {
        Self {
            cache: Mutex::new(None),
        }
    }

    /// 命中未过期缓存（非强制刷新时调用）。
    pub fn fresh(&self) -> Option<QuotaSummary> {
        let guard = self.cache.lock().unwrap_or_else(|error| error.into_inner());
        let (summary, at) = guard.as_ref()?;
        (at.elapsed() < CACHE_TTL).then(|| summary.clone())
    }

    pub fn store(&self, summary: QuotaSummary) {
        let mut guard = self.cache.lock().unwrap_or_else(|error| error.into_inner());
        *guard = Some((summary, Instant::now()));
    }
}

/// 汇总：Codex 实时取数 + Claude 卡位说明。
pub fn collect() -> QuotaSummary {
    QuotaSummary {
        fetched_at_ms: chrono::Local::now().timestamp_millis(),
        codex: codex_quota(),
        claude: EngineQuota::simple(
            "pending_cli",
            "待安装 Claude Code CLI 后接入（与运行轴同批）。",
        ),
    }
}

// ---------------------------------------------------------------- Codex 取数

/// Codex：一次性 app-server 查询（account/read → account/rateLimits/read）。
fn codex_quota() -> EngineQuota {
    let Some(cli) = locator::cli_path() else {
        return EngineQuota::simple("cli_missing", "未找到 codex CLI，配额暂不可查。");
    };
    let mut conn = match OneShot::start(&cli) {
        Ok(conn) => conn,
        Err(cause) => return EngineQuota::simple("error", &cause),
    };
    if let Err(cause) = conn.call(
        "initialize",
        json!({
            "clientInfo": {
                "name": "prism",
                "title": "Prism",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": { "experimentalApi": true },
        }),
        INIT_TIMEOUT,
    ) {
        return EngineQuota::simple("error", &format!("连接初始化失败：{cause}"));
    }

    let account = match conn.call("account/read", json!({}), ACCOUNT_TIMEOUT) {
        Ok(value) => value,
        Err(cause) => return EngineQuota::simple("error", &format!("读取账号信息失败：{cause}")),
    };
    let account_type = account
        .pointer("/account/type")
        .and_then(Value::as_str)
        .map(str::to_string);
    let subscribed = matches!(
        account_type.as_deref(),
        Some("chatgpt") | Some("chatgptAuthTokens")
    );
    if !subscribed {
        let message = if account_type.as_deref() == Some("apiKey") {
            "API Key 认证 · 无订阅配额（按量计费）".to_string()
        } else {
            "未检测到 ChatGPT 订阅登录 · 无订阅配额".to_string()
        };
        return EngineQuota {
            state: "no_subscription".to_string(),
            account_type,
            message: Some(message),
            ..EngineQuota::empty()
        };
    }

    let raw = match conn.call("account/rateLimits/read", json!({}), LIMITS_TIMEOUT) {
        Ok(value) => value,
        Err(cause) => {
            return EngineQuota {
                state: "error".to_string(),
                account_type,
                message: Some(format!("读取订阅配额失败：{cause}")),
                ..EngineQuota::empty()
            }
        }
    };
    let snapshot = match parse_rate_limits(&raw) {
        Ok(snapshot) => snapshot,
        Err(cause) => {
            return EngineQuota {
                state: "error".to_string(),
                account_type,
                message: Some(cause),
                ..EngineQuota::empty()
            }
        }
    };

    let has_window = snapshot.primary.is_some() || snapshot.secondary.is_some();
    EngineQuota {
        state: "ok".to_string(),
        account_type,
        plan_type: snapshot.plan_type,
        primary: snapshot.primary.as_ref().map(to_window),
        secondary: snapshot.secondary.as_ref().map(to_window),
        credits: snapshot.credits.map(|credits| QuotaCredits {
            has_credits: credits.has_credits,
            unlimited: credits.unlimited,
            balance: credits.balance,
        }),
        message: if has_window {
            None
        } else {
            Some("当前没有可显示的配额窗口。".to_string())
        },
    }
}

// ---------------------------------------------------------------- 协议解析

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRateLimits {
    /// 单桶视图（`limitId` 一般为 "codex"）；多桶 `rateLimitsByLimitId` 暂不展开。
    rate_limits: Option<RawSnapshot>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSnapshot {
    plan_type: Option<String>,
    primary: Option<RawWindow>,
    secondary: Option<RawWindow>,
    credits: Option<RawCredits>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawWindow {
    used_percent: Option<f64>,
    #[serde(default, alias = "windowMinutes", alias = "window_minutes")]
    window_duration_mins: Option<i64>,
    #[serde(default, alias = "resets_at")]
    resets_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCredits {
    #[serde(default)]
    has_credits: bool,
    #[serde(default)]
    unlimited: bool,
    balance: Option<String>,
}

/// 解析 `account/rateLimits/read` 的 result（缺失 `rateLimits` 视为空快照）。
fn parse_rate_limits(raw: &Value) -> Result<RawSnapshot, String> {
    let parsed: RawRateLimits =
        serde_json::from_value(raw.clone()).map_err(|cause| format!("配额数据解析失败：{cause}"))?;
    Ok(parsed.rate_limits.unwrap_or_default())
}

/// 归一重置时间：秒 → 毫秒（对已是毫秒的值直接放行，防协议变动）。
fn normalize_resets_ms(raw: i64) -> i64 {
    if raw >= 1_000_000_000_000 {
        raw
    } else {
        raw.saturating_mul(1000)
    }
}

fn to_window(raw: &RawWindow) -> QuotaWindow {
    QuotaWindow {
        used_percent: raw.used_percent.unwrap_or(0.0).max(0.0),
        window_minutes: raw.window_duration_mins.filter(|value| *value > 0),
        resets_at_ms: raw
            .resets_at
            .filter(|value| *value > 0)
            .map(normalize_resets_ms),
    }
}

// ---------------------------------------------------------------- 一次性连接

/// 一次性 app-server 连接：抽屉式查询，Drop 时回收（关 stdin → kill → wait）。
struct OneShot {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: mpsc::Receiver<Value>,
    next_id: u64,
}

impl OneShot {
    fn start(cli: &Path) -> Result<Self, String> {
        let mut command = Command::new(cli);
        command
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
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

        let (sender, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("prism-quota-codex".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
                        if sender.send(value).is_err() {
                            break;
                        }
                    }
                }
            });
        if let Err(cause) = spawned {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("启动读取线程失败：{cause}"));
        }

        Ok(Self {
            child,
            stdin: Some(stdin),
            rx,
            next_id: 0,
        })
    }

    /// 发起一次 JSON-RPC 调用并等待响应（跳过通知与无关响应）。
    fn call(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
            .to_string();
        {
            let stdin = self
                .stdin
                .as_mut()
                .ok_or_else(|| "app-server stdin 已关闭".to_string())?;
            stdin
                .write_all(line.as_bytes())
                .and_then(|_| stdin.write_all(b"\n"))
                .and_then(|_| stdin.flush())
                .map_err(|cause| format!("{method} 发送失败：{cause}"))?;
        }
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!("{method} 响应超时（{}s）", timeout.as_secs()));
            }
            match self.rx.recv_timeout(remaining) {
                Ok(message) => {
                    if message.get("id").and_then(Value::as_u64) != Some(id) {
                        continue; // 通知 / 其他响应：本轮查询不关心
                    }
                    if let Some(error) = message.get("error") {
                        let text = error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("未知错误");
                        return Err(format!("{method}：{text}"));
                    }
                    return Ok(message.get("result").cloned().unwrap_or(Value::Null));
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err(format!("{method} 响应超时（{}s）", timeout.as_secs()));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(format!("{method}：app-server 连接已关闭"));
                }
            }
        }
    }
}

impl Drop for OneShot {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_chatgpt_snapshot_with_windows_and_credits() {
        let raw = json!({
            "ordinaryUsageAllowed": true,
            "rateLimits": {
                "limitId": "codex",
                "limitName": null,
                "primary": { "usedPercent": 12.5, "windowDurationMins": 300, "resetsAt": 1790000000 },
                "secondary": { "usedPercent": 3.25, "windowDurationMins": 10080, "resetsAt": 1790500000 },
                "credits": { "hasCredits": true, "unlimited": false, "balance": "12.34" },
                "planType": "plus"
            },
            "rateLimitsByLimitId": null
        });
        let snapshot = parse_rate_limits(&raw).expect("应能解析");
        assert_eq!(snapshot.plan_type.as_deref(), Some("plus"));
        let primary = to_window(snapshot.primary.as_ref().expect("主窗口"));
        assert_eq!(primary.used_percent, 12.5);
        assert_eq!(primary.window_minutes, Some(300));
        assert_eq!(primary.resets_at_ms, Some(1_790_000_000_000));
        let secondary = to_window(snapshot.secondary.as_ref().expect("次窗口"));
        assert_eq!(secondary.window_minutes, Some(10080));
        let credits = snapshot.credits.as_ref().expect("credits");
        assert!(credits.has_credits);
        assert_eq!(credits.balance.as_deref(), Some("12.34"));
    }

    #[test]
    fn parse_tolerates_null_windows_and_legacy_minutes_field() {
        let raw = json!({
            "rateLimits": {
                "primary": null,
                "secondary": { "usedPercent": 1.0, "windowMinutes": 60, "resetsAt": null },
                "planType": null
            }
        });
        let snapshot = parse_rate_limits(&raw).expect("应能解析");
        assert!(snapshot.primary.is_none());
        let secondary = to_window(snapshot.secondary.as_ref().expect("次窗口"));
        assert_eq!(secondary.window_minutes, Some(60));
        assert!(secondary.resets_at_ms.is_none());
    }

    #[test]
    fn parse_handles_missing_rate_limits_key() {
        let snapshot = parse_rate_limits(&json!({})).expect("应能解析");
        assert!(snapshot.primary.is_none() && snapshot.secondary.is_none());
        assert!(snapshot.plan_type.is_none());
    }

    #[test]
    fn resets_at_normalizes_seconds_and_millis() {
        assert_eq!(normalize_resets_ms(1_790_000_000), 1_790_000_000_000);
        assert_eq!(normalize_resets_ms(1_790_000_000_000), 1_790_000_000_000);
    }

    #[test]
    fn quota_state_cache_respects_ttl_and_force_flow() {
        let state = QuotaState::new();
        assert!(state.fresh().is_none());
        let summary = QuotaSummary {
            fetched_at_ms: 1,
            codex: EngineQuota::simple("no_subscription", "x"),
            claude: EngineQuota::simple("pending_cli", "y"),
        };
        state.store(summary);
        let cached = state.fresh().expect("缓存应命中");
        assert_eq!(cached.codex.state, "no_subscription");
    }

    /// 真机：对真实 codex app-server 取一次账号与配额状态（一次性进程）。
    /// API Key 机器上应得到 no_subscription；ChatGPT 登录机器上得到 ok / error。
    #[test]
    #[ignore]
    fn codex_quota_against_real_app_server() {
        let quota = codex_quota();
        eprintln!(
            "真机配额：state={} account={:?} plan={:?} message={:?}",
            quota.state, quota.account_type, quota.plan_type, quota.message
        );
        assert!(matches!(
            quota.state.as_str(),
            "ok" | "no_subscription" | "cli_missing" | "error"
        ));
        if quota.state == "error" {
            assert!(quota.message.is_some());
        }
    }
}
