//! 权限闸门：只读 / 逐次询问 / 完全放行。
//!
//! 原生引擎的工具默认**没有任何拦截**——模型想写就写、想跑就跑。这里补一道闸：
//! 执行 `write_file` / `edit_file` / `run_command` 之前先按当前档位决定，
//! 「询问」档会发一条 `permission.asked` 事件卡住等待用户回复，回复走
//! `session_permission_reply`（和另外两个引擎同一套契约，前端无需区分来源）。
//!
//! 读类工具（read_file / list_files / grep）不拦：它们本来就被限制在工作目录内。

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tauri::AppHandle;

use crate::engines::opencode::runtime::PermissionRequest;

use super::tools;

/// 三档权限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 只读：写文件、改文件、跑命令一律拒绝。
    ReadOnly,
    /// 逐次询问：危险动作先问（默认）。
    Ask,
    /// 完全放行：不问。
    Full,
}

/// 前端传的档位字符串 → 枚举。认不出来一律按「询问」——宁可多问一句。
pub fn parse(raw: Option<&str>) -> Mode {
    match raw.unwrap_or("").trim() {
        "read-only" | "readonly" | "read_only" => Mode::ReadOnly,
        "full" | "full-access" | "full_access" => Mode::Full,
        _ => Mode::Ask,
    }
}

impl Mode {
    /// 回到前端用的档位字符串（`parse` 的逆）。
    ///
    /// 工具上下文存字符串而不是 `Mode`，是为了让工具层不必反过来依赖
    /// 审批层——审批层要查工具注册表，方向不能反。
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::ReadOnly => "read-only",
            Mode::Ask => "ask",
            Mode::Full => "full",
        }
    }
}

/// 工具 → 动作类别；None＝不需要授权（读类）。
///
/// 三个查询函数都改成查工具注册表：加工具时不必再回来补 match，
/// 权限信息跟着工具登记项一起走。
pub fn action_of(tool: &str) -> Option<&'static str> {
    tools::find(tool).and_then(|def| def.action)
}

/// 权限卡片上「动了什么」：写 / 改给路径，跑命令给命令本身。
pub fn resources_of(tool: &str, input: &Value) -> Vec<String> {
    let Some(key) = tools::find(tool).and_then(|def| def.resource_key) else {
        return Vec::new();
    };
    match input.get(key).and_then(Value::as_str) {
        Some(text) if !text.trim().is_empty() => vec![text.to_string()],
        _ => Vec::new(),
    }
}

/// 权限卡片的标题。
pub fn message_of(tool: &str) -> String {
    tools::find(tool)
        .map(|def| def.summary)
        .filter(|summary| !summary.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{tool} 需要授权"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny(String),
}

/// 一次判定的结果。`None`＝需要问用户。
pub fn evaluate(mode: Mode, action: &str, always: bool) -> Option<Verdict> {
    match mode {
        Mode::Full => Some(Verdict::Allow),
        Mode::ReadOnly => Some(Verdict::Deny(format!(
            "当前权限是「只读」：{action} 被拦下了。改成「每次询问」或「完全访问」再试。"
        ))),
        Mode::Ask => {
            if always {
                Some(Verdict::Allow)
            } else {
                None
            }
        }
    }
}

/// 等用户回复的请求。
struct Pending {
    id: String,
    session_id: String,
    action: String,
    resources: Vec<String>,
    message: String,
    call_id: String,
    decision: Mutex<Option<String>>,
    cv: Condvar,
}

impl Pending {
    fn payload(&self) -> Value {
        json!({
            "id": self.id,
            "sessionID": self.session_id,
            "action": self.action,
            "resources": self.resources,
            "message": self.message,
            "source": { "id": self.call_id, "type": "tool" },
        })
    }

    fn request(&self) -> PermissionRequest {
        PermissionRequest {
            id: self.id.clone(),
            session_id: self.session_id.clone(),
            action: self.action.clone(),
            resources: self.resources.clone(),
            message: Some(self.message.clone()),
            source_message_id: None,
            source_call_id: Some(self.call_id.clone()),
        }
    }
}

fn pending() -> &'static Mutex<HashMap<String, Arc<Pending>>> {
    static PENDING: std::sync::OnceLock<Mutex<HashMap<String, Arc<Pending>>>> =
        std::sync::OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 「总是允许」按会话 + 动作记，进程内有效（重开应用会重新问一次）。
fn always_allowed() -> &'static Mutex<HashMap<String, HashSet<String>>> {
    static ALWAYS: std::sync::OnceLock<Mutex<HashMap<String, HashSet<String>>>> =
        std::sync::OnceLock::new();
    ALWAYS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn is_always(session_id: &str, action: &str) -> bool {
    always_allowed()
        .lock()
        .map(|map| map.get(session_id).is_some_and(|set| set.contains(action)))
        .unwrap_or(false)
}

/// 记下「这个会话以后这类动作不用再问」。
pub fn remember_always(session_id: &str, action: &str) {
    if let Ok(mut map) = always_allowed().lock() {
        map.entry(session_id.to_string())
            .or_default()
            .insert(action.to_string());
    }
}

fn new_request_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or(0);
    format!("native-perm-{nanos}")
}

/// 闸门：放行 / 拒绝 / 卡住等用户回复。
///
/// `should_stop` 每 200ms 问一次——用户点停止时不能还傻等着审批。
pub fn gate(
    app: &AppHandle,
    session_id: &str,
    mode: Mode,
    action: &str,
    resources: Vec<String>,
    message: String,
    call_id: &str,
    should_stop: &dyn Fn() -> bool,
) -> Verdict {
    if let Some(verdict) = evaluate(mode, action, is_always(session_id, action)) {
        return verdict;
    }

    let entry = Arc::new(Pending {
        id: new_request_id(),
        session_id: session_id.to_string(),
        action: action.to_string(),
        resources,
        message,
        call_id: call_id.to_string(),
        decision: Mutex::new(None),
        cv: Condvar::new(),
    });
    if let Ok(mut map) = pending().lock() {
        map.insert(entry.id.clone(), entry.clone());
    }
    crate::engines::native::emit_event(app, "permission.asked", entry.payload());

    let decided = loop {
        {
            let guard = match entry.decision.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            if let Some(decision) = guard.clone() {
                break Some(decision);
            }
            // 等回复时释放锁（否则 reply 拿不到）；超时后重新检查一次停止标记。
            let (guard, _) = entry
                .cv
                .wait_timeout(guard, Duration::from_millis(200))
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            drop(guard);
        }
        if should_stop() {
            break None;
        }
    };

    if let Ok(mut map) = pending().lock() {
        map.remove(&entry.id);
    }

    let stopped = decided.is_none();
    let decision = decided.unwrap_or_else(|| "reject".to_string());
    crate::engines::native::emit_event(
        app,
        if decision == "reject" {
            "permission.rejected"
        } else {
            "permission.replied"
        },
        json!({
            "id": entry.id,
            "sessionID": session_id,
            "requestID": entry.id,
            "decision": decision,
        }),
    );

    match decision.as_str() {
        "once" => Verdict::Allow,
        "always" => {
            remember_always(session_id, action);
            Verdict::Allow
        }
        _ => Verdict::Deny(if stopped {
            "已停止，这次工具调用没有执行。".to_string()
        } else {
            format!("用户拒绝了这次 {action}（可以在输入框旁改权限档位）。")
        }),
    }
}

/// 前端轮询用：这个会话现在挂着哪些请求。
pub fn list(session_id: &str) -> Vec<PermissionRequest> {
    let map = match pending().lock() {
        Ok(map) => map,
        Err(poisoned) => poisoned.into_inner(),
    };
    let mut items: Vec<PermissionRequest> = map
        .values()
        .filter(|entry| entry.session_id == session_id)
        .map(|entry| entry.request())
        .collect();
    items.sort_by(|a, b| a.id.cmp(&b.id));
    items
}

/// 回复一个挂起的请求：once / always / reject。
pub fn reply(request_id: &str, decision: &str) -> Result<(), String> {
    let normalized = match decision {
        "once" | "always" | "reject" => decision,
        other => return Err(format!("不认识的回复：{other}")),
    };
    let entry = {
        let map = match pending().lock() {
            Ok(map) => map,
            Err(poisoned) => poisoned.into_inner(),
        };
        map.get(request_id).cloned()
    };
    let entry = entry.ok_or_else(|| format!("没有这个待审批请求：{request_id}"))?;
    {
        let mut slot = match entry.decision.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        if slot.is_some() {
            return Ok(()); // 重复回复：忽略，不报错
        }
        *slot = Some(normalized.to_string());
    }
    entry.cv.notify_all();
    Ok(())
}

/// 会话被停止 / 结束时，把它挂着的请求都放掉（否则线程要等到下一次轮询才退）。
pub fn release_session(session_id: &str) {
    let ids: Vec<String> = {
        let map = match pending().lock() {
            Ok(map) => map,
            Err(poisoned) => poisoned.into_inner(),
        };
        map.iter()
            .filter(|(_, entry)| entry.session_id == session_id)
            .map(|(id, _)| id.clone())
            .collect()
    };
    for id in ids {
        let _ = reply(&id, "reject");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_falls_back_to_ask() {
        assert_eq!(parse(Some("read-only")), Mode::ReadOnly);
        assert_eq!(parse(Some("full")), Mode::Full);
        assert_eq!(parse(Some("ask")), Mode::Ask);
        assert_eq!(parse(None), Mode::Ask);
        assert_eq!(parse(Some("随便什么")), Mode::Ask);
    }

    #[test]
    fn tool_to_action() {
        assert_eq!(action_of("write_file"), Some("edit"));
        assert_eq!(action_of("edit_file"), Some("edit"));
        assert_eq!(action_of("run_command"), Some("run"));
        assert_eq!(action_of("read_file"), None);
        assert_eq!(action_of("grep"), None);
    }

    #[test]
    fn resources_and_messages() {
        let input = json!({ "path": "src/a.ts", "content": "x" });
        assert_eq!(resources_of("write_file", &input), vec!["src/a.ts"]);
        assert_eq!(resources_of("run_command", &input), Vec::<String>::new());
        let command = json!({ "command": "npm test" });
        assert_eq!(resources_of("run_command", &command), vec!["npm test"]);
        assert!(message_of("run_command").contains("命令"));
        assert!(message_of("edit_file").contains("修改"));
    }

    #[test]
    fn read_only_denies_with_reason() {
        match evaluate(Mode::ReadOnly, "edit", false) {
            Some(Verdict::Deny(text)) => assert!(text.contains("只读")),
            other => panic!("只读档应该拒绝：{other:?}"),
        }
    }

    #[test]
    fn full_allows_everything() {
        assert_eq!(evaluate(Mode::Full, "run", false), Some(Verdict::Allow));
    }

    #[test]
    fn ask_needs_confirmation_unless_already_allowed() {
        assert_eq!(evaluate(Mode::Ask, "edit", false), None);
        assert_eq!(evaluate(Mode::Ask, "edit", true), Some(Verdict::Allow));
    }

    #[test]
    fn reply_rejects_unknown_request_and_decision() {
        assert!(reply("no-such-request", "once").is_err());
        assert!(reply("no-such-request", "maybe").is_err());
    }
}
