//! OpenAI 兼容的流式客户端。
//!
//! 只做一件事：把 `POST {base}/chat/completions` 的 SSE 增量拼成一条完整回复。
//! 拼装逻辑（`StreamAcc`）是纯函数，可以脱离网络单测——这是最容易出错的部分
//! （工具入参是分片到达的，文本和推理也各有各的字段名）。

use super::config::{ModelEntry, ProviderConfig};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub input: i64,
    pub output: i64,
}

#[derive(Debug, Clone, Default)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// 原始 JSON 字符串（分片拼好的结果）。
    pub arguments: String,
}

#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Option<Usage>,
}

/// 本次增量：调用方用它往界面上推 delta。
#[derive(Debug, Default, PartialEq)]
pub struct Delta {
    pub text: String,
    pub reasoning: String,
}

/// 流式增量累加器。
#[derive(Debug, Default)]
pub struct StreamAcc {
    text: String,
    reasoning: String,
    usage: Option<Usage>,
    /// (id, name, arguments) —— 按 index 对齐，缺位补空。
    calls: Vec<(String, String, String)>,
}

impl StreamAcc {
    /// 喂一个 chunk，返回本次新增的文本 / 推理（可能都为空）。
    pub fn apply(&mut self, chunk: &Value) -> Delta {
        let mut delta = Delta::default();

        if let Some(usage) = chunk.get("usage").filter(|value| value.is_object()) {
            let input = usage
                .get("prompt_tokens")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let output = usage
                .get("completion_tokens")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            if input > 0 || output > 0 {
                self.usage = Some(Usage { input, output });
            }
        }

        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|list| list.first())
        else {
            return delta;
        };

        // 流式走 delta；少数服务端不认 stream 时走 message，一并兼容。
        let payload = choice.get("delta").or_else(|| choice.get("message"));
        let Some(payload) = payload else {
            return delta;
        };

        if let Some(text) = content_text(payload.get("content")) {
            self.text.push_str(&text);
            delta.text = text;
        }
        let reasoning = payload
            .get("reasoning_content")
            .and_then(Value::as_str)
            .or_else(|| payload.get("reasoning").and_then(Value::as_str));
        if let Some(reasoning) = reasoning.filter(|value| !value.is_empty()) {
            self.reasoning.push_str(reasoning);
            delta.reasoning = reasoning.to_string();
        }

        if let Some(calls) = payload.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let index = call
                    .get("index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                while self.calls.len() <= index {
                    self.calls.push((String::new(), String::new(), String::new()));
                }
                let slot = &mut self.calls[index];
                if let Some(id) = call.get("id").and_then(Value::as_str) {
                    if !id.is_empty() {
                        slot.0 = id.to_string();
                    }
                }
                if let Some(function) = call.get("function") {
                    if let Some(name) = function.get("name").and_then(Value::as_str) {
                        if !name.is_empty() {
                            slot.1 = name.to_string();
                        }
                    }
                    if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                        slot.2.push_str(arguments);
                    }
                }
            }
        }

        delta
    }

    pub fn into_outcome(self) -> Outcome {
        let tool_calls = self
            .calls
            .into_iter()
            .filter(|(_, name, _)| !name.is_empty())
            .enumerate()
            .map(|(index, (id, name, arguments))| ToolCall {
                id: if id.is_empty() {
                    format!("call_{index}")
                } else {
                    id
                },
                name,
                arguments,
            })
            .collect();
        Outcome {
            text: self.text,
            reasoning: self.reasoning,
            tool_calls,
            usage: self.usage,
        }
    }
}

fn content_text(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if let Some(text) = value.as_str() {
        return Some(text.to_string());
    }
    let parts = value.as_array()?;
    let mut out = String::new();
    for part in parts {
        if let Some(text) = part.get("text").and_then(Value::as_str) {
            out.push_str(text);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// 本机地址不该被代理劫走：本地 Ollama / LM Studio 一旦走代理就是「连不上」。
const DIRECT_HOSTS: &str = "localhost,127.0.0.1,::1,0.0.0.0";

/// 环境里配了代理就显式用（访问境外模型服务基本都要），没配则交给
/// reqwest 的系统代理探测（macOS「网络 → 代理」）。socks 走不了——没开那个特性。
fn env_proxy() -> Option<String> {
    const KEYS: [&str; 6] = [
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "http_proxy",
        "HTTP_PROXY",
    ];
    KEYS.iter()
        .filter_map(|key| std::env::var(key).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        // socks 需要额外的 feature，跳过它而不是让所有请求都失败。
        .find(|value| !value.to_ascii_lowercase().starts_with("socks"))
        // 有些工具只给 host:port，补上默认协议。
        .map(|value| {
            if value.contains("://") {
                value
            } else {
                format!("http://{value}")
            }
        })
}

fn client() -> Result<reqwest::blocking::Client, String> {
    // 连不上就别耗着；普通请求（探活、拉模型清单）总超时 600s。
    // 流式那一路（chat）会自己覆盖总超时并加空闲超时：长回复不该被
    // 10 分钟从中间砍断，卡死的连接也不该被陪到 10 分钟。
    let mut builder = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(600));
    if let Some(url) = env_proxy() {
        let mut direct = DIRECT_HOSTS.to_string();
        if let Ok(extra) = std::env::var("NO_PROXY").or_else(|_| std::env::var("no_proxy")) {
            let extra = extra.trim();
            if !extra.is_empty() {
                direct.push(',');
                direct.push_str(extra);
            }
        }
        let proxy = reqwest::Proxy::all(&url)
            .map_err(|error| format!("代理地址不可用（{url}）：{error}"))?
            .no_proxy(reqwest::NoProxy::from_string(&direct));
        // 显式配了代理就关掉自动探测，免得叠两层。
        builder = builder.no_proxy().proxy(proxy);
    }
    builder
        .build()
        .map_err(|error| format!("创建 HTTP 客户端失败：{}", describe(&error)))
}

/// reqwest 的 `Display` 只有第一层（`error sending request for url (...)`），
/// 真正的原因（TLS 握手、DNS、代理拒绝）藏在 source 链里，拼出来才排查得动。
///
/// `pub(super)`：`web::fetch` 出站失败时也拼这条链——两处各写一份的话，
/// 迟早只有一处被修。
pub(super) fn describe(error: &(dyn std::error::Error + 'static)) -> String {
    let mut parts = vec![error.to_string()];
    let mut cursor = error.source();
    while let Some(cause) = cursor {
        let text = cause.to_string();
        if !text.is_empty() && !parts.contains(&text) {
            parts.push(text);
        }
        cursor = cause.source();
    }
    parts.join(" → ")
}

/// 被用户叫停时给用户看的说法（界面文案）。
const CANCELLED: &str = "已停止本次回复";

/// 一轮对话的失败：用户叫停不算「出错」，上层要能分开处理。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatError {
    /// 用户按了停止：正常收工，界面上不标红。
    Cancelled,
    /// 真出错：网络、HTTP 状态、解析、空闲超时……
    Failed(String),
}

impl std::fmt::Display for ChatError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChatError::Cancelled => formatter.write_str(CANCELLED),
            ChatError::Failed(error) => formatter.write_str(error),
        }
    }
}

/// 内部那些 format! 出来的说明可以直接 `?` 上来。
impl From<String> for ChatError {
    fn from(error: String) -> Self {
        ChatError::Failed(error)
    }
}

fn failed(message: impl Into<String>) -> ChatError {
    ChatError::Failed(message.into())
}

fn check_stop(should_stop: &dyn Fn() -> bool) -> Result<(), ChatError> {
    if should_stop() {
        Err(ChatError::Cancelled)
    } else {
        Ok(())
    }
}

/// 流式收包的轮询粒度：停止按钮的响应延迟就是它（不用等下一行）。
const STREAM_POLL: Duration = Duration::from_millis(200);
/// 空闲多久算「服务端卡住了」。给思考型模型留足首字延迟，但绝不再陪跑总超时。
/// 测试里调短：不然这个用例得跑五分钟。
#[cfg(not(test))]
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
#[cfg(test)]
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_millis(400);

/// 实际生效的空闲上限。
///
/// 测试里默认 400ms 是为了让「服务端卡住」那个用例不用等五分钟；但同一份
/// 二进制里跑真机用例（`--ignored`）时，400ms 连思考型模型的首字都等不到。
/// 所以给一个环境变量口子：`PRISM_STREAM_IDLE_MS=60000` 放宽，生产不受影响。
fn idle_timeout() -> Duration {
    #[cfg(test)]
    if let Ok(raw) = std::env::var("PRISM_STREAM_IDLE_MS") {
        if let Ok(ms) = raw.trim().parse::<u64>() {
            return Duration::from_millis(ms.clamp(100, 3_600_000));
        }
    }
    STREAM_IDLE_TIMEOUT
}
/// 单轮流式请求的兜底上限（正常写代码远用不到，防的是永远不结束的流）。
const STREAM_TOTAL_TIMEOUT: Duration = Duration::from_secs(3600);

/// 流式跑一轮。`on_text` / `on_reasoning` 会在收到增量时被调用；
/// `should_stop` 在发请求前与流中每 200ms 都会问一次，返回 true 就收工。
pub fn chat(
    provider: &ProviderConfig,
    model: &str,
    messages: &[Value],
    tools: &[Value],
    on_text: &mut dyn FnMut(&str),
    on_reasoning: &mut dyn FnMut(&str),
    should_stop: &dyn Fn() -> bool,
) -> Result<Outcome, ChatError> {
    if !provider.usable() {
        return Err(failed("这个模型服务还没填地址"));
    }
    check_stop(should_stop)?;
    let mut body = json!({
        "model": model,
        "stream": true,
        "messages": messages,
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools.to_vec());
        body["tool_choice"] = json!("auto");
    }

    // 流式请求单独给总超时：客户端那个 600s 是给普通请求的，
    // 长时间吐字的一轮不该被它从中间砍断。
    let mut request = client()?
        .post(provider.endpoint())
        .header("Content-Type", "application/json")
        .timeout(STREAM_TOTAL_TIMEOUT)
        .json(&body);
    if !provider.api_key.trim().is_empty() {
        request = request.bearer_auth(provider.api_key.trim());
    }
    let response = request
        .send()
        .map_err(|error| failed(format!("请求失败：{}", describe(&error))))?;
    check_stop(should_stop)?;

    let status = response.status();
    if !status.is_success() {
        let detail = response.text().unwrap_or_default();
        return Err(failed(format!(
            "模型返回 {status}：{}",
            truncate(detail.trim(), 400)
        )));
    }

    let is_json = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.contains("application/json"))
        .unwrap_or(false);

    let mut acc = StreamAcc::default();
    if is_json {
        let value: Value = response
            .json()
            .map_err(|error| failed(format!("解析响应失败：{error}")))?;
        let delta = acc.apply(&value);
        if !delta.text.is_empty() {
            on_text(&delta.text);
        }
        if !delta.reasoning.is_empty() {
            on_reasoning(&delta.reasoning);
        }
        return Ok(acc.into_outcome());
    }

    // 读流丢给一个线程，主循环按 200ms 的粒度收：既不用陪服务端卡到总超时，
    // 停止按钮也不必等下一行才生效。主循环提前返回时这个线程会在下一次
    // 投递失败后收工（最坏也就是等请求总超时把它从 read 里拽出来）。
    let reader = BufReader::new(response);
    // 有界通道当作背压：本地服务端吐得比解析快时，读线程会自己等一下。
    let (sender, receiver) = std::sync::mpsc::sync_channel::<Result<String, String>>(1024);
    let worker = std::thread::spawn(move || {
        for line in reader.lines() {
            let item = match line {
                Ok(line) => Ok(line),
                Err(error) => Err(format!("读取流失败：{error}")),
            };
            let broken = item.is_err();
            if sender.send(item).is_err() || broken {
                break;
            }
        }
    });

    let mut last = Instant::now();
    let idle = idle_timeout();
    loop {
        check_stop(should_stop)?;
        match receiver.recv_timeout(STREAM_POLL) {
            Ok(Ok(line)) => {
                last = Instant::now();
                let Some(payload) = line.strip_prefix("data:") else {
                    continue;
                };
                let payload = payload.trim();
                if payload.is_empty() || payload == "[DONE]" {
                    continue;
                }
                let Ok(chunk) = serde_json::from_str::<Value>(payload) else {
                    continue;
                };
                if let Some(error) = chunk.get("error") {
                    return Err(failed(format!("模型返回错误：{error}")));
                }
                let delta = acc.apply(&chunk);
                if !delta.text.is_empty() {
                    on_text(&delta.text);
                }
                if !delta.reasoning.is_empty() {
                    on_reasoning(&delta.reasoning);
                }
            }
            Ok(Err(error)) => {
                let _ = worker.join();
                return Err(failed(error));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if last.elapsed() > idle {
                    return Err(failed(format!(
                        "模型 {} 秒没有任何响应，已放弃这一轮（网络或服务端卡住了）",
                        idle.as_secs()
                    )));
                }
            }
            // 发送端收工＝流正常读完。
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = worker.join();
    Ok(acc.into_outcome())
}

/// 连通性探测：发一条最小请求，看密钥 / 地址 / 模型是否对得上。
pub fn probe(provider: &ProviderConfig, model: &str) -> Result<String, String> {
    if !provider.usable() {
        return Err("先把地址填完".to_string());
    }
    if model.trim().is_empty() {
        return Err("这个服务还没选模型".to_string());
    }
    let body = json!({
        "model": model,
        "max_tokens": 1,
        "messages": [{ "role": "user", "content": "ping" }],
    });
    let mut request = client()?
        .post(provider.endpoint())
        .header("Content-Type", "application/json")
        .json(&body);
    if !provider.api_key.trim().is_empty() {
        request = request.bearer_auth(provider.api_key.trim());
    }
    let response = request
        .send()
        .map_err(|error| format!("连不上：{}", describe(&error)))?;
    let status = response.status();
    if !status.is_success() {
        let detail = response.text().unwrap_or_default();
        return Err(format!("模型返回 {status}：{}", truncate(detail.trim(), 300)));
    }
    Ok(format!("连通正常（{model}）"))
}

/// 拉取服务端的模型清单（`GET {base}/models`，OpenAI 兼容的约定）。
///
/// 密钥为空时不带 Authorization——本地 Ollama / vLLM 常常不校验。
pub fn fetch_models(provider: &ProviderConfig) -> Result<Vec<ModelEntry>, String> {
    if provider.base_url.trim().is_empty() {
        return Err("先把 Base URL 填上".to_string());
    }
    let url = provider.models_endpoint();
    let mut request = client()?.get(&url).timeout(Duration::from_secs(30));
    if !provider.api_key.trim().is_empty() {
        request = request.bearer_auth(provider.api_key.trim());
    }
    let response = request
        .send()
        .map_err(|error| format!("连不上 {url}：{}", describe(&error)))?;
    let status = response.status();
    let text = response.text().unwrap_or_default();
    if !status.is_success() {
        return Err(format!("拉取模型失败 {status}：{}", truncate(text.trim(), 300)));
    }
    let value: Value =
        serde_json::from_str(&text).map_err(|error| format!("返回的不是 JSON：{error}"))?;
    let models = parse_models(&value);
    if models.is_empty() {
        return Err("这个端点没有返回任何模型".to_string());
    }
    Ok(models)
}

/// 从 `/models` 的响应里取模型列表。
///
/// 认三种常见形状：`{data:[{id}]}`（OpenAI / DeepSeek）、`{models:[{name}]}`
/// （Ollama）、以及直接一个数组。顺便去重、排序。
pub fn parse_models(value: &Value) -> Vec<ModelEntry> {
    let list = value
        .get("data")
        .or_else(|| value.get("models"))
        .and_then(Value::as_array)
        .or_else(|| value.as_array());
    let Some(list) = list else {
        return Vec::new();
    };
    let mut out: Vec<ModelEntry> = Vec::new();
    for item in list {
        let id = item
            .get("id")
            .or_else(|| item.get("name"))
            .or_else(|| item.get("model"))
            .and_then(Value::as_str)
            .or_else(|| item.as_str())
            .map(str::trim)
            .unwrap_or("");
        if id.is_empty() {
            continue;
        }
        if out.iter().any(|model| model.id == id) {
            continue;
        }
        let label = item
            .get("display_name")
            .or_else(|| item.get("displayName"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty() && *text != id);
        out.push(match label {
            Some(text) => ModelEntry::with_label(id, text),
            None => ModelEntry::new(id),
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(value: Value) -> Delta {
        let mut acc = StreamAcc::default();
        acc.apply(&value)
    }

    #[test]
    fn text_delta_accumulates() {
        let mut acc = StreamAcc::default();
        acc.apply(&json!({ "choices": [{ "delta": { "content": "你" } }] }));
        acc.apply(&json!({ "choices": [{ "delta": { "content": "好" } }] }));
        let outcome = acc.into_outcome();
        assert_eq!(outcome.text, "你好");
        assert!(outcome.tool_calls.is_empty());
    }

    #[test]
    fn reasoning_field_aliases() {
        let delta = chunk(json!({ "choices": [{ "delta": { "reasoning_content": "想" } }] }));
        assert_eq!(delta.reasoning, "想");
        let delta = chunk(json!({ "choices": [{ "delta": { "reasoning": "想" } }] }));
        assert_eq!(delta.reasoning, "想");
    }

    #[test]
    fn tool_arguments_are_assembled_per_index() {
        let mut acc = StreamAcc::default();
        acc.apply(&json!({ "choices": [{ "delta": { "tool_calls": [
            { "index": 0, "id": "c1", "function": { "name": "read_file", "arguments": "{\"pa" } }
        ] } }] }));
        acc.apply(&json!({ "choices": [{ "delta": { "tool_calls": [
            { "index": 0, "function": { "arguments": "th\":\"a.rs\"}" } }
        ] } }] }));
        let outcome = acc.into_outcome();
        assert_eq!(outcome.tool_calls.len(), 1);
        assert_eq!(outcome.tool_calls[0].name, "read_file");
        assert_eq!(outcome.tool_calls[0].arguments, "{\"path\":\"a.rs\"}");
        assert_eq!(outcome.tool_calls[0].id, "c1");
    }

    #[test]
    fn missing_id_gets_generated() {
        let mut acc = StreamAcc::default();
        acc.apply(&json!({ "choices": [{ "delta": { "tool_calls": [
            { "index": 0, "function": { "name": "list_files", "arguments": "{}" } }
        ] } }] }));
        let outcome = acc.into_outcome();
        assert_eq!(outcome.tool_calls[0].id, "call_0");
    }

    #[test]
    fn usage_is_captured_from_tail_chunk() {
        let mut acc = StreamAcc::default();
        acc.apply(&json!({ "choices": [], "usage": { "prompt_tokens": 11, "completion_tokens": 7 } }));
        let outcome = acc.into_outcome();
        assert_eq!(outcome.usage, Some(Usage { input: 11, output: 7 }));
    }

    #[test]
    fn content_array_is_joined() {
        let delta = chunk(json!({ "choices": [{ "delta": { "content": [
            { "type": "text", "text": "a" }, { "type": "text", "text": "b" }
        ] } }] }));
        assert_eq!(delta.text, "ab");
    }

    /// 起一个只回一段 SSE 的本地服务，验证「HTTP → 分行 → 拼装」整条链路。
    #[test]
    fn chat_reads_sse_from_local_server() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定本地端口");
        let port = listener.local_addr().expect("取端口").port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer);
                let body = concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"你\"}}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{\"content\":\"好\"}}]}\n\n",
                    "data: [DONE]\n\n"
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let config = ProviderConfig {
            id: "test".to_string(),
            label: "test".to_string(),
            protocol: "openai".to_string(),
            base_url: format!("http://127.0.0.1:{port}/v1"),
            api_key: "test-key".to_string(),
            models: vec![ModelEntry::new("test-model")],
        };
        let mut streamed = String::new();
        let mut on_text = |delta: &str| streamed.push_str(delta);
        let mut on_reasoning = |_delta: &str| {};
        let outcome = chat(
            &config,
            "test-model",
            &[json!({ "role": "user", "content": "hi" })],
            &[],
            &mut on_text,
            &mut on_reasoning,
            &|| false,
        )
        .expect("流式聊天应成功");
        assert_eq!(outcome.text, "你好");
        assert_eq!(streamed, "你好", "增量回调应逐段到达");
    }

    #[test]
    fn parse_models_accepts_common_shapes() {
        let openai = json!({ "data": [
            { "id": "b-model" },
            { "id": "a-model", "display_name": "A Model" },
            { "id": "a-model" }
        ] });
        let models = parse_models(&openai);
        assert_eq!(models.len(), 2, "重复 id 只留一条");
        assert_eq!(models[0].id, "a-model", "按 id 排序");
        assert_eq!(models[0].display(), "A Model");

        let ollama = json!({ "models": [{ "name": "llama3:8b" }] });
        assert_eq!(parse_models(&ollama)[0].id, "llama3:8b");

        let bare = json!(["m1", "m2"]);
        assert_eq!(parse_models(&bare).len(), 2);
        assert!(parse_models(&json!({ "error": "nope" })).is_empty());
    }

    /// 已经被叫停的回合不该再发请求出去。
    #[test]
    fn chat_returns_cancelled_without_sending() {
        let config = ProviderConfig {
            id: "stop".to_string(),
            label: "stop".to_string(),
            protocol: "openai".to_string(),
            // 端口留空：一旦真的去连就会失败，正好证明根本没发请求。
            base_url: "http://127.0.0.1:1/v1".to_string(),
            api_key: String::new(),
            models: vec![ModelEntry::new("m")],
        };
        let error = chat(
            &config,
            "m",
            &[json!({ "role": "user", "content": "hi" })],
            &[],
            &mut |_delta: &str| {},
            &mut |_delta: &str| {},
            &|| true,
        )
        .expect_err("应当直接返回中断");
        assert_eq!(error, ChatError::Cancelled);
        // 界面上还是原来那句话。
        assert_eq!(error.to_string(), "已停止本次回复");
    }

    /// 流卡住时按空闲超时收工（不再是陪着等总超时），而且算「出错」不算中断。
    #[test]
    fn chat_gives_up_on_idle_stream() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定本地端口");
        let port = listener.local_addr().expect("取端口").port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer);
                // 先给一个正常分片，然后卡住不再吐字。
                let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
                let chunk = "data: {\"choices\":[{\"delta\":{\"content\":\"你\"}}]}\n\n";
                let _ = stream.write_all(format!("{head}{chunk}").as_bytes());
                let _ = stream.flush();
                std::thread::sleep(Duration::from_secs(30));
            }
        });

        let config = ProviderConfig {
            id: "idle".to_string(),
            label: "idle".to_string(),
            protocol: "openai".to_string(),
            base_url: format!("http://127.0.0.1:{port}/v1"),
            api_key: String::new(),
            models: vec![ModelEntry::new("m")],
        };
        let mut text = String::new();
        let started = Instant::now();
        let error = chat(
            &config,
            "m",
            &[json!({ "role": "user", "content": "hi" })],
            &[],
            &mut |delta: &str| text.push_str(delta),
            &mut |_delta: &str| {},
            &|| false,
        )
        .expect_err("卡住的流应当被放弃");
        assert!(matches!(error, ChatError::Failed(_)), "{error:?}");
        assert!(error.to_string().contains("没有任何响应"), "{error}");
        assert_eq!(text, "你", "已经收到的增量要留住");
        assert!(started.elapsed() < Duration::from_secs(10), "别等总超时");
    }

    /// `GET /models` 的完整链路：拼路径 → 带 Bearer → 解析。
    #[test]
    fn fetch_models_hits_models_endpoint() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定本地端口");
        let port = listener.local_addr().expect("取端口").port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buffer = [0u8; 4096];
                let size = stream.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..size]).to_string();
                let body = json!({ "data": [{ "id": "hit-model" }] }).to_string();
                let authorized = request.to_ascii_lowercase().contains("authorization: bearer");
                let path_ok = request.starts_with("GET /v1/models");
                let payload = if authorized && path_ok {
                    format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body)
                } else {
                    "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                };
                let _ = stream.write_all(payload.as_bytes());
            }
        });

        let models = fetch_models(&ProviderConfig {
            id: "adhoc".to_string(),
            label: String::new(),
            protocol: "openai".to_string(),
            base_url: format!("http://127.0.0.1:{port}/v1"),
            api_key: "sk-test".to_string(),
            models: Vec::new(),
        })
        .expect("应能拉到模型");
        assert_eq!(models[0].id, "hit-model");
    }

    /// 真实网络的连通性检查（默认忽略，`cargo test --lib live_ -- --ignored`）。
    ///
    /// 不带密钥时 DeepSeek 会回 401——只要能读到状态码，就说明 TLS、DNS、
    /// 代理这一层全是通的，剩下的才是密钥问题。
    #[test]
    #[ignore]
    fn live_https_reaches_deepseek() {
        let error = fetch_models(&ProviderConfig {
            id: "live".to_string(),
            label: String::new(),
            protocol: "openai".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            api_key: String::new(),
            models: Vec::new(),
        })
        .expect_err("没有密钥应当报错");
        assert!(
            !error.contains("连不上"),
            "网络层就没通，检查 TLS / 代理：{error}"
        );
        assert!(error.contains("401"), "预期 401，实际：{error}");
    }

    /// 只留第一层错误会变成「error sending request」这种废话，必须把 source 链拼出来。
    #[test]
    fn describe_walks_the_whole_source_chain() {
        #[derive(Debug)]
        struct Layer {
            text: &'static str,
            cause: Option<Box<Layer>>,
        }
        impl std::fmt::Display for Layer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.text)
            }
        }
        impl std::error::Error for Layer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                self.cause
                    .as_deref()
                    .map(|inner| inner as &(dyn std::error::Error + 'static))
            }
        }

        let error = Layer {
            text: "error sending request for url (https://api.deepseek.com/v1/models)",
            cause: Some(Box::new(Layer {
                text: "client error (Connect)",
                cause: Some(Box::new(Layer {
                    text: "tls handshake eof",
                    cause: None,
                })),
            })),
        };
        let text = describe(&error);
        assert!(text.starts_with("error sending request"), "{text}");
        assert!(text.contains("→ client error (Connect)"), "{text}");
        assert!(text.ends_with("→ tls handshake eof"), "{text}");
    }
}
