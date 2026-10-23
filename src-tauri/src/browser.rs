//! 面板浏览器：把一块**真的**浏览器挂进主窗口。
//!
//! 为什么不是 `<iframe>`：`X-Frame-Options` / `frame-ancestors` 会把 GitHub、Google
//! 这类站点直接拒掉，剩下能开的只有本机 dev server——那不叫浏览器。这里用 Tauri 的
//! 子 webview（`Window::add_child`，目前还标着 `unstable`），一块真正的 WKWebView /
//! WebView2，位置和尺寸由前端按 DOM 里那块区域算好报进来。
//!
//! 三个绕不开的现实问题：
//! - **z 序**：子 webview 是原生视图，永远画在我们 DOM 之上。所以切走标签、收起侧栏、
//!   弹设置之类的浮层时，必须显式把它藏起来（[`set_visible`]），否则它会盖住弹窗。
//! - **尺寸**：它不参与我们的布局，前端把内容区矩形报上来（[`set_bounds`]）才会跟手。
//! - **失败**：创建失败（运行时拒绝、平台限制）要能退化成「用系统浏览器打开」，
//!   不能把整个面板拖垮。
//!
//! 它同时也是 Agent 的浏览器：`web.*` 工具通过 [`eval`] 在这块页面上读文本、点按钮、
//! 填表单——用户在面板里看到的和 Agent 读到的是同一个页面，不做影子会话。
//!
//! 访问限制：只允许 http(s)（`on_navigation` 里再拦一道），`file://` 与 `javascript:`
//! 一律放行不了——页面里的链接不能拿它去读本地文件。

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::webview::{PageLoadEvent, Webview, WebviewBuilder};
use tauri::{AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Url, WebviewUrl, Wry};

/// 子 webview 的标签：全局只有一块，用户可以点、Agent 也可以点，双方看同一页。
const LABEL: &str = "browser";
/// 页面向前端推状态的事件名。
pub const STATE_EVENT: &str = "browser.state";
/// eval 等结果的超时：页面卡死时不该把工具调用一起卡死。
const EVAL_TIMEOUT: Duration = Duration::from_secs(8);
/// 起始尺寸：先给 1×1 再藏起来，等前端报真实矩形。
const START_SIZE: f64 = 1.0;

/// 给前端看的浏览器状态。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserSnapshot {
    /// 当前地址；没开过浏览器时为空。
    pub url: String,
    pub title: String,
    pub loading: bool,
    /// webview 是否已创建。
    pub open: bool,
}

impl Default for BrowserSnapshot {
    fn default() -> Self {
        BrowserSnapshot {
            url: String::new(),
            title: String::new(),
            loading: false,
            open: false,
        }
    }
}

struct Browser {
    webview: Option<Webview<Wry>>,
    url: String,
    title: String,
    loading: bool,
}

impl Browser {
    fn snapshot(&self) -> BrowserSnapshot {
        BrowserSnapshot {
            url: self.url.clone(),
            title: self.title.clone(),
            loading: self.loading,
            open: self.webview.is_some(),
        }
    }
}

fn state() -> &'static Mutex<Browser> {
    static STATE: OnceLock<Mutex<Browser>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(Browser {
            webview: None,
            url: String::new(),
            title: String::new(),
            loading: false,
        })
    })
}

/// 建 webview 的那一段要串行：两个调用同时进来会各建一块，第二块把第一块顶掉。
fn create_lock() -> &'static Mutex<()> {
    static LOCK: Mutex<()> = Mutex::new(());
    &LOCK
}

fn lock() -> std::sync::MutexGuard<'static, Browser> {
    state().lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 前端要的矩形（CSS 像素＝逻辑像素，和窗口客户区左上角对齐）。
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// 读一次当前状态（面板挂载时用它同步，不必等事件）。
pub fn snapshot() -> BrowserSnapshot {
    lock().snapshot()
}

fn emit(app: &AppHandle) {
    let _ = app.emit(STATE_EVENT, lock().snapshot());
}

/// 把用户/Agent 给的地址收敛成能访问的 URL。
///
/// 规则与浏览器地址栏一致：没写协议就补，本机地址默认 http（dev server 极少上
/// https），其余默认 https。只放 http(s)。
pub fn normalize_url(raw: &str) -> Result<String, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Err("地址不能为空".to_string());
    }
    // `javascript:alert(1)` / `mailto:x@y` 这种「有协议、但没写 //」要单独认，
    // 否则会被当成域名补成 https://javascript:alert(1) 再报「端口不对」。
    if !text.contains("://") {
        if let Some(colon) = text.find(':') {
            let head = &text[..colon];
            let dotted = head.contains('.');
            let scheme_like = !head.is_empty()
                && !dotted
                && head.chars().next().is_some_and(|ch| ch.is_ascii_alphabetic())
                && head
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'));
            if scheme_like && !is_loopback_host(text) {
                return Err(format!(
                    "只支持 http(s)，不支持 {head}://——本地文件请用文件面板打开"
                ));
            }
        }
    }
    let with_scheme = if text.contains("://") {
        text.to_string()
    } else if is_loopback_host(text) {
        format!("http://{text}")
    } else {
        format!("https://{text}")
    };
    let url = Url::parse(&with_scheme).map_err(|error| format!("地址不合法（{text}）：{error}"))?;
    match url.scheme() {
        "http" | "https" => Ok(url.to_string()),
        other => Err(format!(
            "只支持 http(s)，不支持 {other}://——本地文件请用文件面板打开"
        )),
    }
}

/// `localhost:3000` / `127.0.0.1:8080` 这类：没写协议时优先按 http 猜。
fn is_loopback_host(text: &str) -> bool {
    let host = text.split('/').next().unwrap_or(text);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let host = host.split(':').next().unwrap_or(host);
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

/// 确保 webview 存在（幂等）。`url` 只在首次创建时使用。
pub fn ensure(app: &AppHandle, url: Option<&str>) -> Result<(), String> {
    if lock().webview.is_some() {
        return Ok(());
    }
    let _creating = create_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // 抢到锁之后再确认一次：可能在等锁的时候别人已经建好了。
    if lock().webview.is_some() {
        return Ok(());
    }

    let start = match url {
        Some(raw) => normalize_url(raw)?,
        None => "about:blank".to_string(),
    };
    let target = Url::parse(&start).map_err(|error| format!("地址不合法：{error}"))?;
    let window = app
        .get_window("main")
        .ok_or_else(|| "找不到主窗口，面板浏览器无法创建".to_string())?;

    let handle = app.clone();
    let builder = WebviewBuilder::new(LABEL, WebviewUrl::External(target))
        // 页面里的链接也叫我们：只许 http(s)，不许 file:// 之类的口子。
        .on_navigation(|url| matches!(url.scheme(), "http" | "https" | "about"))
        .on_page_load(move |webview, payload| {
            {
                let mut guard = lock();
                guard.url = webview.url().map(|url| url.to_string()).unwrap_or_default();
                guard.loading = matches!(payload.event(), PageLoadEvent::Started);
            }
            emit(&handle);
        })
        .on_document_title_changed({
            let handle = app.clone();
            move |_webview, title| {
                lock().title = title;
                emit(&handle);
            }
        });

    let webview = window
        .add_child(
            builder,
            LogicalPosition::new(0.0, 0.0),
            LogicalSize::new(START_SIZE, START_SIZE),
        )
        .map_err(|error| format!("创建内嵌浏览器失败（{error}）；可以改用系统浏览器打开"))?;
    // 先藏起来：尺寸要等前端把内容区矩形报上来，露一帧 1×1 的黑块更难看。
    let _ = webview.hide();

    {
        let mut guard = lock();
        guard.url = start;
        guard.title.clear();
        guard.loading = false;
        guard.webview = Some(webview);
    }
    emit(app);
    Ok(())
}

/// 在面板浏览器里打开一个地址（不存在就先建）。
pub fn open(app: &AppHandle, raw: &str) -> Result<BrowserSnapshot, String> {
    let url = normalize_url(raw)?;
    ensure(app, Some(&url))?;
    let parsed = Url::parse(&url).map_err(|error| error.to_string())?;
    with_webview(|webview| {
        webview
            .navigate(parsed)
            .map_err(|error| format!("导航失败：{error}"))
    })?;
    {
        let mut guard = lock();
        guard.url = url;
        guard.loading = true;
    }
    emit(app);
    Ok(snapshot())
}

/// 后退 / 前进 / 刷新：都走页面自己的 history，避免我们自己维护一份栈。
pub fn history(app: &AppHandle, direction: &str) -> Result<(), String> {
    let js = match direction {
        "back" => "history.back()",
        "forward" => "history.forward()",
        "reload" => "history.go(0)",
        other => return Err(format!("不认识的方向：{other}")),
    };
    eval(app, js)?;
    Ok(())
}

/// 同步位置与尺寸。面板布局变了（窗口缩放、抽屉拖动、标签切换）就要调一次。
pub fn set_bounds(_app: &AppHandle, bounds: Bounds) -> Result<(), String> {
    let width = bounds.width.max(1.0);
    let height = bounds.height.max(1.0);
    with_webview(|webview| {
        webview
            .set_position(LogicalPosition::new(bounds.x, bounds.y))
            .and_then(|_| webview.set_size(LogicalSize::new(width, height)))
            .map_err(|error| format!("调整浏览器尺寸失败：{error}"))
    })
}

/// 显示 / 隐藏。**所有盖在它上面的 DOM 浮层出现时都要隐藏**，否则原生视图会压住弹窗。
pub fn set_visible(_app: &AppHandle, visible: bool) -> Result<(), String> {
    with_webview(|webview| {
        if visible {
            webview.show()
        } else {
            webview.hide()
        }
        .map_err(|error| format!("切换浏览器可见性失败：{error}"))
    })
}

/// 在页面上执行一段 JS，拿回结果（同步等待，带超时）。
///
/// 前端与 Agent 工具都走这里：`eval_with_callback` 会把脚本的返回值序列化成 JSON。
pub fn eval(_app: &AppHandle, script: &str) -> Result<serde_json::Value, String> {
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    with_webview(|webview| {
        webview
            .eval_with_callback(script.to_string(), move |result| {
                let _ = tx.send(result);
            })
            .map_err(|error| format!("执行脚本失败：{error}"))
    })?;
    let raw = rx
        .recv_timeout(EVAL_TIMEOUT)
        .map_err(|_| "页面没有在 8 秒内返回结果（可能卡住了）".to_string())?;
    parse_eval(&raw)
}

/// 有的平台会把返回值再包一层字符串，这里两种都认。
fn parse_eval(raw: &str) -> Result<serde_json::Value, String> {
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|error| format!("页面返回值不是 JSON（{error}）：{raw}"))?;
    Ok(match value {
        serde_json::Value::String(text) => {
            serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text))
        }
        other => other,
    })
}

/// 等页面就绪：导航之后立刻 eval 往往还是上一页 / 空白。
pub fn wait_ready(app: &AppHandle, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(value) = eval(app, "document.readyState") {
            let text = value.as_str().unwrap_or_default().to_string();
            if text == "complete" || text == "interactive" {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            return Err("等页面加载超时（页面可能一直在转圈）".to_string());
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn with_webview<R>(f: impl FnOnce(&Webview<Wry>) -> Result<R, String>) -> Result<R, String> {
    let guard = lock();
    let webview = guard
        .webview
        .as_ref()
        .ok_or_else(|| "面板浏览器还没打开（先 web.open 或用面板里的浏览器标签）".to_string())?;
    f(webview)
}

// ---------------------------------------------------------------- 脚本片段

/// 在页面上取正文 / 标题 / 链接，返回 `{ok, url, title, text, links}`。
///
/// `selector` 为空时取 `document.body`；`links` 只给前 30 条，够模型挑下一步点哪。
pub fn read_script(selector: Option<&str>) -> String {
    let selector = match selector {
        Some(value) => serde_json::to_string(value).unwrap_or_else(|_| "null".to_string()),
        None => "null".to_string(),
    };
    format!(
        r#"(() => {{
  try {{
    const sel = {selector};
    const root = sel ? document.querySelector(sel) : document.body;
    if (!root) return {{ ok: false, error: '没有匹配的元素：' + sel }};
    const text = (root.innerText || root.textContent || '')
      .replace(/\r/g, '')
      .replace(/\n{{3,}}/g, '\n\n')
      .trim();
    const links = Array.from(root.querySelectorAll('a[href]'))
      .slice(0, 30)
      .map(a => ({{ text: (a.innerText || '').trim().slice(0, 80), href: a.href }}))
      .filter(item => item.text && item.href.startsWith('http'));
    return {{ ok: true, url: location.href, title: document.title, text, links }};
  }} catch (error) {{
    return {{ ok: false, error: String(error) }};
  }}
}})()"#
    )
}

/// 点击：优先用 CSS 选择器；给 `text` 时按可见文字找第一个可点的元素。
pub fn click_script(selector: Option<&str>, text: Option<&str>) -> String {
    let selector = optional_json(selector);
    let text = optional_json(text);
    format!(
        r#"(() => {{
  try {{
    const sel = {selector};
    const label = {text};
    let el = sel ? document.querySelector(sel) : null;
    if (!el && label) {{
      const candidate = Array.from(
        document.querySelectorAll('a, button, [role="button"], input[type="submit"], summary')
      ).find(node => (node.innerText || node.value || '').trim().includes(label));
      el = candidate || null;
    }}
    if (!el) return {{ ok: false, error: sel ? '没有匹配的元素：' + sel : '没有找到文字：' + label }};
    el.scrollIntoView({{ block: 'center', inline: 'center' }});
    el.click();
    return {{ ok: true, url: location.href, title: document.title, clicked: (el.innerText || el.value || '').trim().slice(0, 80) }};
  }} catch (error) {{
    return {{ ok: false, error: String(error) }};
  }}
}})()"#
    )
}

/// 往输入框里写字。`submit` 时补一次回车（React 受控组件要派发 input 才会更新）。
pub fn type_script(selector: &str, text: &str, submit: bool) -> String {
    let selector = serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".to_string());
    let text = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        r#"(() => {{
  try {{
    const el = document.querySelector({selector});
    if (!el) return {{ ok: false, error: '没有匹配的输入框：' + {selector} }};
    el.focus();
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    const setter = Object.getOwnPropertyDescriptor(proto, 'value')?.set;
    if (setter) setter.call(el, {text}); else el.value = {text};
    el.dispatchEvent(new Event('input', {{ bubbles: true }}));
    el.dispatchEvent(new Event('change', {{ bubbles: true }}));
    if ({submit}) {{
      el.dispatchEvent(new KeyboardEvent('keydown', {{ key: 'Enter', keyCode: 13, bubbles: true }}));
      if (el.form && el.form.requestSubmit) el.form.requestSubmit();
    }}
    return {{ ok: true, value: el.value, url: location.href }};
  }} catch (error) {{
    return {{ ok: false, error: String(error) }};
  }}
}})()"#,
        submit = if submit { "true" } else { "false" }
    )
}

/// 元素存在性（给 `wait_for` 轮询）。
pub fn exists_script(selector: &str) -> String {
    let selector = serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".to_string());
    format!("(() => !!document.querySelector({selector}))()")
}

fn optional_json(value: Option<&str>) -> String {
    match value {
        Some(text) => serde_json::to_string(text).unwrap_or_else(|_| "null".to_string()),
        None => "null".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_default_to_https_except_loopback() {
        assert_eq!(normalize_url("example.com").unwrap(), "https://example.com/");
        assert_eq!(
            normalize_url("localhost:1520/dev-preview.html").unwrap(),
            "http://localhost:1520/dev-preview.html"
        );
        assert_eq!(
            normalize_url("127.0.0.1:3000").unwrap(),
            "http://127.0.0.1:3000/"
        );
        assert_eq!(
            normalize_url("https://doc.rust-lang.org/std/").unwrap(),
            "https://doc.rust-lang.org/std/"
        );
        // 已经写了协议就照用，不再叠一层。
        assert_eq!(normalize_url("http://example.com/a").unwrap(), "http://example.com/a");
    }

    #[test]
    fn only_http_is_accepted() {
        assert!(normalize_url("").unwrap_err().contains("不能为空"));
        let file = normalize_url("file:///etc/passwd").unwrap_err();
        assert!(file.contains("只支持 http"), "{file}");
        let js = normalize_url("javascript:alert(1)").unwrap_err();
        assert!(js.contains("只支持 http"), "{js}");
    }

    #[test]
    fn eval_result_accepts_double_encoded_string() {
        // 平台差异：有的把返回值原样给，有的再包一层 JSON 字符串。
        let plain = parse_eval(r#"{"ok":true,"text":"hi"}"#).unwrap();
        assert_eq!(plain["text"], "hi");
        let wrapped = parse_eval(r#""{\"ok\":true,\"text\":\"hi\"}""#).unwrap();
        assert_eq!(wrapped["text"], "hi");
        // 普通字符串就按字符串返回，不要报错。
        assert_eq!(parse_eval(r#""complete""#).unwrap(), "complete");
    }

    #[test]
    fn scripts_escape_user_input() {
        let script = click_script(Some("a\"; alert(1); //"), None);
        // 引号被转义进 JSON 字符串，不会被当成代码拼出去。
        assert!(script.contains(r#""a\"; alert(1); //""#), "{script}");

        let typed = type_script("#q", "a\nb\"c", true);
        assert!(typed.contains(r#""a\nb\"c""#), "{typed}");
        assert!(typed.contains("requestSubmit"), "submit=true 要补一次提交");
    }

    #[test]
    fn read_script_handles_selector_and_plain_body() {
        assert!(read_script(None).contains("document.body"));
        assert!(read_script(Some("main")).contains(r#""main""#));
        assert!(read_script(None).contains("links"));
    }

    #[test]
    fn exists_script_is_boolean_expression() {
        assert_eq!(exists_script("#q"), "(() => !!document.querySelector(\"#q\"))()");
    }
}
