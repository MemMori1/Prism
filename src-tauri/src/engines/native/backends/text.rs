//! `prism.text.extract`：清洗 / 切块 / 摘要伪 C 文本。
//!
//! 它**不吃反汇编器**：输入是用户贴的伪 C、上一轮 `pseudocode` 制品，或
//! Ghidra/IDA 导出的 `.c`。所以 Ghidra 没装也能用——这是它和 B 桶其他
//! 三个工具最大的区别。
//!
//! 三个 op：
//!
//! | op | 做什么 | 上限 |
//! |---|---|---|
//! | `strip` | 去块注释、行首地址注释，规范化 `DAT_` / `undefined4` | 64 MiB 输入 |
//! | `chunk` | 按 `// Function:` 或函数签名切块，每块 ≤ 4 KiB | 500 块 |
//! | `summary` | 每函数首 20 行 + 调用清单 | 200 函数 |
//!
//! **`strip` 不动控制流**：只删注释/地址前缀、只改类型别名与符号前缀。
//! 删出语法错误的伪 C 比不删更糟——模型会拿着坏代码编结论。

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};

use super::{job_dir, stage_sample, JobDir};
use crate::dag::artifact::{ArtifactStore, PutRequest};
use crate::engines::native::forensics::read_head;
use crate::engines::native::tools::{
    builtin_full, Network, Permission, Risk, ToolContext, ToolDef, ToolMeta, ToolOutcome, ToolResult,
    ToolScope,
};

/// 输入上限：64 MiB。再大就不该整份进内存。
const MAX_INPUT: u64 = 64 * 1024 * 1024;
/// 单块上限（字节）。
const CHUNK_BYTES: usize = 4 * 1024;
/// 最多切多少块。
const MAX_CHUNKS: usize = 500;
/// 摘要最多覆盖多少个函数。
const MAX_FUNCTIONS: usize = 200;
/// 摘要每函数保留多少行。
const SUMMARY_LINES: usize = 20;
/// 摘要里最多列多少个调用。
const MAX_CALLS: usize = 64;
/// `strip` 一次返回多少字符；完整文本在缓存/制品里。
const STRIP_PREVIEW: usize = 64 * 1024;
/// 切块结果的总字符预算：500 块 × 4 KiB 太大，模型上下文装不下。
const TOTAL_BUDGET: usize = 512 * 1024;

pub fn tools() -> Vec<Arc<ToolDef>> {
    vec![builtin_full(
        "prism.text.extract",
        "伪 C 文本清洗",
        schema,
        Permission::ReadOnly,
        None,
        None,
        "",
        true,
        ToolMeta {
            tags: &["bin:text", "bin:decompile"],
            risk: Risk::Read,
            network: Network::None,
            sandbox: None,
            scope: ToolScope::Any,
            needs: &[],
        },
        run,
    )]
}

fn schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "prism.text.extract",
            "description": "清洗 / 切块 / 摘要伪 C 文本。输入是文本文件（.c / .txt）或 artifact:<id>，不是二进制。strip 只删注释与地址前缀、规范化 DAT_/undefined4，不改控制流。",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "文本文件路径（相对工作目录），或 DAG 制品引用 artifact:<id>"
                    },
                    "op": {
                        "type": "string",
                        "enum": ["strip", "chunk", "summary"],
                        "description": "strip=清洗全文，chunk=按函数切块，summary=每函数首 20 行 + 调用清单；默认 summary"
                    }
                },
                "required": ["path"]
            }
        }
    })
}

fn run(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let raw = args
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(raw) = raw else {
        return ToolResult::fail("缺少 path");
    };
    let op = args
        .get("op")
        .and_then(Value::as_str)
        .unwrap_or("summary")
        .trim()
        .to_ascii_lowercase();
    if !matches!(op.as_str(), "strip" | "chunk" | "summary") {
        return ToolResult::fail(format!(
            "不支持的 op：{op}（只支持 strip / chunk / summary）"
        ));
    }

    let job: JobDir = match job_dir("text") {
        Ok(job) => job,
        Err(error) => return ToolResult::fail(error),
    };
    let sample = match stage_sample(ctx, raw, &job, MAX_INPUT) {
        Ok(sample) => sample,
        Err(error) => return ToolResult::fail(error),
    };
    let bytes = match read_head(&sample.path, MAX_INPUT) {
        Ok(bytes) => bytes,
        Err(error) => return ToolResult::fail(error),
    };
    let input_truncated = sample.size > MAX_INPUT;

    // 文本工具只处理 UTF-8。不是 UTF-8 就按 lossy 读，并在结果里如实标，
    // 不假装"读到了完整文本"。
    let (text, encoding_note) = match String::from_utf8(bytes) {
        Ok(text) => (text, None),
        Err(error) => {
            let note = format!(
                "输入不是合法 UTF-8（{}），已按替换字符读取；偏移/字符数可能不准。",
                error.utf8_error()
            );
            (String::from_utf8_lossy(error.as_bytes()).into_owned(), Some(note))
        }
    };
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let binary_note = if text.chars().take(4096).any(|ch| ch == '\0') {
        Some("输入里有 NUL 字节：这可能是二进制文件，不是伪 C 文本。".to_string())
    } else {
        None
    };

    let (cleaned, dropped) = strip(&text);
    let mut truncated = input_truncated;
    let mut notes: Vec<String> = Vec::new();
    if input_truncated {
        notes.push(format!(
            "文件 {:.1} MiB，只读了前 {} MiB。",
            sample.size as f64 / (1024.0 * 1024.0),
            MAX_INPUT / (1024 * 1024)
        ));
    }
    if let Some(note) = encoding_note {
        notes.push(note);
    }
    if let Some(note) = binary_note {
        notes.push(note);
    }

    let (chunks, full_text, op_truncated, op_note) = match op.as_str() {
        "strip" => {
            let preview: String = cleaned.chars().take(STRIP_PREVIEW).collect();
            let cut = cleaned.chars().count() > STRIP_PREVIEW;
            if cut {
                truncated = true;
            }
            (
                vec![Chunk {
                    name: sample.name.clone(),
                    va: None,
                    text: preview,
                    calls: Vec::new(),
                    lines: cleaned.lines().count(),
                }],
                cleaned.clone(),
                cut,
                cut.then(|| {
                    format!(
                        "清洗后的文本有 {} 字符，只返回前 {STRIP_PREVIEW}；完整文本在缓存/制品里。",
                        cleaned.chars().count()
                    )
                }),
            )
        }
        "chunk" => {
            let functions = split_functions(&cleaned);
            let (chunks, cut) = chunk_functions(functions);
            let full = chunks_text_chunks(&chunks);
            (chunks, full, cut, None)
        }
        _ => {
            let functions = split_functions(&cleaned);
            let (chunks, cut) = summarize(functions);
            let full = chunks_text_chunks(&chunks);
            (chunks, full, cut, None)
        }
    };

    let mut total_chars = 0usize;
    let mut total_lines = 0usize;
    let mut budgeted: Vec<Value> = Vec::new();
    let mut budget_cut = false;
    for chunk in chunks {
        if total_chars >= TOTAL_BUDGET {
            budget_cut = true;
            break;
        }
        let remaining = TOTAL_BUDGET - total_chars;
        let text: String = if chunk.text.chars().count() > remaining {
            budget_cut = true;
            chunk.text.chars().take(remaining).collect()
        } else {
            chunk.text.clone()
        };
        total_chars += text.chars().count();
        total_lines += chunk.lines;
        let mut item = json!({
            "name": chunk.name,
            "va": chunk.va.map(|va| format!("0x{va:x}")),
            "text": text,
            "lines": chunk.lines,
        });
        if !chunk.calls.is_empty() {
            item["calls"] = json!(chunk.calls);
        }
        budgeted.push(item);
    }
    if budget_cut {
        truncated = true;
        notes.push(format!(
            "切块结果超过 {TOTAL_BUDGET} 字符预算，已截断；完整文本在缓存/制品里。"
        ));
    }
    if let Some(note) = op_note {
        notes.push(note);
    }
    if op_truncated {
        truncated = true;
    }
    if op == "chunk" && op_truncated {
        notes.push(format!("函数超过 {MAX_CHUNKS} 块，只保留前 {MAX_CHUNKS} 块。"));
    }
    if op == "summary" && op_truncated {
        notes.push(format!("函数超过 {MAX_FUNCTIONS} 个，只摘要前 {MAX_FUNCTIONS} 个。"));
    }


    let source = json!({
        "input": raw,
        "path": sample
            .origin_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| sample.path.display().to_string()),
        "name": sample.name,
        "size": sample.size,
        "sha256": sample.sha256,
        "artifact": sample.artifact,
    });

    let mut payload = json!({
        "ok": true,
        "op": op,
        "source": source,
        "chunks": budgeted,
        "chunkCount": budgeted.len(),
        "chars": total_chars,
        "lines": total_lines,
        "dropped": {
            "comments": dropped.comments,
            "lines": dropped.lines,
        },
        "truncated": truncated,
        "note": join_notes(&notes),
    });

    // 聊天模式：完整文本 + JSON 都落缓存；DAG 里把文本落成 key=text 制品。
    let cache = write_cache(&sample.sha256, &op, &full_text, &payload);
    payload["wrote"] = json!(cache
        .as_ref()
        .map(|(text_path, _)| format!("缓存（{}）", text_path.display()))
        .unwrap_or_else(|| "缓存不可用".to_string()));

    if let Some(scope) = ctx.dag {
        match ArtifactStore::global() {
            Ok(store) => {
                let file_name = format!("{}.{op}.txt", file_stem(&sample.name));
                match store.put(PutRequest {
                    dag_id: &scope.dag_id,
                    node_id: &scope.node_id,
                    key: "text",
                    name: &file_name,
                    kind: "text",
                    parent_id: sample.artifact.as_deref(),
                    bytes: full_text.as_bytes(),
                }) {
                    Ok(reference) => {
                        payload["artifact"] = json!(reference.id);
                        payload["wrote"] =
                            json!(format!("制品库（key=text，{} 字节）", reference.size));
                    }
                    Err(error) => payload["artifactError"] = json!(error),
                }
            }
            Err(error) => payload["artifactError"] = json!(error),
        }
    }

    ToolResult::plain(ToolOutcome::ok(
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()),
    ))
}

// ---------------------------------------------------------------- 数据结构

#[derive(Debug, Clone, PartialEq, Eq)]
struct Dropped {
    comments: usize,
    lines: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Chunk {
    name: String,
    va: Option<u64>,
    text: String,
    calls: Vec<String>,
    lines: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FunctionText {
    name: String,
    va: Option<u64>,
    text: String,
}

// ---------------------------------------------------------------- strip

/// 清洗：去块注释、行首地址注释；规范化 Ghidra 类型/符号前缀。
///
/// 块注释替换成等量换行，保持行号；`// Function:` 等普通行注释保留，
/// 因为 `chunk` 要靠它切块。
fn strip(text: &str) -> (String, Dropped) {
    let mut without_blocks = String::with_capacity(text.len());
    let mut comments = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            comments += 1;
            let mut previous = '\0';
            for inner in chars.by_ref() {
                if inner == '\n' {
                    without_blocks.push('\n');
                }
                if previous == '*' && inner == '/' {
                    break;
                }
                previous = inner;
            }
        } else {
            without_blocks.push(ch);
        }
    }

    let mut out = String::with_capacity(without_blocks.len());
    let mut lines = 0usize;
    for line in without_blocks.lines() {
        if is_address_comment(line.trim()) {
            lines += 1;
            continue;
        }
        out.push_str(&normalize_names(line));
        out.push('\n');
    }
    (
        out,
        Dropped {
            comments,
            lines,
        },
    )
}

/// 行首地址注释：`// 00401000`、`// 0x401000`、`// 401000`。
fn is_address_comment(trimmed: &str) -> bool {
    let Some(rest) = trimmed.strip_prefix("//") else {
        return false;
    };
    let body = rest.trim();
    let body = body
        .strip_prefix("0x")
        .or_else(|| body.strip_prefix("0X"))
        .unwrap_or(body);
    body.len() >= 4 && body.chars().all(|ch| ch.is_ascii_hexdigit())
}

/// 只改标识符，不碰字符串/字符字面量里的内容。
fn normalize_names(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '"' || ch == '\'' {
            let quote = ch;
            out.push(ch);
            while let Some(inner) = chars.next() {
                out.push(inner);
                if inner == '\\' {
                    if let Some(escaped) = chars.next() {
                        out.push(escaped);
                    }
                } else if inner == quote {
                    break;
                }
            }
            continue;
        }
        if ch.is_ascii_alphabetic() || ch == '_' {
            let mut token = String::new();
            token.push(ch);
            while let Some(next) = chars.peek() {
                if next.is_ascii_alphanumeric() || *next == '_' {
                    token.push(chars.next().unwrap_or_default());
                } else {
                    break;
                }
            }
            out.push_str(&normalize_token(&token));
        } else {
            out.push(ch);
        }
    }
    out
}

fn normalize_token(token: &str) -> String {
    match token {
        "undefined8" => "u64".to_string(),
        "undefined4" => "u32".to_string(),
        "undefined2" => "u16".to_string(),
        "undefined1" => "u8".to_string(),
        "undefined" => "u8".to_string(),
        _ => {
            if let Some(rest) = token.strip_prefix("DAT_") {
                format!("data_{rest}")
            } else if let Some(rest) = token.strip_prefix("LAB_") {
                format!("label_{rest}")
            } else if let Some(rest) = token.strip_prefix("UNK_") {
                format!("unk_{rest}")
            } else {
                token.to_string()
            }
        }
    }
}

// ---------------------------------------------------------------- 切函数

fn split_functions(text: &str) -> Vec<FunctionText> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<FunctionText> = Vec::new();
    let mut current: Option<FunctionText> = None;

    let flush = |current: &mut Option<FunctionText>, out: &mut Vec<FunctionText>| {
        if let Some(chunk) = current.take() {
            if !chunk.text.trim().is_empty() {
                out.push(chunk);
            }
        }
    };

    let mut index = 0usize;
    while index < lines.len() {
        let line = lines[index];
        if let Some((name, va)) = function_marker(line) {
            flush(&mut current, &mut out);
            let mut chunk = FunctionText {
                name,
                va,
                text: String::new(),
            };
            chunk.text.push_str(line);
            chunk.text.push('\n');
            current = Some(chunk);
            index += 1;
            continue;
        }
        if let Some((name, va)) = signature_at(&lines, index) {
            // marker 行（`// Function: name`）后面紧跟的签名行是同一函数的第二行，
            // 不是新的函数边界。同名就归入当前 chunk，别把 marker 和函数体切两半。
            let same_as_current = current
                .as_ref()
                .map(|chunk| chunk.name == name)
                .unwrap_or(false);
            if !same_as_current {
                flush(&mut current, &mut out);
                current = Some(FunctionText {
                    name,
                    va,
                    text: String::new(),
                });
            }
        }
        match current.as_mut() {
            Some(chunk) => {
                chunk.text.push_str(line);
                chunk.text.push('\n');
            }
            None => {
                if !line.trim().is_empty() {
                    let mut chunk = FunctionText {
                        name: "preamble".to_string(),
                        va: None,
                        text: String::new(),
                    };
                    chunk.text.push_str(line);
                    chunk.text.push('\n');
                    current = Some(chunk);
                }
            }
        }
        index += 1;
    }
    flush(&mut current, &mut out);
    out
}

/// `// Function: name @ 0x401000`（Ghidra）或 `// Function: name`。
fn function_marker(line: &str) -> Option<(String, Option<u64>)> {
    let rest = line.trim().strip_prefix("// Function:")?.trim();
    if rest.is_empty() {
        return None;
    }
    if let Some((name, address)) = rest.rsplit_once(" @ ") {
        let va = parse_address(address.trim());
        return Some((name.trim().to_string(), va));
    }
    Some((rest.to_string(), None))
}

/// 函数签名行：`undefined4 FUN_00401000(int param_1)` / `int main(...) {`。
///
/// 只在下一非空行是 `{`（或本行以 `{` 结尾）时认，避免把普通调用当函数。
fn signature_at(lines: &[&str], index: usize) -> Option<(String, Option<u64>)> {
    let line = lines[index].trim();
    if line.is_empty()
        || line.starts_with("//")
        || line.starts_with('#')
        || line.starts_with('}')
        || line.starts_with('{')
        || !line.contains('(')
        || (!line.trim_end().ends_with(')')
            && !line.trim_end().ends_with(") {")
            && !line.trim_end().ends_with("){"))
    {
        return None;
    }
    let next = lines[index + 1..]
        .iter()
        .map(|line| line.trim())
        .find(|line| !line.is_empty());
    let has_body = line.ends_with('{') || next == Some("{");
    if !has_body {
        return None;
    }
    let before = line.split('(').next()?.trim();
    let name = before.split_whitespace().last()?.trim_matches('*');
    if name.is_empty() || is_keyword(name) {
        return None;
    }
    Some((name.to_string(), va_from_name(name)))
}

fn is_keyword(name: &str) -> bool {
    matches!(
        name,
        "if" | "for" | "while" | "switch" | "return" | "sizeof" | "do" | "else" | "catch"
    )
}

fn va_from_name(name: &str) -> Option<u64> {
    for prefix in ["FUN_", "sub_", "thunk_FUN_", "j_"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            if let Some(va) = parse_address(rest) {
                return Some(va);
            }
        }
    }
    None
}

fn parse_address(text: &str) -> Option<u64> {
    let text = text.trim();
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        u64::from_str_radix(text, 16)
            .ok()
            .or_else(|| text.parse().ok())
    }
}

// ---------------------------------------------------------------- chunk / summary

fn chunk_functions(functions: Vec<FunctionText>) -> (Vec<Chunk>, bool) {
    let mut out: Vec<Chunk> = Vec::new();
    let mut truncated = false;
    for function in functions {
        let lines = function.text.lines().count();
        let parts = split_bytes(&function.text, CHUNK_BYTES);
        let total = parts.len();
        for (index, part) in parts.into_iter().enumerate() {
            if out.len() >= MAX_CHUNKS {
                truncated = true;
                break;
            }
            let name = if total <= 1 {
                function.name.clone()
            } else {
                format!("{}#{}", function.name, index + 1)
            };
            out.push(Chunk {
                name,
                va: function.va,
                text: part,
                calls: Vec::new(),
                lines,
            });
        }
        if truncated {
            break;
        }
    }
    (out, truncated)
}

/// 按字节上限切块，优先在换行处切；单行超长时按字符边界硬切。
fn split_bytes(text: &str, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        let needed = line.len() + 1;
        if !current.is_empty() && current.len() + needed > limit {
            out.push(std::mem::take(&mut current));
        }
        if needed <= limit {
            current.push_str(line);
            current.push('\n');
            continue;
        }
        // 单行超过上限：按字符边界硬切。
        for ch in line.chars() {
            if current.len() + ch.len_utf8() > limit && !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            current.push(ch);
        }
        current.push('\n');
    }
    if !current.is_empty() {
        out.push(current);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn summarize(functions: Vec<FunctionText>) -> (Vec<Chunk>, bool) {
    let truncated = functions.len() > MAX_FUNCTIONS;
    let chunks = functions
        .into_iter()
        .take(MAX_FUNCTIONS)
        .map(|function| {
            let lines: Vec<&str> = function.text.lines().collect();
            let mut text = lines
                .iter()
                .take(SUMMARY_LINES)
                .copied()
                .collect::<Vec<_>>()
                .join("\n");
            let calls = extract_calls(&function.text);
            if !calls.is_empty() {
                text.push_str("\n// calls: ");
                text.push_str(&calls.join(", "));
            }
            Chunk {
                name: function.name,
                va: function.va,
                text,
                calls,
                lines: lines.len(),
            }
        })
        .collect();
    (chunks, truncated)
}

/// 从伪 C 里抽 `name(` 形式的调用。跳过字符串与 `//` 注释，去掉关键字与
/// Ghidra 的内建宏（`LOBYTE` 之类不是 API）。
fn extract_calls(text: &str) -> Vec<String> {
    let mut calls: Vec<String> = Vec::new();
    let mut chars = text.chars().peekable();
    let mut in_line_comment = false;
    while let Some(ch) = chars.next() {
        if in_line_comment {
            if ch == '\n' {
                in_line_comment = false;
            }
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'/') {
            in_line_comment = true;
            continue;
        }
        if ch == '"' || ch == '\'' {
            let quote = ch;
            while let Some(inner) = chars.next() {
                if inner == '\\' {
                    chars.next();
                } else if inner == quote {
                    break;
                }
            }
            continue;
        }
        if ch.is_ascii_alphabetic() || ch == '_' {
            let mut token = String::new();
            token.push(ch);
            while let Some(next) = chars.peek() {
                if next.is_ascii_alphanumeric() || *next == '_' {
                    token.push(chars.next().unwrap_or_default());
                } else {
                    break;
                }
            }
            // 允许 `foo (` 之间有一个空格。
            let mut lookahead = chars.clone();
            while lookahead.peek() == Some(&' ') {
                lookahead.next();
            }
            if lookahead.peek() == Some(&'(')
                && !is_keyword(&token)
                && !is_noise_call(&token)
                && !calls.iter().any(|existing| existing == &token)
            {
                calls.push(token);
                if calls.len() >= MAX_CALLS {
                    break;
                }
            }
        }
    }
    calls
}

fn is_noise_call(name: &str) -> bool {
    matches!(
        name,
        "LOBYTE"
            | "HIBYTE"
            | "LOWORD"
            | "HIWORD"
            | "LODWORD"
            | "HIDWORD"
            | "CONCAT44"
            | "CONCAT31"
            | "CONCAT22"
            | "CONCAT13"
            | "SUB84"
            | "SBORROW4"
            | "CARRY4"
            | "OVERFLOW4"
            | "ZEXT48"
            | "SEXT48"
    )
}

// ---------------------------------------------------------------- 输出

fn chunks_text_chunks(chunks: &[Chunk]) -> String {
    let mut out = String::new();
    for chunk in chunks {
        if let Some(va) = chunk.va {
            out.push_str(&format!("// Function: {} @ 0x{va:x}\n", chunk.name));
        } else {
            out.push_str(&format!("// Function: {}\n", chunk.name));
        }
        out.push_str(&chunk.text);
        out.push_str("\n\n");
    }
    out
}

fn join_notes(notes: &[String]) -> Option<String> {
    (!notes.is_empty()).then(|| notes.join(" "))
}

fn file_stem(name: &str) -> String {
    let stem = PathBuf::from(name)
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| "text".to_string());
    let safe: String = stem
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .take(60)
        .collect();
    if safe.is_empty() {
        "text".to_string()
    } else {
        safe
    }
}

fn write_cache(
    sha256: &str,
    op: &str,
    text: &str,
    payload: &Value,
) -> Option<(PathBuf, PathBuf)> {
    let dir = super::index::cache_root()?.join(sha256).join("text");
    std::fs::create_dir_all(&dir).ok()?;
    let text_path = dir.join(format!("{op}.txt"));
    let json_path = dir.join(format!("{op}.json"));
    std::fs::write(&text_path, text.as_bytes()).ok()?;
    std::fs::write(
        &json_path,
        serde_json::to_string_pretty(payload).unwrap_or_default(),
    )
    .ok()?;
    Some((text_path, json_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_removes_warning_blocks_and_address_lines() {
        let text = "/* WARNING: Unknown calling convention */\n\
                    int main(void)\n\
                    {\n\
                      // 00401000\n\
                      undefined4 value = DAT_00401020;\n\
                      return value;\n\
                    }\n";
        let (cleaned, dropped) = strip(text);
        assert_eq!(dropped.comments, 1);
        assert_eq!(dropped.lines, 1);
        assert!(!cleaned.contains("WARNING"));
        assert!(!cleaned.contains("00401000\n"));
        assert!(cleaned.contains("u32 value = data_00401020;"));
        assert!(cleaned.contains("int main(void)"));
        assert!(cleaned.contains("return value;"));
    }

    #[test]
    fn strip_does_not_touch_string_literals() {
        let text = "char *s = \"DAT_00401000 undefined4\";\n";
        let (cleaned, _) = strip(text);
        assert!(cleaned.contains("\"DAT_00401000 undefined4\""));
    }

    #[test]
    fn strip_keeps_line_numbers_when_removing_multiline_comments() {
        let text = "int a;\n/* one\ntwo */\nint b;\n";
        let (cleaned, dropped) = strip(text);
        assert_eq!(dropped.comments, 1);
        assert_eq!(cleaned.lines().count(), 4);
        assert!(cleaned.contains("int a;"));
        assert!(cleaned.contains("int b;"));
    }

    #[test]
    fn function_markers_and_signatures_split_into_functions() {
        let text = "// Function: alpha @ 0x401000\nint alpha(void)\n{\n  return 1;\n}\n\
                    int beta(int x) {\n  return x;\n}\n";
        let functions = split_functions(text);
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "alpha");
        assert_eq!(functions[0].va, Some(0x401000));
        assert_eq!(functions[1].name, "beta");
        assert!(functions[1].text.contains("return x;"));
    }

    #[test]
    fn chunk_splits_long_functions_at_four_kib() {
        let mut text = String::from("// Function: big @ 0x5000\nvoid big(void)\n{\n");
        for index in 0..500 {
            text.push_str(&format!("  int v{index} = {index};\n"));
        }
        text.push_str("}\n");
        let (chunks, truncated) = chunk_functions(split_functions(&text));
        assert!(!truncated);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.text.len() <= CHUNK_BYTES, "块超过 4 KiB");
        }
        assert!(chunks[1].name.starts_with("big#"));
    }

    #[test]
    fn summary_keeps_twenty_lines_and_extracts_calls() {
        let mut text = String::from("// Function: worker @ 0x6000\nint worker(void)\n{\n");
        for index in 0..40 {
            text.push_str(&format!("  step{index}();\n"));
        }
        text.push_str("  return RegOpenKeyExA(0);\n}\n");
        let (chunks, truncated) = summarize(split_functions(&text));
        assert!(!truncated);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text.lines().count(), SUMMARY_LINES + 1);
        assert!(chunks[0].calls.iter().any(|call| call == "RegOpenKeyExA"));
        assert!(!chunks[0].calls.iter().any(|call| call == "if"));
    }

    #[test]
    fn call_extraction_skips_comments_and_ghidra_noise() {
        let calls = extract_calls(
            "// FakeCall(1)\nint x = LOBYTE(y) + real_api(z);\nchar *s = \"NotACall()\";\n",
        );
        assert!(calls.contains(&"real_api".to_string()));
        assert!(!calls.contains(&"FakeCall".to_string()));
        assert!(!calls.contains(&"LOBATE".to_string()));
        assert!(!calls.contains(&"NotACall".to_string()));
    }

    #[test]
    fn address_comment_detection_is_strict() {
        assert!(is_address_comment("// 00401000"));
        assert!(is_address_comment("// 0x401000"));
        assert!(!is_address_comment("// 401"));
        assert!(!is_address_comment("// not an address"));
        assert!(!is_address_comment("int x;"));
    }

    #[test]
    fn tool_is_registered_and_read_only() {
        let tools = tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name.as_ref(), "prism.text.extract");
        assert_eq!(tools[0].permission, Permission::ReadOnly);
        assert_eq!(tools[0].meta.risk, Risk::Read);
    }
}
