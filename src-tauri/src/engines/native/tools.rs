//! 原生引擎的内置工具。
//!
//! 六个就够覆盖日常：读、列、搜、写、改、跑命令。
//! 所有路径先做词法归一 + 前缀校验，禁止越出会话工作目录；命令有超时和输出上限。

use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

/// 单次工具输出的字符上限（超出截断，避免一轮就把上下文撑爆）。
const MAX_OUTPUT_CHARS: usize = 20_000;
/// 读文件默认行数。
const READ_LINES: usize = 2000;
/// 列目录的最大条目数。
const LIST_ENTRIES: usize = 300;
/// 搜索命中上限。
const GREP_HITS: usize = 120;
/// 搜索时跳过的大文件（字节）。
const GREP_MAX_BYTES: u64 = 1_500_000;
/// 遍历时永远跳过的目录。
const SKIP_DIRS: &[&str] = &[
    ".git", "node_modules", "target", "dist", "build", ".next", ".nuxt", ".venv", "__pycache__",
    ".turbo", ".cache", "vendor",
];

#[derive(Debug, Clone)]
pub struct ToolOutcome {
    pub text: String,
    /// 失败时为错误说明（界面上工具卡标红）。
    pub error: Option<String>,
}

impl ToolOutcome {
    fn ok(text: impl Into<String>) -> Self {
        ToolOutcome {
            text: text.into(),
            error: None,
        }
    }

    fn fail(text: impl Into<String>) -> Self {
        let text = text.into();
        ToolOutcome {
            error: Some(text.clone()),
            text,
        }
    }
}

/// 发给模型的工具声明（OpenAI function schema）。
pub fn specs() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "读取工作目录里的一个文本文件，带行号。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "相对工作目录的路径" },
                        "offset": { "type": "integer", "description": "起始行（1 开始），默认 1" },
                        "limit": { "type": "integer", "description": "最多读多少行，默认 2000" }
                    },
                    "required": ["path"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "list_files",
                "description": "列出目录下的文件，最多三层。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "相对路径，默认工作目录根" },
                        "pattern": { "type": "string", "description": "简单的通配过滤，如 *.rs" }
                    }
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "grep",
                "description": "在文件内容里搜子串，返回 文件:行号:内容。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "要搜的文本" },
                        "path": { "type": "string", "description": "搜索起点，默认工作目录根" },
                        "ignore_case": { "type": "boolean", "description": "忽略大小写" }
                    },
                    "required": ["pattern"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "写文件（新建或整体覆盖）。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "content": { "type": "string" }
                    },
                    "required": ["path", "content"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "edit_file",
                "description": "把文件里的 old_string 精确替换成 new_string；old_string 必须唯一，或显式 replace_all。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "old_string": { "type": "string", "description": "要替换的原文（含缩进）" },
                        "new_string": { "type": "string" },
                        "replace_all": { "type": "boolean" }
                    },
                    "required": ["path", "old_string", "new_string"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "run_command",
                "description": "在工作目录里跑一条 shell 命令，返回退出码和输出。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" },
                        "timeout_ms": { "type": "integer", "description": "超时毫秒，默认 120000" }
                    },
                    "required": ["command"]
                }
            }
        }),
    ]
}

/// 执行一次工具调用。`arguments` 是模型给的 JSON 字符串。
pub fn execute(directory: &str, name: &str, arguments: &str) -> ToolOutcome {
    let args: Value = if arguments.trim().is_empty() {
        json!({})
    } else {
        match serde_json::from_str(arguments) {
            Ok(value) => value,
            Err(error) => return ToolOutcome::fail(format!("入参不是合法 JSON：{error}")),
        }
    };
    let root = Path::new(directory);
    match name {
        "read_file" => read_file(root, &args),
        "list_files" => list_files(root, &args),
        "grep" => grep(root, &args),
        "write_file" => write_file(root, &args),
        "edit_file" => edit_file(root, &args),
        "run_command" => run_command(root, &args),
        other => ToolOutcome::fail(format!("没有这个工具：{other}")),
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str).filter(|value| !value.is_empty())
}

fn int_arg(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(Value::as_u64)
}

fn bool_arg(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// 归一化路径并确认没跑出工作目录。
fn resolve(root: &Path, raw: &str) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(root.to_path_buf());
    }
    let candidate = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        root.join(raw)
    };
    // 词法归一：处理 `..` / `.`，不做 IO（文件可能还不存在）。
    let mut normalized = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    let root_abs = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf());
    let base = normalized
        .ancestors()
        .find(|path| path.exists())
        .and_then(|path| path.canonicalize().ok());
    let inside = match base {
        Some(base) => base.starts_with(&root_abs) || base == root_abs,
        None => normalized.starts_with(&root_abs),
    };
    if !inside {
        return Err(format!("路径越出工作目录：{raw}"));
    }
    Ok(normalized)
}

fn read_file(root: &Path, args: &Value) -> ToolOutcome {
    let Some(path) = str_arg(args, "path") else {
        return ToolOutcome::fail("缺少 path");
    };
    let target = match resolve(root, path) {
        Ok(target) => target,
        Err(error) => return ToolOutcome::fail(error),
    };
    let Ok(text) = std::fs::read_to_string(&target) else {
        return ToolOutcome::fail(format!("读不到 {}", target.display()));
    };
    let offset = int_arg(args, "offset").unwrap_or(1).max(1) as usize;
    let limit = int_arg(args, "limit").unwrap_or(READ_LINES as u64).clamp(1, 5000) as usize;
    let mut out = String::new();
    for (index, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
        out.push_str(&format!("{:>5}→{}\n", index + 1, line));
    }
    if out.is_empty() {
        return ToolOutcome::ok("（该范围没有内容）");
    }
    ToolOutcome::ok(clamp(&out))
}

fn list_files(root: &Path, args: &Value) -> ToolOutcome {
    let start = match str_arg(args, "path").map(|path| resolve(root, path)) {
        Some(Ok(path)) => path,
        Some(Err(error)) => return ToolOutcome::fail(error),
        None => root.to_path_buf(),
    };
    let pattern = str_arg(args, "pattern").map(str::to_string);
    let mut files = Vec::new();
    walk(&start, 0, &mut files);
    let mut out = String::new();
    let mut shown = 0;
    for file in files {
        let text = file.display().to_string();
        if let Some(pattern) = &pattern {
            let name = file.file_name().map(|v| v.to_string_lossy().to_string()).unwrap_or_default();
            if !wildcard_match(pattern, &name) {
                continue;
            }
        }
        if shown >= LIST_ENTRIES {
            out.push_str("…（还有更多，已截断）\n");
            break;
        }
        out.push_str(&format!("{text}\n"));
        shown += 1;
    }
    if out.is_empty() {
        return ToolOutcome::ok("（没有匹配的文件）");
    }
    ToolOutcome::ok(clamp(&out))
}

fn grep(root: &Path, args: &Value) -> ToolOutcome {
    let Some(pattern) = str_arg(args, "pattern") else {
        return ToolOutcome::fail("缺少 pattern");
    };
    let start = match str_arg(args, "path").map(|path| resolve(root, path)) {
        Some(Ok(path)) => path,
        Some(Err(error)) => return ToolOutcome::fail(error),
        None => root.to_path_buf(),
    };
    let ignore_case = bool_arg(args, "ignore_case");
    let needle = if ignore_case {
        pattern.to_lowercase()
    } else {
        pattern.to_string()
    };
    let mut files = Vec::new();
    walk(&start, 0, &mut files);
    let mut out = String::new();
    let mut hits = 0;
    for file in files {
        let Ok(meta) = std::fs::metadata(&file) else { continue };
        if meta.len() > GREP_MAX_BYTES {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        if text.contains('\u{0}') {
            continue;
        }
        for (index, line) in text.lines().enumerate() {
            let haystack = if ignore_case {
                line.to_lowercase()
            } else {
                line.to_string()
            };
            if !haystack.contains(&needle) {
                continue;
            }
            out.push_str(&format!(
                "{}:{}:{}\n",
                file.display(),
                index + 1,
                line.trim_end()
            ));
            hits += 1;
            if hits >= GREP_HITS {
                out.push_str("…（命中过多，已截断）\n");
                return ToolOutcome::ok(clamp(&out));
            }
        }
    }
    if out.is_empty() {
        return ToolOutcome::ok("（没有命中）");
    }
    ToolOutcome::ok(clamp(&out))
}

fn write_file(root: &Path, args: &Value) -> ToolOutcome {
    let Some(path) = str_arg(args, "path") else {
        return ToolOutcome::fail("缺少 path");
    };
    let Some(content) = args.get("content").and_then(Value::as_str) else {
        return ToolOutcome::fail("缺少 content");
    };
    let target = match resolve(root, path) {
        Ok(target) => target,
        Err(error) => return ToolOutcome::fail(error),
    };
    if let Some(parent) = target.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            return ToolOutcome::fail(format!("创建目录失败：{error}"));
        }
    }
    match std::fs::write(&target, content) {
        Ok(()) => ToolOutcome::ok(format!(
            "已写入 {}（{} 行）",
            target.display(),
            content.lines().count()
        )),
        Err(error) => ToolOutcome::fail(format!("写入失败：{error}")),
    }
}

fn edit_file(root: &Path, args: &Value) -> ToolOutcome {
    let Some(path) = str_arg(args, "path") else {
        return ToolOutcome::fail("缺少 path");
    };
    let Some(old) = args.get("old_string").and_then(Value::as_str) else {
        return ToolOutcome::fail("缺少 old_string");
    };
    let Some(new) = args.get("new_string").and_then(Value::as_str) else {
        return ToolOutcome::fail("缺少 new_string");
    };
    if old.is_empty() {
        return ToolOutcome::fail("old_string 不能为空（整体重写请用 write_file）");
    }
    let target = match resolve(root, path) {
        Ok(target) => target,
        Err(error) => return ToolOutcome::fail(error),
    };
    let Ok(text) = std::fs::read_to_string(&target) else {
        return ToolOutcome::fail(format!("读不到 {}", target.display()));
    };
    let count = text.matches(old).count();
    if count == 0 {
        return ToolOutcome::fail(
            "没有找到与 old_string 完全一致的片段：请带上完整缩进与上下文行再试",
        );
    }
    let replace_all = bool_arg(args, "replace_all");
    if count > 1 && !replace_all {
        return ToolOutcome::fail(format!(
            "old_string 出现了 {count} 次，请补足上下文让它唯一，或传 replace_all=true"
        ));
    }
    let updated = if replace_all {
        text.replace(old, new)
    } else {
        text.replacen(old, new, 1)
    };
    match std::fs::write(&target, &updated) {
        Ok(()) => ToolOutcome::ok(format!(
            "已替换 {} 处：{}",
            if replace_all { count } else { 1 },
            target.display()
        )),
        Err(error) => ToolOutcome::fail(format!("写入失败：{error}")),
    }
}

fn run_command(root: &Path, args: &Value) -> ToolOutcome {
    let Some(command) = str_arg(args, "command") else {
        return ToolOutcome::fail("缺少 command");
    };
    let timeout = Duration::from_millis(
        int_arg(args, "timeout_ms")
            .unwrap_or(120_000)
            .clamp(1_000, 600_000),
    );
    let mut process = if cfg!(windows) {
        let mut cmd = std::process::Command::new("cmd");
        cmd.arg("/C").arg(command);
        cmd
    } else {
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd
    };
    process
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => return ToolOutcome::fail(format!("启动命令失败：{error}")),
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if started.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return ToolOutcome::fail(format!(
                        "命令超时（{} ms）已终止：{command}",
                        timeout.as_millis()
                    ));
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(error) => return ToolOutcome::fail(format!("等待命令失败：{error}")),
        }
    }
    let output = match child.wait_with_output() {
        Ok(output) => output,
        Err(error) => return ToolOutcome::fail(format!("读取命令输出失败：{error}")),
    };
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut text = format!("exit={code}\n");
    if !stdout.trim().is_empty() {
        text.push_str("--- stdout ---\n");
        text.push_str(stdout.trim_end());
        text.push('\n');
    }
    if !stderr.trim().is_empty() {
        text.push_str("--- stderr ---\n");
        text.push_str(stderr.trim_end());
        text.push('\n');
    }
    let text = clamp(&text);
    if output.status.success() {
        ToolOutcome::ok(text)
    } else {
        ToolOutcome::fail(text)
    }
}

/// 递归收集文件（有深度与跳过规则，不跟符号链接）。
fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 3 || out.len() >= LIST_ENTRIES * 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut items: Vec<_> = entries.filter_map(Result::ok).collect();
    items.sort_by_key(|entry| entry.file_name());
    for entry in items {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(file_type) = entry.file_type() else { continue };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            walk(&path, depth + 1, out);
        } else if file_type.is_file() {
            out.push(path);
        }
    }
}

/// 极简通配：只支持 `*` 与 `?`。
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while ti < text.len() {
        if pi < pattern.len() && (pattern[pi] == '?' || pattern[pi] == text[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pattern.len() && pattern[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(star_index) = star {
            pi = star_index + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == '*' {
        pi += 1;
    }
    pi == pattern.len()
}

fn clamp(text: &str) -> String {
    if text.chars().count() <= MAX_OUTPUT_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX_OUTPUT_CHARS).collect();
    format!("{head}\n…（输出过长，已截断）")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("prism-native-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("建临时目录");
        path
    }

    #[test]
    fn specs_cover_six_tools() {
        let names: Vec<String> = specs()
            .iter()
            .filter_map(|spec| spec["function"]["name"].as_str().map(str::to_string))
            .collect();
        assert_eq!(
            names,
            vec![
                "read_file",
                "list_files",
                "grep",
                "write_file",
                "edit_file",
                "run_command"
            ]
        );
    }

    #[test]
    fn read_write_round_trip_has_line_numbers() {
        let dir = temp_dir("rw");
        let root = dir.to_string_lossy().to_string();
        write_file(&dir, &json!({ "path": "a.txt", "content": "第一行\n第二行\n" }));
        let outcome = execute(&root, "read_file", r#"{"path":"a.txt"}"#);
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        assert!(outcome.text.contains("1→第一行"), "{}", outcome.text);
        assert!(outcome.text.contains("2→第二行"), "{}", outcome.text);
    }

    #[test]
    fn edit_requires_unique_match() {
        let dir = temp_dir("edit");
        let root = dir.to_string_lossy().to_string();
        execute(
            &root,
            "write_file",
            r#"{"path":"b.txt","content":"x\nx\n"}"#,
        );
        let outcome = execute(
            &root,
            "edit_file",
            r#"{"path":"b.txt","old_string":"x","new_string":"y"}"#,
        );
        assert!(outcome.error.is_some());
        assert!(outcome.text.contains("2 次"), "{}", outcome.text);

        let outcome = execute(
            &root,
            "edit_file",
            r#"{"path":"b.txt","old_string":"x","new_string":"y","replace_all":true}"#,
        );
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "y\ny\n");
    }

    #[test]
    fn paths_outside_workspace_are_rejected() {
        let dir = temp_dir("guard");
        let root = dir.to_string_lossy().to_string();
        let outcome = execute(&root, "read_file", r#"{"path":"../../etc/hosts"}"#);
        assert!(outcome.error.is_some());
        assert!(outcome.text.contains("越出"), "{}", outcome.text);
    }

    #[test]
    fn run_command_reports_exit_code() {
        let dir = temp_dir("cmd");
        let root = dir.to_string_lossy().to_string();
        let outcome = execute(&root, "run_command", r#"{"command":"echo hi"}"#);
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        assert!(outcome.text.contains("exit=0"), "{}", outcome.text);
        assert!(outcome.text.contains("hi"), "{}", outcome.text);
    }

    #[test]
    fn wildcard_matches_simple_patterns() {
        assert!(wildcard_match("*.rs", "main.rs"));
        assert!(!wildcard_match("*.rs", "main.ts"));
        assert!(wildcard_match("a?c", "abc"));
        assert!(wildcard_match("*", "anything"));
    }
}
