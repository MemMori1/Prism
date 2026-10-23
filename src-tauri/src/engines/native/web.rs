//! 网络工具：`fetch`。
//!
//! 为什么需要它：分析节点要查 CVE、核对库版本、读厂商文档，没有出站能力时
//! 模型只能凭记忆编版本号。而"用 `run_command curl` 凑一个"有三个毛病：
//! 没有超时（一次挂死拖住整轮）、没有上限（一页 3 MB 的 HTML 直接把上下文
//! 撑爆）、拿到的是原始 HTML（脚本和广告占九成）。
//!
//! 所以这个工具只做一件事，并把边界钉死：
//! - **只有 GET + http(s)**：`file://` / `data:` 之类一律拒——URL 是模型写的，
//!   可能被页面内容带偏，能读本地文件就等于绕过了工作目录限制；
//! - **超时** 默认 20s（上限 60s），**字节** 上限 2 MB，**字符** 默认 2 万
//!   （上限 6 万）；截断了就在结尾说清楚，不让模型以为那是全文；
//! - **HTML 抽正文**：脚本 / 样式 / 注释 / `<head>` 全丢，块级标签转换行，
//!   实体解码；JSON / 纯文本原样返回；
//! - **返回头一行是事实**：最终 URL、状态码、类型、大小、耗时——重定向过也看得见。
//!
//! 权限上它属于只读（不改磁盘），但 `network` 标成 `outbound`：设置页会把
//! 「会出站」摆出来，关掉这个开关等于断网。不挂审批闸门是刻意的——查一次
//! CVE 要连点五次「允许」的工具没人会用；真正的闸门是开关 + 安全线工种
//! 的白名单里根本没有它（见 `agents.rs`）。

use std::io::Read;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::provider::describe;
use super::tools::{
    builtin_full, Network, Permission, Risk, ToolContext, ToolDef, ToolMeta, ToolOutcome,
    ToolResult, ToolScope,
};

/// 默认超时。
const DEFAULT_TIMEOUT_MS: u64 = 20_000;
const MIN_TIMEOUT_MS: u64 = 2_000;
const MAX_TIMEOUT_MS: u64 = 60_000;
/// 默认回多少字符给模型。
const DEFAULT_CHARS: usize = 20_000;
const MIN_CHARS: usize = 200;
const MAX_CHARS: usize = 60_000;
/// 最多读多少字节：留得比字符上限富余，够抽完正文又不至于把一页二进制读进内存。
const MAX_BYTES: u64 = 2 * 1024 * 1024;
/// 失败响应（4xx / 5xx）正文里带多少字符：错误页往往有解释，但不值得整页。
const ERROR_BODY_CHARS: usize = 1_200;
/// 自己报门牌：有些站点会按 UA 直接 403。
const USER_AGENT: &str = "Mozilla/5.0 (compatible; Prism/0.3; +https://github.com/MemMori1/Prism)";

pub fn tools() -> Vec<std::sync::Arc<ToolDef>> {
    vec![builtin_full(
        "fetch",
        "抓网页",
        schema,
        Permission::ReadOnly,
        // 不挂审批：查文档要连点好几次「允许」的工具等于没有。
        // 控制点在设置里的工具开关（关掉 = 断网）。
        None,
        None,
        "",
        true,
        ToolMeta {
            tags: &["web"],
            risk: Risk::Read,
            network: Network::Outbound,
            sandbox: None,
            scope: ToolScope::Any,
        },
        run_fetch,
    )]
}

fn schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "fetch",
            "description": "GET 一个 http(s) 地址，返回可读正文：HTML 会抽成纯文本（去脚本/样式），JSON 与纯文本原样返回。默认 20 秒超时、最多 2 万字符（可调）。用于查文档、CVE、变更日志、接口示例。只支持 GET；需要 POST、下载文件、需要登录或靠 JS 渲染的页面，改用 run_command。",
            "parameters": {
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "完整地址，必须带 http:// 或 https://" },
                    "max_chars": {
                        "type": "integer",
                        "description": "最多返回多少字符，默认 20000，上限 60000"
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "超时毫秒，默认 20000，范围 2000–60000"
                    },
                    "raw": {
                        "type": "boolean",
                        "description": "true = HTML 不抽正文，返回原始标记（要自己看标签时用）"
                    }
                },
                "required": ["url"]
            }
        }
    })
}

fn run_fetch(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    ToolResult::plain(fetch(args, ctx.should_stop))
}

// ---------------------------------------------------------------- 抓取

/// 两种客户端：默认走系统代理；本机地址绕开代理。
///
/// 环回地址必须绕：开发时抓 `localhost:1520` 的接口，被系统代理一接管，
/// 拿回来的是代理自己的 502，看起来像"服务挂了"。
fn client_for(url: &reqwest::Url) -> Result<&'static reqwest::blocking::Client, String> {
    if is_loopback(url) {
        static LOCAL: OnceLock<reqwest::blocking::Client> = OnceLock::new();
        if let Some(client) = LOCAL.get() {
            return Ok(client);
        }
        let built = build(true)?;
        return Ok(LOCAL.get_or_init(|| built));
    }
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client);
    }
    let built = build(false)?;
    Ok(CLIENT.get_or_init(|| built))
}

fn build(no_proxy: bool) -> Result<reqwest::blocking::Client, String> {
    let mut builder = reqwest::blocking::Client::builder()
        // 连不上就别耗着；总超时按请求单独设（见 `fetch`）。
        .connect_timeout(Duration::from_secs(10))
        .user_agent(USER_AGENT)
        .redirect(reqwest::redirect::Policy::limited(5));
    if no_proxy {
        builder = builder.no_proxy();
    }
    builder
        .build()
        .map_err(|error| format!("创建 HTTP 客户端失败：{error}"))
}

fn is_loopback(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback() || ip.is_unspecified())
        .unwrap_or(false)
}

fn fetch(args: &Value, should_stop: &dyn Fn() -> bool) -> ToolOutcome {
    let raw_url = args.get("url").and_then(Value::as_str).unwrap_or("").trim();
    let target = match parse_target(raw_url) {
        Ok(url) => url,
        Err(error) => return ToolOutcome::fail(error),
    };
    if should_stop() {
        return ToolOutcome::fail("已停止，没有发出请求");
    }
    let timeout = Duration::from_millis(clamp_u64(
        args.get("timeout_ms").and_then(Value::as_u64),
        DEFAULT_TIMEOUT_MS,
        MIN_TIMEOUT_MS,
        MAX_TIMEOUT_MS,
    ));
    let limit = clamp_u64(
        args.get("max_chars").and_then(Value::as_u64),
        DEFAULT_CHARS as u64,
        MIN_CHARS as u64,
        MAX_CHARS as u64,
    ) as usize;
    let raw = args.get("raw").and_then(Value::as_bool).unwrap_or(false);

    let client = match client_for(&target) {
        Ok(client) => client,
        Err(error) => return ToolOutcome::fail(error),
    };

    let started = Instant::now();
    let response = match client
        .get(target.clone())
        .timeout(timeout)
        .header(reqwest::header::ACCEPT, "text/html,application/json;q=0.9,text/plain;q=0.8,*/*;q=0.5")
        .send()
    {
        Ok(response) => response,
        Err(error) => {
            return ToolOutcome::fail(format!(
                "GET {raw_url} 失败：{}\n{}",
                describe(&error),
                network_hint(&error)
            ))
        }
    };
    let elapsed = started.elapsed();
    let status = response.status();
    let final_url = response.url().to_string();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();

    // 先声明读多少，再读：`take` 之后拿不到响应头了。
    let mut buffer = Vec::new();
    let mut reader = response.take(MAX_BYTES + 1);
    if let Err(error) = reader.read_to_end(&mut buffer) {
        return ToolOutcome::fail(format!("读取响应正文失败：{error}"));
    }
    let cut_bytes = buffer.len() as u64 > MAX_BYTES;
    if cut_bytes {
        buffer.truncate(MAX_BYTES as usize);
    }
    let charset = charset_of(&content_type);
    let text = String::from_utf8_lossy(&buffer).to_string();

    let mut head = format!(
        "GET {final_url} → {} · {} · {} · {}ms",
        status.as_u16(),
        type_label(&content_type),
        human_size(buffer.len() as u64),
        elapsed.as_millis()
    );
    if final_url != raw_url {
        head.push_str(&format!("\n（由 {raw_url} 重定向而来）"));
    }
    if cut_bytes {
        head.push_str(&format!(
            "\n（正文超过 {} 上限，只读了前面这些；要完整的请用 run_command 自己下载）",
            human_size(MAX_BYTES)
        ));
    }

    let failed = !status.is_success();
    let body = if failed {
        let snippet = truncate_chars(&text, ERROR_BODY_CHARS);
        format!(
            "{head}\n\n没拿到内容（HTTP {}）。响应正文（供判断原因）：\n{}",
            status.as_u16(),
            if snippet.trim().is_empty() {
                "（空）".to_string()
            } else {
                snippet
            }
        )
    } else if is_binary(&content_type) {
        format!("{head}\n\n（二进制内容，没有当文本返回。要处理它请用 run_command 下载到工作目录）")
    } else if is_html(&content_type, &text) && !raw {
        let title = html_title(&text);
        let body = visible_text(&text);
        let mut out = String::from(head);
        if let Some(title) = title {
            out.push_str(&format!("\n标题：{title}"));
        }
        if let Some(note) = charset_note(charset.as_deref(), &text) {
            out.push_str(&format!("\n{note}"));
        }
        out.push_str("\n\n");
        out.push_str(&body);
        out
    } else {
        // 纯文本（JSON / markdown / 纯文本）也要给字符集提示：老站点常发
        // 声明成 gbk 的 text/plain，按 UTF-8 读出来是乱码，模型会当成内容读。
        let mut out = String::from(head);
        if let Some(note) = charset_note(charset.as_deref(), &text) {
            out.push_str(&format!("\n{note}"));
        }
        out.push_str("\n\n");
        out.push_str(&text);
        out
    };

    let out = finish(body, limit);
    // 4xx / 5xx 算失败（工具卡标红），但正文照给：错误页里常常写着真正的原因。
    if failed {
        ToolOutcome::fail(out)
    } else {
        ToolOutcome::ok(out)
    }
}

/// 截断到上限，并在结尾说清楚"这不是全文"——模型最容易把截断当结论。
fn finish(body: String, limit: usize) -> String {
    let total = body.chars().count();
    if total <= limit {
        return body;
    }
    format!(
        "{}\n…（已截断：只给前 {limit} 字符，正文共 {total} 字符；要看更多把 max_chars 调大，上限 {MAX_CHARS}）",
        truncate_chars(&body, limit)
    )
}

/// 只认 http(s)。模型写的 URL 可能被页面内容带偏，`file://` 能读本地文件，
/// 等于绕过工作目录限制，必须在发出请求前挡住。
fn parse_target(raw: &str) -> Result<reqwest::Url, String> {
    if raw.is_empty() {
        return Err("缺少 url".to_string());
    }
    let url = reqwest::Url::parse(raw).map_err(|error| format!("URL 不合法（{raw}）：{error}"))?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        other => Err(format!(
            "只支持 http(s)，不支持 {other}://——本地文件用 read_file，其它协议用 run_command"
        )),
    }
}

/// 网络错误里最常见的三种，直接说下一步怎么做，别让模型自己猜。
fn network_hint(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "提示：超时了。地址可能不通，或页面本身很慢；可以调大 timeout_ms，或换镜像/离线查。"
    } else if error.is_connect() {
        "提示：连不上。检查 DNS / 代理 / 是否需要科学上网；内网地址可能需要给代理加例外。"
    } else if error.is_redirect() {
        "提示：重定向次数超过 5 次（可能在循环跳转），直接用最终地址再试一次。"
    } else {
        "提示：这是出站请求，失败通常和本机网络或代理有关；也可以用 run_command 跑 curl 对比。"
    }
}

// ---------------------------------------------------------------- 正文抽取

/// 版面标签 → 换行，单元格 → 分隔符，其余标签直接丢。
fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "main"
            | "aside"
            | "nav"
            | "ul"
            | "ol"
            | "li"
            | "dl"
            | "dt"
            | "dd"
            | "table"
            | "tr"
            | "blockquote"
            | "pre"
            | "hr"
            | "br"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
    )
}

/// 整段丢掉的元素：内容不是给人读的正文，留着只会污染上下文。
fn is_opaque(name: &str) -> bool {
    matches!(
        name,
        "script" | "style" | "noscript" | "template" | "svg" | "canvas" | "iframe" | "head"
    )
}

/// HTML → 可读正文。
///
/// 不引 HTML 解析库是权衡：这里要的是"能读"，不是"结构完整"。手写扫描
/// 处理得了真实页面里 99% 的情况，省掉一棵 DOM 树和它的依赖。
fn visible_text(html: &str) -> String {
    // 只用于找标签 / 找闭合，逐字节等长（ASCII 小写化不改变长度）。
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len() / 2);
    let mut i = 0usize;

    while i < html.len() {
        let rest = &lower[i..];
        if rest.starts_with("<!--") {
            match rest.find("-->") {
                Some(end) => i += end + 3,
                None => break,
            }
            continue;
        }
        if rest.starts_with('<') {
            let Some(gt) = html[i..].find('>') else { break };
            let tag_end = i + gt + 1;
            let inner = &html[i + 1..i + gt];
            let closing = inner.starts_with('/');
            let name = tag_name(inner);
            if !closing && is_opaque(&name) {
                // 跳到闭合标签之后；没有闭合（被截断）就到此为止。
                let needle = format!("</{name}");
                let after = match lower[tag_end..].find(&needle) {
                    Some(pos) => tag_end + pos,
                    None => break,
                };
                match html[after..].find('>') {
                    Some(gt2) => i = after + gt2 + 1,
                    None => break,
                }
                continue;
            }
            if is_block(&name) {
                out.push('\n');
                if name == "li" && !closing {
                    out.push_str("- ");
                }
            } else if (name == "td" || name == "th") && !closing {
                out.push_str(" | ");
            }
            i = tag_end;
            continue;
        }
        let next = rest.find('<').map(|pos| i + pos).unwrap_or(html.len());
        out.push_str(&html[i..next]);
        i = next;
    }

    normalize(&decode_entities(&out))
}

fn tag_name(inner: &str) -> String {
    let inner = inner.trim_start_matches('/').trim_start();
    let end = inner
        .find(|ch: char| !(ch.is_ascii_alphanumeric()))
        .unwrap_or(inner.len());
    inner[..end].to_ascii_lowercase()
}

/// 压掉多余空白：行内连续空格合一，三行以上空行减到一行，行尾不留空格。
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank = false;
    for raw in text.split('\n') {
        let line = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            if out.is_empty() || blank {
                continue;
            }
            blank = true;
            out.push('\n');
            continue;
        }
        blank = false;
        out.push_str(line.trim_end_matches(" |"));
        out.push('\n');
    }
    out.trim_end().to_string()
}

fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        // 实体最长也就 `&#x1F600;` 这种 10 来个字符；找不到分号就当普通 & 处理。
        let semi = tail[1..].find(';').filter(|&end| end <= 12);
        let Some(end) = semi else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let name = &tail[1..=end];
        match entity_char(name) {
            Some(ch) => {
                out.push(ch);
                rest = &tail[end + 2..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity_char(name: &str) -> Option<char> {
    if let Some(hex) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
        return u32::from_str_radix(hex, 16).ok().and_then(char::from_u32);
    }
    if let Some(dec) = name.strip_prefix('#') {
        return dec.parse::<u32>().ok().and_then(char::from_u32);
    }
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        "ensp" | "emsp" | "thinsp" => Some(' '),
        "mdash" => Some('—'),
        "ndash" => Some('–'),
        "hellip" => Some('…'),
        "middot" => Some('·'),
        "bull" => Some('•'),
        "copy" => Some('©'),
        "reg" => Some('®'),
        "trade" => Some('™'),
        "times" => Some('×'),
        "divide" => Some('÷'),
        "deg" => Some('°'),
        "plusmn" => Some('±'),
        "laquo" => Some('«'),
        "raquo" => Some('»'),
        "ldquo" => Some('“'),
        "rdquo" => Some('”'),
        "lsquo" => Some('‘'),
        "rsquo" => Some('’'),
        _ => None,
    }
}

fn html_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let open_end = html[start..].find('>').map(|pos| start + pos + 1)?;
    let close = lower[open_end..].find("</title>").map(|pos| open_end + pos)?;
    let title = normalize(&decode_entities(&html[open_end..close]));
    if title.is_empty() {
        None
    } else {
        Some(title.chars().take(200).collect())
    }
}

// ---------------------------------------------------------------- 小工具

fn is_html(content_type: &str, body: &str) -> bool {
    let kind = content_type.to_ascii_lowercase();
    if kind.contains("html") || kind.contains("xhtml") {
        return true;
    }
    if !kind.is_empty() && !kind.contains("text/") && !kind.contains("xml") {
        return false;
    }
    // 有些静态站 / 老网关给的是 text/plain 或干脆没有 Content-Type。
    let head = body.trim_start();
    let head = head.chars().take(200).collect::<String>().to_ascii_lowercase();
    head.starts_with("<!doctype html") || head.starts_with("<html")
}

fn is_binary(content_type: &str) -> bool {
    let kind = content_type.to_ascii_lowercase();
    if kind.is_empty() {
        return false;
    }
    kind.starts_with("image/")
        || kind.starts_with("audio/")
        || kind.starts_with("video/")
        || kind.starts_with("font/")
        || kind.contains("octet-stream")
        || kind.contains("application/pdf")
        || kind.contains("application/zip")
        || kind.contains("application/gzip")
        || kind.contains("application/wasm")
}

fn type_label(content_type: &str) -> String {
    let kind = content_type.split(';').next().unwrap_or("").trim();
    if kind.is_empty() {
        "未知类型".to_string()
    } else {
        kind.to_string()
    }
}

fn charset_of(content_type: &str) -> Option<String> {
    content_type
        .split(';')
        .skip(1)
        .filter_map(|part| {
            let part = part.trim();
            let (key, value) = part.split_once('=')?;
            key.trim()
                .eq_ignore_ascii_case("charset")
                .then(|| value.trim().trim_matches('"').to_ascii_lowercase())
        })
        .next()
}

/// 声明了非 UTF-8 字符集却按 UTF-8 解：告诉模型这段可能是乱码，别硬读。
fn charset_note(charset: Option<&str>, body: &str) -> Option<String> {
    let charset = charset?;
    if charset.is_empty() || charset.starts_with("utf-8") || charset == "utf8" || charset == "us-ascii"
    {
        return None;
    }
    let garbled = body.contains('\u{FFFD}');
    Some(if garbled {
        format!("（页面声明 charset={charset}，按 UTF-8 解码有乱码：正文可能读不准，结论请打折）")
    } else {
        format!("（页面声明 charset={charset}，这里按 UTF-8 解码）")
    })
}

fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let value = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if value < KB * KB {
        format!("{:.1} KB", value / KB)
    } else {
        format!("{:.1} MB", value / (KB * KB))
    }
}

fn clamp_u64(value: Option<u64>, fallback: u64, min: u64, max: u64) -> u64 {
    value.unwrap_or(fallback).clamp(min, max)
}

fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    text.chars().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    /// 起一个只回一次的本地服务：探针不该依赖外网，也不该赖在别人的站上。
    fn serve_once(status: &str, content_type: &str, body: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定本地端口");
        let addr = listener.local_addr().expect("取本地地址");
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut head = [0u8; 2048];
                let _ = stream.read(&mut head);
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        format!("http://{addr}/")
    }

    #[test]
    fn only_http_is_allowed() {
        assert!(parse_target("https://doc.rust-lang.org/").is_ok());
        assert!(parse_target("http://127.0.0.1:1520/").is_ok());
        let file = parse_target("file:///etc/passwd").unwrap_err();
        assert!(file.contains("只支持 http"), "{file}");
        let data = parse_target("data:text/html,<h1>x</h1>").unwrap_err();
        assert!(data.contains("只支持 http"), "{data}");
        assert!(parse_target("").unwrap_err().contains("缺少 url"));
    }

    /// 端到端：真的发一次 GET，确认头一行事实、正文抽取、截断都说对了。
    #[test]
    fn fetch_reports_facts_and_extracts_text() {
        let url = serve_once(
            "200 OK",
            "text/html; charset=utf-8",
            "<html><head><title>示例页</title><script>bad()</script></head>\
             <body><h1>开头</h1><p>正文 &amp; 结论</p></body></html>",
        );
        let outcome = fetch(&json!({ "url": url }), &|| false);
        assert!(outcome.error.is_none(), "{}", outcome.text);
        let text = outcome.text;
        // 头一行要能核对：URL、状态码、类型、大小。
        assert!(text.starts_with("GET http://127.0.0.1:"), "{text}");
        assert!(text.contains("→ 200"), "{text}");
        assert!(text.contains("text/html"), "{text}");
        assert!(text.contains("标题：示例页"), "{text}");
        // 正文抽出来了，脚本丢了。
        assert!(text.contains("正文 & 结论"), "{text}");
        assert!(!text.contains("bad()"), "{text}");
        assert!(!text.contains("<h1>"), "{text}");
    }

    #[test]
    fn fetch_keeps_raw_html_when_asked() {
        let url = serve_once("200 OK", "text/html", "<p>hello</p>");
        let outcome = fetch(&json!({ "url": url, "raw": true }), &|| false);
        assert!(outcome.text.contains("<p>hello</p>"), "{}", outcome.text);
    }

    #[test]
    fn fetch_truncates_and_says_so() {
        let body = format!("<p>{}</p>", "很长的正文".repeat(200));
        let url = serve_once("200 OK", "text/html", &body);
        let outcome = fetch(&json!({ "url": url, "max_chars": 200 }), &|| false);
        assert!(outcome.text.contains("已截断"), "{}", outcome.text);
        assert!(outcome.text.chars().count() < 600, "截断没生效");
    }

    /// 4xx 不该只丢一句「失败了」：错误页里往往写着原因，要带出来。
    #[test]
    fn fetch_failure_carries_status_and_body() {
        let url = serve_once("404 Not Found", "text/plain", "no such page: check the path");
        let outcome = fetch(&json!({ "url": url }), &|| false);
        assert!(outcome.error.is_some());
        assert!(outcome.text.contains("404"), "{}", outcome.text);
        assert!(outcome.text.contains("no such page"), "{}", outcome.text);
    }

    #[test]
    fn fetch_refuses_local_files_without_touching_network() {
        let outcome = fetch(&json!({ "url": "file:///etc/passwd" }), &|| false);
        assert!(outcome.error.unwrap().contains("只支持 http"));
        assert!(outcome.text.contains("read_file"), "{}", outcome.text);
    }

    /// 注册表这一层也得通：模型回传的是工具名 `fetch`，得能查到并真的执行。
    #[test]
    fn dispatches_through_the_registry() {
        let url = serve_once("200 OK", "text/plain; charset=utf-8", "hello from tool");
        let outcome = super::super::tools::execute_with_stop(
            &std::env::temp_dir().to_string_lossy(),
            "fetch",
            &json!({ "url": url }).to_string(),
            &|| false,
        );
        assert!(outcome.error.is_none(), "{}", outcome.text);
        assert!(outcome.text.contains("hello from tool"), "{}", outcome.text);
    }

    #[test]
    fn loopback_hosts_are_detected() {
        let local = |text: &str| is_loopback(&reqwest::Url::parse(text).unwrap());
        assert!(local("http://127.0.0.1:1520/"));
        assert!(local("http://localhost:1520/"));
        assert!(local("http://[::1]:1520/"));
        assert!(local("http://dev.localhost/"));
        assert!(!local("https://example.com/"));
    }

    #[test]
    fn strips_scripts_styles_and_comments() {
        let html = r#"<html><head><title>标题 A</title><style>.a{color:red}</style></head>
            <body><!-- 注释不要 -->
            <script>alert('不要')</script>
            <h1>开头</h1><p>第一段</p><p>第二段</p>
            </body></html>"#;
        let text = visible_text(html);
        assert!(text.contains("开头"), "{text}");
        assert!(text.contains("第一段"), "{text}");
        assert!(!text.contains("alert"), "{text}");
        assert!(!text.contains("color:red"), "{text}");
        assert!(!text.contains("注释"), "{text}");
        // 块级标签要转换行：两段不能粘成一行（中间可以有空行）。
        assert!(!text.contains("第一段第二段"), "{text}");
        let first = text.lines().position(|line| line == "第一段").expect("缺第一段");
        let second = text.lines().position(|line| line == "第二段").expect("缺第二段");
        assert!(first < second, "{text}");
    }

    #[test]
    fn keeps_list_structure_and_decodes_entities() {
        let html = "<ul><li>甲 &amp; 乙</li><li>丙 &#0x4E01; &mdash; 丁</li></ul>";
        let text = visible_text(html);
        assert!(text.contains("- 甲 & 乙"), "{text}");
        assert!(text.contains("丁"), "{text}");
        assert!(text.contains('—'), "{text}");
    }

    #[test]
    fn numeric_entities_and_stray_ampersand() {
        assert_eq!(decode_entities("a &lt; b"), "a < b");
        assert_eq!(decode_entities("&#65;&#x42;"), "AB");
        // 不是实体的 & 要原样留着，不能吃掉后面的字。
        assert_eq!(decode_entities("Q&A 与 a & b"), "Q&A 与 a & b");
    }

    #[test]
    fn table_cells_become_separators() {
        let html = "<table><tr><td>版本</td><td>1.2.3</td></tr></table>";
        let text = visible_text(html);
        assert!(text.contains("版本 | 1.2.3"), "{text}");
    }

    #[test]
    fn title_is_extracted_and_entities_decoded() {
        let html = "<html><head><title>CVE-2024-1 &amp; 说明</title></head><body>x</body></html>";
        assert_eq!(html_title(html).as_deref(), Some("CVE-2024-1 & 说明"));
        assert_eq!(html_title("<html><body>没有标题</body></html>"), None);
    }

    #[test]
    fn html_detection_falls_back_to_sniffing() {
        assert!(is_html("text/html; charset=utf-8", ""));
        // 没有 Content-Type 的老网关：靠开头几个字符认。
        assert!(is_html("", "<!DOCTYPE html><html>..."));
        assert!(is_html("text/plain", "<html lang=\"zh\">"));
        assert!(!is_html("text/plain", "just a note"));
        assert!(!is_html("text/markdown", "# 标题"));
    }

    #[test]
    fn binary_types_are_not_decoded() {
        assert!(is_binary("image/png"));
        assert!(is_binary("application/octet-stream"));
        assert!(is_binary("application/pdf"));
        assert!(!is_binary("text/html"));
        assert!(!is_binary(""));
    }

    #[test]
    fn charset_mismatch_is_reported() {
        assert!(charset_note(Some("utf-8"), "正常").is_none());
        assert!(charset_note(None, "x").is_none());
        let note = charset_note(Some("gbk"), "正常").unwrap();
        assert!(note.contains("gbk"), "{note}");
        // 有替换字符才说「乱码」，否则只说「按 UTF-8 解码」。
        assert!(charset_note(Some("gbk"), "正常").unwrap().contains("按 UTF-8"));
        assert!(charset_note(Some("gbk"), "坏\u{FFFD}")
            .unwrap()
            .contains("乱码"));
    }

    #[test]
    fn charset_is_parsed_from_content_type() {
        assert_eq!(
            charset_of("text/html; charset=UTF-8").as_deref(),
            Some("utf-8")
        );
        assert_eq!(
            charset_of("text/html;charset=\"gbk\"; x=1").as_deref(),
            Some("gbk")
        );
        assert_eq!(charset_of("text/html"), None);
    }

    #[test]
    fn limits_are_clamped() {
        assert_eq!(clamp_u64(None, 20_000, 200, 60_000), 20_000);
        // 要 1 亿字符也别真给：上下文撑爆是确定的，截断是可控的。
        assert_eq!(clamp_u64(Some(100_000_000), 20_000, 200, 60_000), 60_000);
        assert_eq!(clamp_u64(Some(1), 20_000, 200, 60_000), 200);
        assert_eq!(truncate_chars("abcdef", 3), "abc");
        assert_eq!(truncate_chars("abc", 10), "abc");
    }

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }
}
