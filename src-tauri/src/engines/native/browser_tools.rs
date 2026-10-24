//! Agent 的浏览器：`browser.open` / `browser.read` / `browser.act`。
//!
//! 用的是**面板里那块同一个 webview**（见 `browser.rs`）：用户眼前看到的页面就是
//! Agent 在读的页面，不做影子会话。好处是透明可核对——它点了什么、现在停在哪一页，
//! 面板里都看得见；代价是它会把用户的页面导走，所以有一节说明写在
//! `docs/PANEL_BROWSER.md` 里。
//!
//! 为什么值得单独做一套而不是只给 `fetch`：`fetch` 拿不到 JS 渲染后的内容，也点不了
//! 按钮。查一个需要登录/需要点「展开」的文档、走一遍表单，只有真浏览器能办。
//!
//! 边界：
//! - 只认 http(s)（`browser::normalize_url` 与 `on_navigation` 两道）；
//! - 不挂审批闸门（在 DAG 节点里弹审批没人点会卡死），控制点是设置里的开关 +
//!   `risk = act` 的显式标注 + 面板可见；
//! - `delegable = false`：一块浏览器只能有一个司机，子代理 / DAG 并行节点不参与，
//!   免得两个节点互相把页面导走。要并行就等「每节点一块隔离 webview」。

use serde_json::{json, Value};

use super::tools::{
    builtin_full, Network, Permission, Risk, ToolContext, ToolDef, ToolMeta, ToolOutcome,
    ToolResult, ToolScope,
};
use crate::browser;

/// `browser.read` 默认回多少字符。
const READ_CHARS: usize = 12_000;
const MAX_READ_CHARS: usize = 40_000;
/// 导航后最多等多久（文档站慢一点也够）。
const LOAD_WAIT: std::time::Duration = std::time::Duration::from_secs(15);
/// 点击之后给页面跳转留的时间。
const ACT_WAIT: std::time::Duration = std::time::Duration::from_secs(8);

pub fn tools() -> Vec<std::sync::Arc<ToolDef>> {
    vec![
        builtin_full(
            "browser.open",
            "浏览器打开",
            open_schema,
            Permission::ReadOnly,
            None,
            None,
            "",
            // 一块浏览器一个司机：子代理 / DAG 并行节点不给。
            false,
            ToolMeta {
                tags: &["web", "browser"],
                risk: Risk::Read,
                network: Network::Outbound,
                sandbox: None,
                scope: ToolScope::Any,
                needs: &[],
            },
            run_open,
        ),
        builtin_full(
            "browser.read",
            "读网页正文",
            read_schema,
            Permission::ReadOnly,
            None,
            None,
            "",
            false,
            ToolMeta {
                tags: &["web", "browser"],
                risk: Risk::Read,
                network: Network::Outbound,
                sandbox: None,
                scope: ToolScope::Any,
                needs: &[],
            },
            run_read,
        ),
        builtin_full(
            "browser.act",
            "操作网页",
            act_schema,
            Permission::ReadOnly,
            None,
            None,
            "",
            false,
            ToolMeta {
                tags: &["web", "browser"],
                // 点按钮 / 填表单会改变远端状态：本地磁盘没动，但别当成「只读浏览」。
                risk: Risk::Act,
                network: Network::Outbound,
                sandbox: None,
                scope: ToolScope::Any,
                needs: &[],
            },
            run_act,
        ),
    ]
}

fn open_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "browser.open",
            "description": "在面板浏览器里打开一个地址（会和用户看到的是同一页）。可用于需要 JS 渲染或需要登录的页面。打开后用 browser.read 读正文，用 browser.act 点击 / 输入。只支持 http(s)。",
            "parameters": {
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "完整地址或裸域名，如 example.com/docs" }
                },
                "required": ["url"]
            }
        }
    })
}

fn read_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "browser.read",
            "description": "读当前页面（含 JS 渲染后的内容）：标题、地址、正文、前 30 个链接。可以用 selector 只读某一块。比 fetch 多的是渲染后的 DOM 与登录态。",
            "parameters": {
                "type": "object",
                "properties": {
                    "selector": { "type": "string", "description": "CSS 选择器，只读这块；省略读整页正文" },
                    "max_chars": { "type": "integer", "description": "正文最多返回多少字符，默认 12000，上限 40000" }
                }
            }
        }
    })
}

fn act_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "browser.act",
            "description": "在页面上操作。action=click：按 selector 或文字点一个元素；type：往输入框写字（submit=true 顺带回车）；back/forward/reload：导航；wait：等某个元素出现。改完页面用 browser.read 复核。",
            "parameters": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["click", "type", "back", "forward", "reload", "wait"],
                        "description": "要做的动作"
                    },
                    "selector": { "type": "string", "description": "CSS 选择器；click 时可以省略，改用 text" },
                    "text": { "type": "string", "description": "click：按可见文字找元素；type：要写入的内容" },
                    "submit": { "type": "boolean", "description": "type 时是否提交（回车）" },
                    "timeout_ms": { "type": "integer", "description": "wait 的超时，默认 10000" }
                },
                "required": ["action"]
            }
        }
    })
}

fn app_of<'a>(ctx: &'a ToolContext<'_>) -> Result<&'a tauri::AppHandle, String> {
    ctx.app
        .ok_or_else(|| "浏览器工具需要应用运行时（当前上下文没有窗口）".to_string())
}

fn run_open(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    // 先把参数看完再要窗口：参数错的时候不该让用户以为「浏览器坏了」。
    let Some(url) = args.get("url").and_then(Value::as_str).map(str::trim) else {
        return ToolResult::fail("缺少 url");
    };
    if url.is_empty() {
        return ToolResult::fail("url 不能为空");
    }
    let app = match app_of(ctx) {
        Ok(app) => app,
        Err(error) => return ToolResult::fail(error),
    };
    let snapshot = match browser::open(app, url) {
        Ok(snapshot) => snapshot,
        Err(error) => return ToolResult::fail(error),
    };
    // 等页面就绪再返回：模型下一步就要读，早读一步拿到的是空白。
    let ready = browser::wait_ready(app, LOAD_WAIT).is_ok();
    let waited = browser::snapshot();
    let mut text = format!("已打开 {}", snapshot.url);
    if !waited.title.trim().is_empty() {
        text.push_str(&format!("\n标题：{}", waited.title));
    }
    if !ready {
        text.push_str("\n（页面还在加载：内容可能不完整，必要时用 browser.act 的 wait 等某个元素）");
    }
    ToolResult::plain(ToolOutcome::ok(text))
}

fn run_read(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let app = match app_of(ctx) {
        Ok(app) => app,
        Err(error) => return ToolResult::fail(error),
    };
    let selector = args
        .get("selector")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let limit = args
        .get("max_chars")
        .and_then(Value::as_u64)
        .unwrap_or(READ_CHARS as u64)
        .clamp(500, MAX_READ_CHARS as u64) as usize;

    let _ = browser::wait_ready(app, std::time::Duration::from_secs(3));
    let value = match browser::eval(app, &browser::read_script(selector)) {
        Ok(value) => value,
        Err(error) => return ToolResult::fail(error),
    };
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        let error = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("读取页面失败");
        return ToolResult::fail(error.to_string());
    }

    let url = value.get("url").and_then(Value::as_str).unwrap_or("");
    let title = value.get("title").and_then(Value::as_str).unwrap_or("");
    let body = value.get("text").and_then(Value::as_str).unwrap_or("");
    let mut out = format!("{title}\n{url}\n\n{body}");
    let total = body.chars().count();
    if total > limit {
        let head: String = body.chars().take(limit).collect();
        out = format!(
            "{title}\n{url}\n\n{head}\n…（正文已截断：{limit}/{total} 字符；要看更多把 max_chars 调大，或先用 selector 缩小范围）"
        );
    }

    if let Some(links) = value.get("links").and_then(Value::as_array) {
        if !links.is_empty() {
            out.push_str("\n\n可点的链接：");
            for item in links.iter().take(30) {
                let text = item.get("text").and_then(Value::as_str).unwrap_or("");
                let href = item.get("href").and_then(Value::as_str).unwrap_or("");
                out.push_str(&format!("\n- {text} → {href}"));
            }
        }
    }
    ToolResult::plain(ToolOutcome::ok(out))
}

fn run_act(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let action = args
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let selector = args
        .get("selector")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let text = args.get("text").and_then(Value::as_str);
    let submit = args
        .get("submit")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // 参数先校验完，再问窗口：模型少给一个字段时，报错要直接说字段。
    if action.is_empty() {
        return ToolResult::fail("缺少 action");
    }
    let clickable = selector.is_some() || text.map(str::trim).is_some_and(|value| !value.is_empty());
    match action {
        "click" if !clickable => return ToolResult::fail("click 需要 selector 或 text"),
        "type" if selector.is_none() => return ToolResult::fail("type 需要 selector"),
        "type" if text.is_none() => return ToolResult::fail("type 需要 text"),
        "wait" if selector.is_none() => return ToolResult::fail("wait 需要 selector"),
        "click" | "type" | "wait" | "back" | "forward" | "reload" => {}
        other => {
            return ToolResult::fail(format!(
                "不认识的动作「{other}」：可用 click / type / back / forward / reload / wait"
            ))
        }
    }

    let app = match app_of(ctx) {
        Ok(app) => app,
        Err(error) => return ToolResult::fail(error),
    };

    match action {
        "click" => {
            let value = match browser::eval(app, &browser::click_script(selector, text)) {
                Ok(value) => value,
                Err(error) => return ToolResult::fail(error),
            };
            if value.get("ok").and_then(Value::as_bool) != Some(true) {
                let error = value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("点击失败");
                return ToolResult::fail(error.to_string());
            }
            let clicked = value.get("clicked").and_then(Value::as_str).unwrap_or("");
            let _ = browser::wait_ready(app, ACT_WAIT);
            let after = browser::snapshot();
            ToolResult::plain(ToolOutcome::ok(format!(
                "已点击「{clicked}」，当前 {}（{}）",
                after.title, after.url
            )))
        }
        "type" => {
            let (Some(selector), Some(text)) = (selector, text) else {
                return ToolResult::fail("type 需要 selector 与 text");
            };
            let value = match browser::eval(app, &browser::type_script(selector, text, submit)) {
                Ok(value) => value,
                Err(error) => return ToolResult::fail(error),
            };
            if value.get("ok").and_then(Value::as_bool) != Some(true) {
                let error = value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("写入失败");
                return ToolResult::fail(error.to_string());
            }
            if submit {
                let _ = browser::wait_ready(app, ACT_WAIT);
                let after = browser::snapshot();
                ToolResult::plain(ToolOutcome::ok(format!(
                    "已填入并提交，当前 {}（{}）",
                    after.title, after.url
                )))
            } else {
                ToolResult::plain(ToolOutcome::ok(format!("已填入 {selector}")))
            }
        }
        "back" | "forward" | "reload" => {
            if let Err(error) = browser::history(app, action) {
                return ToolResult::fail(error);
            }
            let _ = browser::wait_ready(app, ACT_WAIT);
            let after = browser::snapshot();
            ToolResult::plain(ToolOutcome::ok(format!(
                "{}：{}（{}）",
                label_of(action),
                after.title,
                after.url
            )))
        }
        _ => {
            // 只剩 "wait"：轮询元素是否出现（eval 不能等 Promise，只能在 Rust 侧等）。
            let Some(selector) = selector else {
                return ToolResult::fail("wait 需要 selector");
            };
            let timeout = args
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .unwrap_or(10_000)
                .clamp(500, 60_000);
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout);
            let script = browser::exists_script(selector);
            loop {
                let found = browser::eval(app, &script)
                    .map(|value| value.as_bool().unwrap_or(false))
                    .unwrap_or(false);
                if found {
                    return ToolResult::plain(ToolOutcome::ok(format!("元素已出现：{selector}")));
                }
                if std::time::Instant::now() >= deadline {
                    return ToolResult::fail(format!("等了 {timeout}ms，元素仍未出现：{selector}"));
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        }
    }
}

fn label_of(action: &str) -> &'static str {
    match action {
        "back" => "已后退",
        "forward" => "已前进",
        _ => "已刷新",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_are_registered_with_expected_risk() {
        let list = tools();
        let names: Vec<String> = list.iter().map(|tool| tool.name.to_string()).collect();
        assert_eq!(names, vec!["browser.open", "browser.read", "browser.act"]);
        let act = list.iter().find(|tool| tool.name == "browser.act").unwrap();
        // 点按钮会改远端状态，不能标成「只读浏览」。
        assert_eq!(act.meta.risk, Risk::Act);
        assert_eq!(act.meta.network, Network::Outbound);
        // 一块浏览器一个司机：不委派。
        assert!(list.iter().all(|tool| !tool.delegable));
    }

    #[test]
    fn schema_names_use_wire_form() {
        for tool in tools() {
            let schema = tool.schema_value();
            let name = schema["function"]["name"].as_str().unwrap();
            assert!(!name.contains('.'), "发给模型的名字不能带点：{name}");
        }
    }

    /// 没有窗口上下文时要说清为什么，而不是 panic。
    #[test]
    fn fails_cleanly_without_app_handle() {
        let enabled = crate::engines::native::tools::EnabledTools::all();
        let ctx = ToolContext {
            app: None,
            session_id: "",
            directory: "/tmp",
            call_id: "",
            provider: None,
            model: None,
            permission: None,
            enabled: &enabled,
            dag: None,
            should_stop: &|| false,
        };
        let result = run_open(&ctx, &json!({ "url": "example.com" }));
        assert!(result.outcome.error.unwrap().contains("需要应用运行时"));
    }

    #[test]
    fn open_rejects_missing_url() {
        let enabled = crate::engines::native::tools::EnabledTools::all();
        let ctx = ToolContext {
            app: None,
            session_id: "",
            directory: "/tmp",
            call_id: "",
            provider: None,
            model: None,
            permission: None,
            enabled: &enabled,
            dag: None,
            should_stop: &|| false,
        };
        assert!(run_open(&ctx, &json!({})).outcome.error.unwrap().contains("缺少 url"));
        assert!(run_act(&ctx, &json!({ "action": "click" }))
            .outcome
            .error
            .unwrap()
            .contains("需要 selector 或 text"));
    }
}
