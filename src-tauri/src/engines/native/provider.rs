//! OpenAI 兼容的流式客户端。
//!
//! 只做一件事：把 `POST {base}/chat/completions` 的 SSE 增量拼成一条完整回复。
//! 拼装逻辑（`StreamAcc`）是纯函数，可以脱离网络单测——这是最容易出错的部分
//! （工具入参是分片到达的，文本和推理也各有各的字段名）。

use super::config::{ModelEntry, ProviderConfig};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::time::Duration;

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
    let mut builder = reqwest::blocking::Client::builder()
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
fn describe(error: &(dyn std::error::Error + 'static)) -> String {
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

/// 被用户叫停时的返回值；上层靠它区分「出错」和「中断」。
pub const CANCELLED: &str = "已停止本次回复";

fn check_stop(should_stop: &dyn Fn() -> bool) -> Result<(), String> {
    if should_stop() {
        Err(CANCELLED.to_string())
    } else {
        Ok(())
    }
}

/// 流式跑一轮。`on_text` / `on_reasoning` 会在收到增量时被调用；
/// `should_stop` 在发请求前与每收到一行 SSE 时都会问一次，返回 true 就收工。
pub fn chat(
    provider: &ProviderConfig,
    model: &str,
    messages: &[Value],
    tools: &[Value],
    on_text: &mut dyn FnMut(&str),
    on_reasoning: &mut dyn FnMut(&str),
    should_stop: &dyn Fn() -> bool,
) -> Result<Outcome, String> {
    if !provider.usable() {
        return Err("这个模型服务还没填地址".to_string());
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

    let mut request = client()?
        .post(provider.endpoint())
        .header("Content-Type", "application/json")
        .json(&body);
    if !provider.api_key.trim().is_empty() {
        request = request.bearer_auth(provider.api_key.trim());
    }
    let response = request
        .send()
        .map_err(|error| format!("请求失败：{}", describe(&error)))?;
    check_stop(should_stop)?;

    let status = response.status();
    if !status.is_success() {
        let detail = response.text().unwrap_or_default();
        return Err(format!(
            "模型返回 {status}：{}",
            truncate(detail.trim(), 400)
        ));
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
            .map_err(|error| format!("解析响应失败：{error}"))?;
        let delta = acc.apply(&value);
        if !delta.text.is_empty() {
            on_text(&delta.text);
        }
        if !delta.reasoning.is_empty() {
            on_reasoning(&delta.reasoning);
        }
        return Ok(acc.into_outcome());
    }

    let reader = BufReader::new(response);
    for line in reader.lines() {
        // 每个分片都问一次：模型吐字很密，这样中断延迟基本就是一行的时间。
        check_stop(should_stop)?;
        let line = line.map_err(|error| format!("读取流失败：{error}"))?;
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
            return Err(format!("模型返回错误：{}", error));
        }
        let delta = acc.apply(&chunk);
        if !delta.text.is_empty() {
            on_text(&delta.text);
        }
        if !delta.reasoning.is_empty() {
            on_reasoning(&delta.reasoning);
        }
    }
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
        assert_eq!(error, CANCELLED);
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
