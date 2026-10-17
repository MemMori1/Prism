//! MCP 服务清单：三引擎配置真相位置（只读解析）+ 探活。
//!
//! 真相位置（源码/官方文档核实，2026-10）：
//! - OpenCode：`<配置目录>/opencode.jsonc|json` 的 `mcp.servers.<名>`（v2 口径：local/remote、
//!   `disabled`；兼容旧 `enabled`）+ 项目根 `opencode.jsonc|json`。
//! - Codex：`~/.codex/config.toml` 的 `[mcp_servers.<名>]`（command/args/url；`.env` 子段取键名）。
//! - Claude Code：`~/.claude.json` 顶层 `mcpServers`（用户级）与 `projects.<cwd>.mcpServers`
//!   （local scope）+ 项目 `.mcp.json` + 用户/项目 `settings.json` 的 `mcpServers`。
//!
//! 边界：解析是"接受根路径的纯函数"；探活只对 http url 发 GET，
//! **绝不 spawn stdio**（stdio 服务是否可用只做命令路径存在性判断）。
//! 环境变量一律只取键名，不落值（防密钥进前端）。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServer {
    pub engine: String,
    pub name: String,
    /// stdio | http
    pub transport: String,
    /// stdio：可执行文件（首个 token）
    pub command: Option<String>,
    pub args: Vec<String>,
    /// http：远端地址
    pub url: Option<String>,
    pub disabled: bool,
    /// 环境变量键名（不暴露值）
    pub env_keys: Vec<String>,
    /// 定义所在配置文件
    pub source: String,
    /// 归属：user / project
    pub scope: String,
    pub project: Option<String>,
    /// stdio：命令是否可解析（绝对路径存在 / PATH 可找到）
    pub command_exists: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    pub ok: bool,
    pub status: Option<u16>,
    pub latency_ms: u64,
    pub error: Option<String>,
}

/// 命令层装配的根集合。
pub struct McpRoots {
    /// 家目录（`~/.claude.json` 所在）
    pub home: Option<PathBuf>,
    pub opencode_config: Option<PathBuf>,
    pub codex_home: Option<PathBuf>,
}

/// 入口：扫描三引擎用户级与项目级 MCP 定义（只读）。
pub fn collect(roots: &McpRoots, project_cwds: &[String]) -> Vec<McpServer> {
    let mut servers: Vec<McpServer> = Vec::new();

    if let Some(config) = roots.opencode_config.as_deref() {
        for file in ["opencode.jsonc", "opencode.json"] {
            servers.extend(opencode_servers(&config.join(file), "user", None));
        }
    }

    if let Some(home) = roots.home.as_deref() {
        // Claude：用户级 + 各项目 local scope（同一个 .claude.json 两处真相）
        let claude_json = home.join(".claude.json");
        servers.extend(claude_json_servers(&claude_json));
        servers.extend(claude_config_servers(
            &home.join(".claude").join("settings.json"),
            "user",
            None,
        ));
    }

    if let Some(codex) = roots.codex_home.as_deref() {
        servers.extend(codex_servers(&codex.join("config.toml")));
    }

    for cwd in project_cwds {
        let root = Path::new(cwd);
        if !root.is_dir() {
            continue;
        }
        for file in ["opencode.jsonc", "opencode.json"] {
            servers.extend(opencode_servers(&root.join(file), "project", Some(cwd)));
        }
        servers.extend(claude_config_servers(&root.join(".mcp.json"), "project", Some(cwd)));
        servers.extend(claude_config_servers(
            &root.join(".claude").join("settings.json"),
            "project",
            Some(cwd),
        ));
    }

    servers.sort_by(|a, b| {
        (
            engine_rank(&a.engine),
            scope_rank(&a.scope),
            a.project.as_deref().unwrap_or(""),
            a.name.as_str(),
            a.source.as_str(),
        )
            .cmp(&(
                engine_rank(&b.engine),
                scope_rank(&b.scope),
                b.project.as_deref().unwrap_or(""),
                b.name.as_str(),
                b.source.as_str(),
            ))
    });
    servers
}

fn engine_rank(engine: &str) -> u8 {
    match engine {
        "opencode" => 0,
        "codex" => 1,
        "claude" => 2,
        _ => 9,
    }
}

fn scope_rank(scope: &str) -> u8 {
    match scope {
        "user" => 0,
        "project" => 1,
        _ => 9,
    }
}

// ---------------------------------------------------------------- 各引擎解析

fn opencode_servers(path: &Path, scope: &str, project: Option<&str>) -> Vec<McpServer> {
    let Some(value) = read_jsonc(path) else {
        return Vec::new();
    };
    let Some(servers) = value.pointer("/mcp/servers").and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    servers
        .iter()
        .map(|(name, entry)| {
            let is_remote = entry.get("type").and_then(|v| v.as_str()) == Some("remote");
            let url = entry
                .get("url")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let mut command = None;
            let mut args = Vec::new();
            if !is_remote {
                if let Some(list) = entry.get("command").and_then(|v| v.as_array()) {
                    let parts: Vec<String> = list
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect();
                    if let Some((first, rest)) = parts.split_first() {
                        command = Some(first.clone());
                        args = rest.to_vec();
                    }
                }
            }
            let env_keys = entry
                .get("environment")
                .and_then(|v| v.as_object())
                .map(|obj| obj.keys().cloned().collect())
                .unwrap_or_default();
            named_server("opencode", name, url, command, args, env_keys)
                .with_flags(entry, scope, project, path)
        })
        .collect()
}

/// Codex：`config.toml` 的 `[mcp_servers.<名>]` 段（极简 TOML 子集解析）。
fn codex_servers(path: &Path) -> Vec<McpServer> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut servers: Vec<McpServer> = Vec::new();
    let mut section: Vec<String> = Vec::new();
    let mut pending_array: Option<String> = None;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if let Some(buffer) = pending_array.as_mut() {
            buffer.push(' ');
            buffer.push_str(line);
            if line.contains(']') {
                let values = scan_quoted(buffer);
                if let Some(current) = current_codex(&mut servers, &section) {
                    current.args = values;
                }
                pending_array = None;
            }
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            let header = line
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim()
                .to_string();
            section = section_path(&header);
            if is_codex_server_section(&section) {
                let name = section[1].clone();
                if !servers.iter().any(|s| s.name == name) {
                    servers.push(McpServer {
                        engine: "codex".to_string(),
                        name,
                        transport: "stdio".to_string(),
                        command: None,
                        args: Vec::new(),
                        url: None,
                        disabled: false,
                        env_keys: Vec::new(),
                        source: path.display().to_string(),
                        scope: "user".to_string(),
                        project: None,
                        command_exists: None,
                    });
                }
            }
            continue;
        }
        if !is_codex_server_section(&section) {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        // `.env` 子段：只记键名
        if section.len() >= 3 && section[2] == "env" {
            if let Some(current) = current_codex(&mut servers, &section) {
                current.env_keys.push(key.to_string());
            }
            continue;
        }
        match key {
            "command" => {
                if let (Some(current), Some(command)) =
                    (current_codex(&mut servers, &section), scan_quoted(value).into_iter().next())
                {
                    current.command = Some(command);
                }
            }
            "args" => {
                if value.starts_with('[') && !value.contains(']') {
                    pending_array = Some(value.to_string());
                } else if let Some(current) = current_codex(&mut servers, &section) {
                    current.args = scan_quoted(value);
                }
            }
            "url" => {
                if let (Some(current), Some(url)) =
                    (current_codex(&mut servers, &section), scan_quoted(value).into_iter().next())
                {
                    current.url = Some(url);
                    current.transport = "http".to_string();
                }
            }
            "enabled" => {
                if let Some(current) = current_codex(&mut servers, &section) {
                    current.disabled = value.trim() == "false";
                }
            }
            _ => {}
        }
    }

    for server in servers.iter_mut() {
        finalize_stdio(server);
    }
    servers
}

fn is_codex_server_section(section: &[String]) -> bool {
    section.first().map(String::as_str) == Some("mcp_servers") && section.len() >= 2
}

fn current_codex<'a>(servers: &'a mut Vec<McpServer>, section: &[String]) -> Option<&'a mut McpServer> {
    let name = section.get(1)?;
    servers.iter_mut().find(|s| &s.name == name)
}

/// `~/.claude.json`：顶层 mcpServers（用户级）+ projects.<cwd>.mcpServers（项目 local scope）。
fn claude_json_servers(path: &Path) -> Vec<McpServer> {
    let Some(value) = read_json(path) else {
        return Vec::new();
    };
    let mut servers = Vec::new();
    if let Some(obj) = value.get("mcpServers").and_then(|v| v.as_object()) {
        servers.extend(named_claude_servers(obj, path, "user", None));
    }
    if let Some(projects) = value.get("projects").and_then(|v| v.as_object()) {
        for (cwd, entry) in projects {
            if !Path::new(cwd).is_dir() {
                continue;
            }
            if let Some(obj) = entry.get("mcpServers").and_then(|v| v.as_object()) {
                servers.extend(named_claude_servers(obj, path, "project", Some(cwd)));
            }
        }
    }
    servers
}

/// Claude 纯 JSON 配置（`.mcp.json` / `settings.json`）的 `mcpServers`。
fn claude_config_servers(path: &Path, scope: &str, project: Option<&str>) -> Vec<McpServer> {
    let Some(value) = read_json(path) else {
        return Vec::new();
    };
    value
        .get("mcpServers")
        .and_then(|v| v.as_object())
        .map(|obj| named_claude_servers(obj, path, scope, project))
        .unwrap_or_default()
}

fn named_claude_servers(
    obj: &serde_json::Map<String, serde_json::Value>,
    path: &Path,
    scope: &str,
    project: Option<&str>,
) -> Vec<McpServer> {
    obj.iter()
        .map(|(name, entry)| {
            let url = entry
                .get("url")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let command = entry
                .get("command")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let args = entry
                .get("args")
                .and_then(|v| v.as_array())
                .map(|list| {
                    list.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let env_keys = entry
                .get("env")
                .and_then(|v| v.as_object())
                .map(|obj| obj.keys().cloned().collect())
                .unwrap_or_default();
            let disabled = entry
                .get("disabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let mut server = named_server("claude", name, url, command, args, env_keys);
            server.disabled = disabled;
            server.source = path.display().to_string();
            server.scope = scope.to_string();
            server.project = project.map(str::to_string);
            finalize_stdio(&mut server);
            server
        })
        .collect()
}

/// 组装未定归属的服务器（供各解析器补 scope/source/disabled）。
fn named_server(
    engine: &str,
    name: &str,
    url: Option<String>,
    command: Option<String>,
    args: Vec<String>,
    env_keys: Vec<String>,
) -> McpServer {
    let transport = if url.is_some() { "http" } else { "stdio" };
    McpServer {
        engine: engine.to_string(),
        name: name.to_string(),
        transport: transport.to_string(),
        command,
        args,
        url,
        disabled: false,
        env_keys,
        source: String::new(),
        scope: "user".to_string(),
        project: None,
        command_exists: None,
    }
}

trait WithFlags {
    fn with_flags(
        self,
        entry: &serde_json::Value,
        scope: &str,
        project: Option<&str>,
        path: &Path,
    ) -> McpServer;
}

impl WithFlags for McpServer {
    fn with_flags(
        mut self,
        entry: &serde_json::Value,
        scope: &str,
        project: Option<&str>,
        path: &Path,
    ) -> McpServer {
        // v2 用 disabled；旧配置用 enabled（兼容）
        let disabled = entry.get("disabled").and_then(|v| v.as_bool()).unwrap_or_else(|| {
            entry
                .get("enabled")
                .and_then(|v| v.as_bool())
                .map(|enabled| !enabled)
                .unwrap_or(false)
        });
        self.disabled = disabled;
        self.source = path.display().to_string();
        self.scope = scope.to_string();
        self.project = project.map(str::to_string);
        finalize_stdio(&mut self);
        self
    }
}

/// stdio 服务补"命令是否可解析"，http 服务不需要。
fn finalize_stdio(server: &mut McpServer) {
    if server.transport == "stdio" {
        server.command_exists = server.command.as_deref().map(command_exists);
    }
}

// ---------------------------------------------------------------- 工具

/// 去 JSONC 注释（`//`、`/* */`，跳过字符串内部）并去掉尾随逗号，再按 JSON 解析。
fn read_jsonc(path: &Path) -> Option<serde_json::Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&normalize_jsonc(&text)).ok()
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn normalize_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            match ch {
                '\\' => {
                    if let Some(next) = chars.next() {
                        out.push(next);
                    }
                }
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                out.push(ch);
            }
            '/' => match chars.peek() {
                Some('/') => {
                    for comment in chars.by_ref() {
                        if comment == '\n' {
                            out.push('\n');
                            break;
                        }
                    }
                }
                Some('*') => {
                    chars.next();
                    let mut previous = '\0';
                    for comment in chars.by_ref() {
                        if previous == '*' && comment == '/' {
                            break;
                        }
                        previous = comment;
                    }
                }
                _ => out.push(ch),
            },
            ',' => {
                // 尾随逗号：后面（跳过空白）若是 } 或 ] 则丢弃
                let mut lookahead = chars.clone();
                let mut trailing = false;
                for next in lookahead.by_ref() {
                    if next.is_whitespace() {
                        continue;
                    }
                    trailing = next == '}' || next == ']';
                    break;
                }
                if !trailing {
                    out.push(ch);
                }
            }
            _ => out.push(ch),
        }
    }
    out
}

/// 极简 TOML 段名切分：`mcp_servers."名.带点".env` → [mcp_servers, 名.带点, env]。
fn section_path(header: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = header.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '.' if !in_quotes => {
                parts.push(current.trim().to_string());
                current = String::new();
            }
            '\\' if in_quotes => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            _ => current.push(ch),
        }
    }
    parts.push(current.trim().to_string());
    parts
}

/// 扫描文本里所有双引号字符串（TOML 值用；支持常见转义）。
fn scan_quoted(raw: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '"' {
            continue;
        }
        let mut out = String::new();
        let mut escaped = false;
        for inner in chars.by_ref() {
            if escaped {
                out.push(match inner {
                    'n' => '\n',
                    't' => '\t',
                    '"' => '"',
                    '\\' => '\\',
                    other => other,
                });
                escaped = false;
            } else if inner == '\\' {
                escaped = true;
            } else if inner == '"' {
                break;
            } else {
                out.push(inner);
            }
        }
        items.push(out);
    }
    items
}

/// 命令是否可解析：带路径判断文件存在；裸命令名扫 PATH。
pub fn command_exists(command: &str) -> bool {
    let command = command.trim();
    if command.is_empty() {
        return false;
    }
    if command.contains('/') {
        return Path::new(command).is_file();
    }
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| dir.join(command).is_file())
        })
        .unwrap_or(false)
}

/// 探活：只对 http 发 GET（任何 HTTP 响应都算可达），超时即失败。
pub fn probe_http(url: &str, timeout: Duration) -> ProbeResult {
    let start = Instant::now();
    let client = match reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
    {
        Ok(client) => client,
        Err(cause) => {
            return ProbeResult {
                ok: false,
                status: None,
                latency_ms: 0,
                error: Some(cause.to_string()),
            }
        }
    };
    match client.get(url).send() {
        Ok(response) => ProbeResult {
            ok: true,
            status: Some(response.status().as_u16()),
            latency_ms: start.elapsed().as_millis() as u64,
            error: None,
        },
        Err(cause) => ProbeResult {
            ok: false,
            status: None,
            latency_ms: start.elapsed().as_millis() as u64,
            error: Some(cause.to_string()),
        },
    }
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().expect("父目录")).expect("建目录");
        fs::write(path, content).expect("写文件");
    }

    fn fixture() -> (PathBuf, McpRoots, PathBuf) {
        let base = std::env::temp_dir().join(format!("prism-mcp-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let home = base.join("home");
        let opencode = base.join("opencode");
        let codex = base.join("codex");
        let project = base.join("proj");

        write(
            &opencode.join("opencode.jsonc"),
            r#"{
  // 全局 MCP：注释 + 尾随逗号都要容忍
  "mcp": {
    "servers": {
      "imessage": {
        "type": "local",
        "enabled": true,
        "command": ["/usr/bin/python3", "/x/imsg_mcp.py", "--mcp"],
      },
      "docs": {
        "type": "remote",
        "url": "https://mcp.example.com/mcp",
        "disabled": true,
      },
    },
  },
}"#,
        );
        write(
            &codex.join("config.toml"),
            r#"[mcp_servers.node_repl]
args = []
command = "/Applications/Codex.app/Contents/Resources/node_repl"
startup_timeout_sec = 120

[mcp_servers.node_repl.env]
NODE_REPL_TRUSTED_CODE_PATHS = "/x"

[mcp_servers.multi]
command = "npx"
args = [
  "-y",
  "one",
]

[mcp_servers.remote_one]
url = "https://mcp.remote.test/sse"
"#,
        );
        write(
            &home.join(".claude.json"),
            &format!(
                r#"{{
  "mcpServers": {{
    "figma": {{ "command": "npx", "args": ["-y", "figma-mcp"], "env": {{ "FIGMA_KEY": "secret" }} }}
  }},
  "projects": {{
    "{}": {{ "mcpServers": {{ "local-tools": {{ "type": "sse", "url": "https://local.test/sse" }} }} }}
  }}
}}"#,
                project.display()
            ),
        );
        write(
            &project.join(".mcp.json"),
            r#"{"mcpServers": {"proj-one": {"command": "/bin/ls", "args": ["-la"]}}}"#,
        );

        let roots = McpRoots {
            home: Some(home),
            opencode_config: Some(opencode),
            codex_home: Some(codex),
        };
        (base, roots, project)
    }

    #[test]
    fn collects_servers_across_engines() {
        let (base, roots, project) = fixture();
        let servers = collect(&roots, &[project.display().to_string()]);

        let opencode: Vec<_> = servers.iter().filter(|s| s.engine == "opencode").collect();
        assert_eq!(opencode.len(), 2);
        let imessage = opencode.iter().find(|s| s.name == "imessage").expect("imessage");
        assert_eq!(imessage.transport, "stdio");
        assert_eq!(imessage.command.as_deref(), Some("/usr/bin/python3"));
        assert_eq!(imessage.args.len(), 2);
        assert!(!imessage.disabled, "enabled: true 不算禁用");
        assert_eq!(imessage.command_exists, Some(true));
        let docs = opencode.iter().find(|s| s.name == "docs").expect("docs");
        assert_eq!(docs.transport, "http");
        assert!(docs.disabled);

        let codex: Vec<_> = servers.iter().filter(|s| s.engine == "codex").collect();
        assert_eq!(codex.len(), 3);
        let node = codex.iter().find(|s| s.name == "node_repl").expect("node_repl");
        assert_eq!(node.env_keys, vec!["NODE_REPL_TRUSTED_CODE_PATHS".to_string()]);
        let multi = codex.iter().find(|s| s.name == "multi").expect("multi");
        assert_eq!(multi.args, vec!["-y".to_string(), "one".to_string()]);
        let remote = codex.iter().find(|s| s.name == "remote_one").expect("remote");
        assert_eq!(remote.transport, "http");
        assert_eq!(remote.url.as_deref(), Some("https://mcp.remote.test/sse"));

        let claude: Vec<_> = servers.iter().filter(|s| s.engine == "claude").collect();
        assert_eq!(claude.len(), 3);
        let figma = claude.iter().find(|s| s.name == "figma").expect("figma");
        assert_eq!(figma.scope, "user");
        assert_eq!(figma.env_keys, vec!["FIGMA_KEY".to_string()]);
        let scoped = claude.iter().find(|s| s.name == "local-tools").expect("local-tools");
        assert_eq!(scoped.scope, "project");
        assert_eq!(scoped.project.as_deref(), Some(project.to_string_lossy().as_ref()));
        let proj = claude.iter().find(|s| s.name == "proj-one").expect("proj-one");
        assert_eq!(proj.transport, "stdio");
        assert_eq!(proj.command_exists, Some(true));

        // 排序：引擎序 opencode → codex → claude
        let ranks: Vec<u8> = servers.iter().map(|s| engine_rank(&s.engine)).collect();
        assert!(ranks.windows(2).all(|pair| pair[0] <= pair[1]));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn normalize_jsonc_handles_comments_and_trailing_commas() {
        let text = r#"{
  // 行注释
  "a": "// 字符串里的斜杠",
  /* 块
     注释 */
  "b": [1, 2,],
}"#;
        let value: serde_json::Value = serde_json::from_str(&normalize_jsonc(text)).expect("解析");
        assert_eq!(value["a"], "// 字符串里的斜杠");
        assert_eq!(value["b"].as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn command_exists_checks_paths() {
        assert!(command_exists("/bin/ls"));
        assert!(!command_exists("/nonexistent/tool"));
    }

    #[test]
    fn probe_connection_refused_is_not_ok() {
        let result = probe_http("http://127.0.0.1:1/never", Duration::from_millis(1500));
        assert!(!result.ok);
        assert!(result.status.is_none());
    }

    /// 依赖本机真实配置：`cargo test -- --ignored --nocapture`。
    #[test]
    #[ignore]
    fn lists_real_mcp_servers() {
        let roots = McpRoots {
            home: std::env::var_os("HOME").map(PathBuf::from),
            opencode_config: crate::engines::opencode::locator::config_dir(),
            codex_home: crate::engines::codex::locator::codex_home(),
        };
        let servers = collect(&roots, &crate::ipc::known_project_cwds());
        println!("真实 MCP 服务 {} 个", servers.len());
        for server in &servers {
            println!(
                "  [{} / {} / {}] {} — {}",
                server.engine,
                server.scope,
                server.transport,
                server.name,
                server
                    .url
                    .clone()
                    .or_else(|| server.command.clone())
                    .unwrap_or_default()
            );
        }
    }
}
