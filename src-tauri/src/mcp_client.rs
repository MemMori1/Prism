//! MCP stdio 客户端：起一个 MCP server 子进程，按换行分隔的 JSON-RPC 说话。
//!
//! 为什么只做 stdio
//! ----------------
//! `engines/mcp.rs` 那一层是**只读盘点**——把三家引擎配置文件里的 MCP 定义
//! 列出来给人看，从不拉起任何进程。这里是执行侧：prism 自己要当 MCP 客户端，
//! 就得真的连上去拿 `tools/list`、发 `tools/call`。
//!
//! stdio 先做，是因为它自带进程生命周期：谁拉起的谁收，退出时杀干净就行，
//! 不需要处理 URL、鉴权、断线重连。HTTP 传输留给以后——那时候缺的不是代码
//! 量，而是"连接属于谁"的答案。
//!
//! 配置独立
//! --------
//! 只读 `~/.prism/mcp.json`，**不碰** opencode / codex / claude 的配置文件：
//! 那是别人的地方，改它们会把"我想多接一个工具"变成"我改了三个引擎的配置"。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

/// 单次工具结果最多带多少字符回来（和内置工具同档，避免一轮撑爆上下文）。
const MAX_RESULT_CHARS: usize = 20_000;

/// `~/.prism/mcp.json` 里一条 server 定义。
#[derive(Debug, Clone, Deserialize)]
pub struct ServerSpec {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// 显式给的环境变量。不继承宿主的一堆无关变量，避免把密钥顺手传出去。
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub disabled: bool,
    /// 这家 server 的工具默认开。
    ///
    /// 默认 `false`：外部工具不该因为"配置里写了一行"就在用户不知情下拿到
    /// 执行权。信任是显式给的，不是默认给的。
    #[serde(default)]
    pub trusted: bool,
    /// 单次请求超时（秒），默认 30。
    pub timeout_secs: Option<u64>,
}

impl ServerSpec {
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs.unwrap_or(30))
    }
}

#[derive(Debug, Default, Deserialize)]
struct McpConfig {
    #[serde(default)]
    servers: HashMap<String, ServerSpec>,
}

/// 配置文件位置（与 `plugins.json` 同一个目录）。
pub fn config_path() -> Option<PathBuf> {
    crate::engines::opencode::search::prism_dir().map(|dir| dir.join("mcp.json"))
}

/// 读配置：按名字排序，跳过 `disabled`。
///
/// 文件不存在、解析失败一律当作"没配"——和插件开关同样的处置：配置坏掉
/// 不该让应用起不来，最坏只是少了几个外部工具。
pub fn load_servers() -> Vec<(String, ServerSpec)> {
    let Some(path) = config_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let config = match serde_json::from_str::<McpConfig>(&text) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("[prism] mcp.json 解析失败，按未配置处理：{error}");
            return Vec::new();
        }
    };
    let mut servers: Vec<(String, ServerSpec)> = config
        .servers
        .into_iter()
        .filter(|(_, spec)| !spec.disabled)
        .collect();
    servers.sort_by(|a, b| a.0.cmp(&b.0));
    servers
}

/// 一条 stdio 连接。
///
/// 读线程负责把响应按 id 分发给等待者；写走一把单独的锁。请求-响应用
/// channel 配对，这样多个工具调用可以并发，不必串行排队。
pub struct StdioClient {
    name: String,
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    pending: Arc<Mutex<HashMap<i64, Sender<Value>>>>,
    next_id: AtomicI64,
    timeout: Duration,
}

impl StdioClient {
    /// 起进程 + 握手。握手失败就把子进程杀掉，不留半个连接。
    pub fn start(name: &str, spec: &ServerSpec) -> Result<Arc<Self>, String> {
        let mut command = Command::new(&spec.command);
        command
            .args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in &spec.env {
            command.env(key, value);
        }

        let mut child = command
            .spawn()
            .map_err(|error| format!("启动 {} 失败：{error}", spec.command))?;
        let stdin = child.stdin.take().ok_or_else(|| "拿不到 stdin".to_string())?;
        let stdout = child.stdout.take().ok_or_else(|| "拿不到 stdout".to_string())?;
        if let Some(stderr) = child.stderr.take() {
            let label = name.to_string();
            // stderr 必须有人读：管道写满会把服务器卡死，看起来像"超时"。
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    eprintln!("[mcp:{label}] {line}");
                }
            });
        }

        let pending: Arc<Mutex<HashMap<i64, Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        {
            let pending = pending.clone();
            std::thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    let Ok(line) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    let Ok(message) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    let Some(id) = message.get("id").and_then(Value::as_i64) else {
                        // 服务器主动通知（日志、进度）：我们不消费，丢掉。
                        continue;
                    };
                    if message.get("method").is_some() {
                        // 服务器反向请求（sampling / roots）：prism 不提供，
                        // 直接丢弃。真接的话这里要回一个"不支持"的错误。
                        continue;
                    }
                    let sender = pending
                        .lock()
                        .ok()
                        .and_then(|mut map| map.remove(&id));
                    if let Some(sender) = sender {
                        let _ = sender.send(message);
                    }
                }
                // 进程没了：清空等待者，让所有 recv 立刻返回 Err，而不是干等到超时。
                if let Ok(mut map) = pending.lock() {
                    map.clear();
                }
            });
        }

        let client = Arc::new(Self {
            name: name.to_string(),
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            pending,
            next_id: AtomicI64::new(1),
            timeout: spec.timeout(),
        });

        if let Err(error) = client.handshake() {
            client.shutdown();
            return Err(error);
        }
        Ok(client)
    }

    fn handshake(&self) -> Result<(), String> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "prism", "version": env!("CARGO_PKG_VERSION") },
            }),
        )?;
        // 通知没有响应，发完就往下走。
        self.notify("notifications/initialized", json!({}))
    }

    /// 服务器提供了哪些工具（原样返回 MCP 的 tool 描述）。
    pub fn list_tools(&self) -> Result<Vec<Value>, String> {
        let result = self.request("tools/list", json!({}))?;
        Ok(result
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// 调一次工具，把 content 拼成一段文本。`isError` 为真时按失败返回。
    pub fn call_tool(&self, tool: &str, arguments: &Value) -> Result<String, String> {
        let result = self.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        )?;
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let text = flatten_content(&result);
        if is_error {
            Err(text)
        } else {
            Ok(text)
        }
    }

    fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = mpsc::channel();
        if let Ok(mut map) = self.pending.lock() {
            map.insert(id, sender);
        }

        let payload = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if let Err(error) = self.send_line(&payload) {
            if let Ok(mut map) = self.pending.lock() {
                map.remove(&id);
            }
            return Err(error);
        }

        match receiver.recv_timeout(self.timeout) {
            Ok(message) => {
                if let Some(error) = message.get("error") {
                    let text = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("未知错误");
                    return Err(format!("{method} 失败：{text}"));
                }
                Ok(message.get("result").cloned().unwrap_or(Value::Null))
            }
            Err(_) => {
                if let Ok(mut map) = self.pending.lock() {
                    map.remove(&id);
                }
                Err(format!(
                    "{method} 超时（{}s）；服务器可能已退出",
                    self.timeout.as_secs()
                ))
            }
        }
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        self.send_line(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    fn send_line(&self, message: &Value) -> Result<(), String> {
        let mut line = serde_json::to_string(message).map_err(|error| error.to_string())?;
        line.push('\n');
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|_| "stdin 锁中毒".to_string())?;
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|error| format!("写入 {} 失败：{error}", self.name))
    }

    /// 杀子进程。可重入（重复调用只是再 kill 一次已死的进程）。
    pub fn shutdown(&self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Ok(mut map) = self.pending.lock() {
            map.clear();
        }
    }
}

impl Drop for StdioClient {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// MCP 的 `content` 数组拼文本：文本直接取，其它类型（图片、资源）只留标记，
/// 免得把 base64 塞进上下文。
fn flatten_content(result: &Value) -> String {
    let Some(items) = result.get("content").and_then(Value::as_array) else {
        return clip(result.to_string());
    };
    let mut parts: Vec<String> = Vec::new();
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("text") => parts.push(
                item.get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ),
            Some(kind) => parts.push(format!("[{kind} 内容省略]")),
            None => {}
        }
    }
    if parts.is_empty() {
        clip(result.to_string())
    } else {
        clip(parts.join("\n"))
    }
}

fn clip(text: String) -> String {
    if text.chars().count() <= MAX_RESULT_CHARS {
        return text;
    }
    let head: String = text.chars().take(MAX_RESULT_CHARS).collect();
    format!("{head}\n…（已截断，原长 {} 字符）", text.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个只会说 JSON-RPC 的假 server。用 python 是因为本机就带，
    /// 也不必为了测试往仓库里塞一个二进制。
    const FAKE_SERVER: &str = r#"
import sys, json
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    message = json.loads(line)
    if "id" not in message:
        continue
    method = message.get("method")
    if method == "initialize":
        result = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
                  "serverInfo": {"name": "fake", "version": "0"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "echo", "description": "回声",
                             "inputSchema": {"type": "object",
                                             "properties": {"text": {"type": "string"}}}}]}
    elif method == "tools/call":
        args = message["params"].get("arguments") or {}
        result = {"content": [{"type": "text", "text": "echo:" + str(args.get("text"))}]}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}), flush=True)
"#;

    fn fake_spec(script: &std::path::Path) -> ServerSpec {
        ServerSpec {
            command: "/usr/bin/python3".to_string(),
            args: vec![script.display().to_string()],
            env: HashMap::new(),
            disabled: false,
            trusted: false,
            timeout_secs: Some(10),
        }
    }

    fn write_script() -> Option<std::path::PathBuf> {
        if !std::path::Path::new("/usr/bin/python3").is_file() {
            return None;
        }
        let path = std::env::temp_dir().join(format!("prism-fake-mcp-{}.py", std::process::id()));
        std::fs::write(&path, FAKE_SERVER).expect("写假 server");
        Some(path)
    }

    /// 握手 + 列工具 + 调工具，一条链路走通。
    #[test]
    fn list_and_call_over_stdio() {
        let Some(script) = write_script() else {
            eprintln!("跳过：本机没有 /usr/bin/python3");
            return;
        };
        let client = StdioClient::start("fake", &fake_spec(&script)).expect("启动");

        let tools = client.list_tools().expect("列工具");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "echo");

        let text = client
            .call_tool("echo", &json!({ "text": "你好" }))
            .expect("调工具");
        assert_eq!(text, "echo:你好");

        client.shutdown();
        let _ = std::fs::remove_file(&script);
    }

    /// 服务器不存在时给的是可读错误，而不是 panic。
    #[test]
    fn missing_command_fails_readably() {
        let error = StdioClient::start(
            "gone",
            &ServerSpec {
                command: "/nonexistent/mcp-server".to_string(),
                args: Vec::new(),
                env: HashMap::new(),
                disabled: false,
                trusted: false,
                timeout_secs: Some(1),
            },
        )
        .err()
        .expect("应当失败");
        assert!(error.contains("启动"), "{error}");
    }

    #[test]
    fn flatten_keeps_text_and_marks_other_types() {
        let result = json!({
            "content": [
                { "type": "text", "text": "第一行" },
                { "type": "image", "data": "AAA" }
            ]
        });
        assert_eq!(flatten_content(&result), "第一行\n[image 内容省略]");
    }

    #[test]
    fn clip_cuts_long_output() {
        let text = clip("a".repeat(MAX_RESULT_CHARS + 10));
        assert!(text.contains("已截断"), "{text}");
        assert!(text.chars().count() < MAX_RESULT_CHARS + 60);
    }
}
