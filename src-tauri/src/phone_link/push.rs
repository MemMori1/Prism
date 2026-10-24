//! 企业微信群机器人 webhook 推送（单向：桌面 → 微信群）。
//!
//! 消息体只用 markdown 类型（群机器人支持的最可读格式）；payload 构造是
//! 纯函数，HTTP 发送独立出来便于测试与将来换成别的渠道适配器。

/// 推送超时：webhook 挂了也不能拖住回合线程。
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// markdown 正文截断：群里刷不出长文，摘要就够。
const PREVIEW_CAP: usize = 600;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PushKind {
    Done,
    Permission,
    Error,
}

/// 回合收口消息（成功 / 失败共用）。
pub fn done_message(engine: &str, status: &str, text: &str) -> String {
    let head = if status == "succeeded" { "✅ **手机会话已完成**" } else { "⚠️ **手机会话已结束**" };
    format!(
        "{head}\n> 引擎：{engine}\n> 状态：{status}\n\n{}",
        preview(text)
    )
}

/// 权限请求消息。
pub fn permission_message(engine: &str, title: &str) -> String {
    format!(
        "🔐 **手机会话等待授权**\n> 引擎：{engine}\n> 请求：{}\n\n请到手机对话页批准或拒绝。",
        preview(title)
    )
}

/// 回合出错消息。
pub fn error_message(engine: &str, error: &str) -> String {
    format!("❌ **手机会话出错**\n> 引擎：{engine}\n\n{}", preview(error))
}

/// 测试消息（保存 webhook 后点「发送测试消息」）。
pub fn test_message() -> String {
    "🔔 **Prism 手机链接**\n> 这是一条测试消息：webhook 配置成功，之后任务的完成 / 授权 / 出错会推到这里。".to_string()
}

/// 发送 markdown 消息；返回 Err 带企业微信返回的 errcode/errmsg。
pub fn send(webhook_url: &str, markdown: &str) -> Result<(), String> {
    let body = serde_json::json!({
        "msgtype": "markdown",
        "markdown": { "content": markdown },
    });
    let client = reqwest::blocking::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|error| format!("创建 HTTP 客户端失败：{error}"))?;
    let response = client
        .post(webhook_url.trim())
        .json(&body)
        .send()
        .map_err(|error| format!("推送请求失败：{error}"))?;
    let status = response.status();
    let payload: serde_json::Value = response
        .json()
        .map_err(|error| format!("解析 webhook 响应失败（HTTP {status}）：{error}"))?;
    let errcode = payload.get("errcode").and_then(serde_json::Value::as_i64).unwrap_or(-1);
    if errcode != 0 {
        let message = payload
            .get("errmsg")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("未知错误");
        return Err(format!("webhook 返回错误（{errcode}）：{message}"));
    }
    Ok(())
}

/// 截断成群消息友好的长度。
fn preview(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= PREVIEW_CAP {
        return text.to_string();
    }
    format!("{}…（已截断）", text.chars().take(PREVIEW_CAP).collect::<String>())
}

/// 推送事件的开关判断（纯函数，便于测试）。
pub fn should_push(events: &super::store::PushEvents, kind: PushKind, status: Option<&str>) -> bool {
    match kind {
        PushKind::Permission => events.on_permission,
        PushKind::Error => events.on_error,
        PushKind::Done => {
            if !events.on_done {
                return false;
            }
            // 失败的收口走 error 通道（更醒目），这里只推成功与主动打断。
            matches!(status, Some("succeeded") | Some("interrupted"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phone_link::store::PushEvents;

    #[test]
    fn messages_carry_engine_and_status() {
        let done = done_message("opencode", "succeeded", "改完了。");
        assert!(done.contains("✅"));
        assert!(done.contains("opencode"));
        let failed = done_message("codex", "failed", "炸了");
        assert!(failed.contains("⚠️"));
        assert!(permission_message("opencode", "run_command rm -rf /tmp/x").contains("run_command"));
        assert!(error_message("opencode", "超时").contains("超时"));
    }

    #[test]
    fn preview_truncates_long_text() {
        let long = "字".repeat(PREVIEW_CAP + 100);
        let text = preview(&long);
        assert!(text.chars().count() < PREVIEW_CAP + 20);
        assert!(text.ends_with("…（已截断）"));
    }

    #[test]
    fn push_switches_follow_config() {
        let events = PushEvents::default();
        assert!(should_push(&events, PushKind::Done, Some("succeeded")));
        assert!(!should_push(&events, PushKind::Done, Some("failed")), "失败走 error 通道");
        assert!(should_push(&events, PushKind::Permission, None));
        assert!(should_push(&events, PushKind::Error, None));

        let quiet = PushEvents { on_done: false, on_permission: false, on_error: false };
        assert!(!should_push(&quiet, PushKind::Done, Some("succeeded")));
        assert!(!should_push(&quiet, PushKind::Permission, None));
        assert!(!should_push(&quiet, PushKind::Error, None));
    }
}
