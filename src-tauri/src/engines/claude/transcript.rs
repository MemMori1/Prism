//! Claude Code 会话档案（`~/.claude/projects/<编码 cwd>/<会话 uuid>.jsonl`）解析。
//!
//! 格式（claude-code 观察值，解析层保持宽容）：
//! - `user`：真实用户输入（content 为字符串或 text 块数组），
//!   或工具结果回填（content[] 里是 `tool_result` 块，不切记录）；
//! - `assistant`：正文 / 思考 / 工具调用（content[] 里是 `text | thinking | tool_use`）；
//! - `summary` / `ai-title` / `custom-title`：标题元数据（时间线跳过，列表用）；
//! - `file-history-snapshot` 等大块非消息行：直接跳过。
//!
//! 记录切分与 Codex 一致：真实用户消息开一条用户记录；两条用户消息之间的
//! 所有 assistant 侧条目合并成一条助手记录（parts 保序）；工具结果只回填
//! 状态（`is_error` → error，否则 completed），不切记录。
//! 子代理（`isSidechain`）与元消息（`isMeta`）不进主时间线。

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::locator::{self, ENGINE_ID};
use crate::engines::core::{
    ProjectItem, SessionSummary, TimelinePage, TimelinePart, TimelineRecord, ToolDetail,
    TurnTokens,
};
use crate::engines::time::{file_modified_ms, parse_iso_ms};

type ParseResult<T> = Result<T, String>;

/// Claude Code 会话 id 在 Prism 内的命名空间前缀。
pub const SESSION_PREFIX: &str = "claude:";
/// Claude Code 项目 id 前缀（项目 = 编码目录名）。
pub const PROJECT_PREFIX: &str = "claude:";

/// 工具详情输出上限（字符），超出截断。
const TOOL_OUTPUT_LIMIT: usize = 200_000;
/// 工具摘要最大长度（字符）。
const SUMMARY_LIMIT: usize = 80;
/// 标题最大长度（字符）。
const TITLE_LIMIT: usize = 80;

pub fn session_id_for(uuid: &str) -> String {
    format!("{SESSION_PREFIX}{uuid}")
}

pub fn is_claude_session(session_id: &str) -> bool {
    session_id.starts_with(SESSION_PREFIX)
}

pub fn session_uuid_of(session_id: &str) -> Option<&str> {
    session_id.strip_prefix(SESSION_PREFIX)
}

pub fn is_claude_project(project_id: &str) -> bool {
    project_id.starts_with(PROJECT_PREFIX)
}

pub fn dir_name_of(project_id: &str) -> Option<&str> {
    project_id.strip_prefix(PROJECT_PREFIX)
}

pub fn project_id_for(dir_name: &str) -> String {
    format!("{PROJECT_PREFIX}{dir_name}")
}

/// 目录名 / 文件名的安全校验（防 `..` 与路径分隔符逃逸）。
fn is_safe_component(name: &str) -> bool {
    !name.is_empty()
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
}

fn project_name(worktree: &str) -> String {
    Path::new(worktree)
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| worktree.to_string())
}

/// 目录内的会话档案（排除子代理的 `agent-*.jsonl`）。
fn session_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .filter(|path| {
            !path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .starts_with("agent-")
        })
        .filter(|path| path.is_file())
        .collect();
    files.sort();
    files
}

/// 项目列表：扫描档案根目录，按最近活动排序（空目录不出现）。
pub fn list_projects(root: &Path) -> ParseResult<Vec<ProjectItem>> {
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut items: Vec<ProjectItem> = Vec::new();
    let entries = fs::read_dir(root).map_err(|cause| format!("读取 Claude 档案目录失败：{cause}"))?;
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(dir_name) = dir.file_name().and_then(|name| name.to_str()).map(str::to_string)
        else {
            continue;
        };
        let files = session_files(&dir);
        if files.is_empty() {
            continue;
        }
        let time_updated = files
            .iter()
            .map(|path| file_modified_ms(path))
            .max()
            .unwrap_or(0);
        let worktree = project_worktree(&dir_name, &files);
        items.push(ProjectItem {
            engine: ENGINE_ID.to_string(),
            id: project_id_for(&dir_name),
            name: project_name(&worktree),
            worktree,
            session_count: files.len() as i64,
            time_updated,
        });
    }
    items.sort_by(|a, b| b.time_updated.cmp(&a.time_updated));
    Ok(items)
}

/// 项目真实路径：优先采信最新档案里的 cwd（编码回目录名一致才认），
/// 否则退回有损解码（编码不可逆、只能尽力还原）。
fn project_worktree(dir_name: &str, files: &[PathBuf]) -> String {
    let mut ordered: Vec<&PathBuf> = files.iter().collect();
    ordered.sort_by_key(|path| std::cmp::Reverse(file_modified_ms(path)));
    for path in ordered {
        if let Some(cwd) = cwd_from_head(path) {
            if locator::cwd_matches_dir(&cwd, dir_name) {
                return cwd;
            }
        }
    }
    locator::decode_dir_name(dir_name)
}

/// 档案开头几行里取第一条非空 cwd。
fn cwd_from_head(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines().take(20).flatten() {
        if !line.contains("\"cwd\"") {
            continue;
        }
        let value: Value = serde_json::from_str(&line).ok()?;
        if let Some(cwd) = value
            .get("cwd")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
        {
            return Some(cwd.to_string());
        }
    }
    None
}

/// 某项目下的会话（按最近活动排序）。
pub fn list_sessions(root: &Path, dir_name: &str, limit: i64) -> ParseResult<Vec<SessionSummary>> {
    if !is_safe_component(dir_name) {
        return Err("非法的 Claude 项目 id".to_string());
    }
    let dir = root.join(dir_name);
    let mut summaries: Vec<SessionSummary> = session_files(&dir)
        .into_iter()
        .filter_map(|path| summarize_file(&path, dir_name))
        .collect();
    summaries.sort_by(|a, b| b.time_updated.cmp(&a.time_updated));
    summaries.truncate(limit.max(0) as usize);
    Ok(summaries)
}

/// 按会话 uuid 读取单个概要（工作台恢复列时用）。
pub fn session_summary(root: &Path, uuid: &str) -> ParseResult<Option<SessionSummary>> {
    let Some((dir_name, path)) = find_session_file(root, uuid) else {
        return Ok(None);
    };
    Ok(summarize_file(&path, &dir_name))
}

/// 会话 uuid → 档案文件（扫描各项目目录；uuid 必须为合法路径片段）。
pub fn find_session_file(root: &Path, uuid: &str) -> Option<(String, PathBuf)> {
    if !is_safe_component(uuid) {
        return None;
    }
    let entries = fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(dir_name) = dir.file_name().and_then(|name| name.to_str()).map(str::to_string)
        else {
            continue;
        };
        let candidate = dir.join(format!("{uuid}.jsonl"));
        if candidate.is_file() {
            return Some((dir_name, candidate));
        }
    }
    None
}

// ---------------------------------------------------------------- 会话概要

/// 单遍扫描出的会话概要原料。
#[derive(Default)]
struct FileScan {
    custom_title: Option<String>,
    ai_title: Option<String>,
    summary_title: Option<String>,
    first_user: Option<String>,
    model: Option<String>,
    cwd: Option<String>,
    first_ts: Option<i64>,
    last_ts: Option<i64>,
    /// input + cache_creation + cache_read（真实发给模型的输入量）。
    tokens_input: i64,
    tokens_output: i64,
}

/// 扫描一个会话档案，构建 SessionSummary（读全文件，后续可加文件级缓存）。
fn summarize_file(path: &Path, dir_name: &str) -> Option<SessionSummary> {
    let uuid = path.file_stem()?.to_str()?.to_string();
    let scan = scan_file(path)?;
    let worktree = scan
        .cwd
        .as_deref()
        .filter(|cwd| locator::cwd_matches_dir(cwd, dir_name))
        .map(str::to_string)
        .unwrap_or_else(|| locator::decode_dir_name(dir_name));
    let title = [scan.custom_title, scan.ai_title, scan.summary_title]
        .into_iter()
        .flatten()
        .find_map(|text| normalize_title(&text))
        .or_else(|| scan.first_user.as_deref().and_then(normalize_title));
    Some(SessionSummary {
        engine: ENGINE_ID.to_string(),
        id: session_id_for(&uuid),
        project_id: project_id_for(dir_name),
        title,
        directory: worktree,
        model: scan.model,
        cost: 0.0,
        tokens_input: scan.tokens_input,
        tokens_output: scan.tokens_output,
        tokens_reasoning: 0,
        time_created: scan.first_ts.unwrap_or(0),
        time_updated: scan.last_ts.unwrap_or_else(|| file_modified_ms(path)),
    })
}

/// 标题规整：只取首行并截断到 80 字符。
fn normalize_title(raw: &str) -> Option<String> {
    let first_line = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    if first_line.is_empty() {
        return None;
    }
    let chars: Vec<char> = first_line.chars().collect();
    if chars.len() <= TITLE_LIMIT {
        return Some(first_line.to_string());
    }
    Some(format!("{}…", chars[..TITLE_LIMIT].iter().collect::<String>()))
}

/// 单遍扫描：绝大多数行只做 contains 快筛，命中才 JSON 解析。
fn scan_file(path: &Path) -> Option<FileScan> {
    let file = File::open(path).ok()?;
    let reader = BufReader::with_capacity(64 * 1024, file);
    let mut scan = FileScan::default();
    let mut first_line: Option<String> = None;
    let mut last_line: Option<String> = None;

    for line in reader.lines() {
        let Ok(line) = line else { continue };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if first_line.is_none() {
            first_line = Some(trimmed.to_string());
        }
        last_line = Some(trimmed.to_string());

        // 大块非消息行直接跳过（避免解析巨行）。
        if trimmed.contains("file-history-snapshot") || trimmed.contains("queue-operation") {
            continue;
        }
        // 值子串粗筛（不依赖 JSON 冒号后的空格）；命中才整行解析、按 type 分发。
        let likely = trimmed.contains("\"user\"")
            || trimmed.contains("\"assistant\"")
            || trimmed.contains("\"custom-title\"")
            || trimmed.contains("\"ai-title\"")
            || trimmed.contains("\"summary\"");
        if !likely {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str) {
            Some("custom-title") => {
                scan.custom_title = value
                    .get("customTitle")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            Some("ai-title") => {
                scan.ai_title = value
                    .get("aiTitle")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            Some("summary") => {
                scan.summary_title = value
                    .get("summary")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            Some("user") => {
                note_meta(&mut scan, &value);
                let is_meta = value.get("isMeta").and_then(Value::as_bool).unwrap_or(false);
                let is_sidechain = value
                    .get("isSidechain")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if !is_meta && !is_sidechain && scan.first_user.is_none() {
                    if let Some(content) = value.pointer("/message/content") {
                        let text = user_text(content);
                        if !text.is_empty() {
                            scan.first_user = Some(text);
                        }
                    }
                }
            }
            Some("assistant") => {
                note_meta(&mut scan, &value);
                if scan.model.is_none() {
                    scan.model = value
                        .pointer("/message/model")
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                        .map(str::to_string);
                }
                // 用量含子代理（sidechain）调用：真实计费口径。
                if let Some(usage) = value.pointer("/message/usage") {
                    scan.tokens_input += usage
                        .get("input_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or(0)
                        + usage
                            .get("cache_creation_input_tokens")
                            .and_then(Value::as_i64)
                            .unwrap_or(0)
                        + usage
                            .get("cache_read_input_tokens")
                            .and_then(Value::as_i64)
                            .unwrap_or(0);
                    scan.tokens_output += usage
                        .get("output_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                }
            }
            _ => {}
        }
    }

    if scan.first_ts.is_none() {
        scan.first_ts = first_line.as_deref().and_then(extract_ts);
    }
    if scan.last_ts.is_none() {
        scan.last_ts = last_line.as_deref().and_then(extract_ts);
    }
    Some(scan)
}

/// 从已解析的记录里补充 cwd / 首末时间戳。
fn note_meta(scan: &mut FileScan, value: &Value) {
    if scan.cwd.is_none() {
        scan.cwd = value
            .get("cwd")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map(str::to_string);
    }
    let ts = value.get("timestamp").and_then(Value::as_str).and_then(parse_iso_ms);
    if scan.first_ts.is_none() {
        scan.first_ts = ts;
    }
    if ts.is_some() {
        scan.last_ts = ts;
    }
}

/// 从原始行里快速抠出 `"timestamp":"…"`（免整行 JSON 解析）。
fn extract_ts(line: &str) -> Option<i64> {
    let marker = "\"timestamp\":\"";
    let start = line.find(marker)? + marker.len();
    let end = line[start..].find('"')? + start;
    parse_iso_ms(&line[start..end])
}

// ---------------------------------------------------------------- 时间线

/// 内部解析结果：seq = 行号（1 起），游标对齐 OpenCode / Codex 的分页语义。
struct ParsedRecord {
    seq: i64,
    time_created: i64,
    role: &'static str,
    model: Option<String>,
    parts: Vec<TimelinePart>,
    /// 该轮最后一次 API 调用的用量（Claude 一个助手回合可能含多次调用）。
    tokens: Option<TurnTokens>,
}

/// 时间线分页：与 OpenCode 语义一致（`before_seq` 严格递减；正序返回）。
pub fn read_timeline(path: &Path, before_seq: Option<i64>, limit: i64) -> ParseResult<TimelinePage> {
    let file = File::open(path).map_err(|cause| format!("打开会话档案失败：{cause}"))?;
    let records = parse_reader(BufReader::new(file))?;
    Ok(render_page(records, before_seq, limit))
}

/// 纯分页逻辑（独立出来便于测试）。
fn render_page(records: Vec<ParsedRecord>, before_seq: Option<i64>, limit: i64) -> TimelinePage {
    let limit = limit.max(1) as usize;
    let mut selected: Vec<ParsedRecord> = records
        .into_iter()
        .filter(|record| before_seq.map_or(true, |cursor| record.seq < cursor))
        .collect();

    let has_more = selected.len() > limit;
    if has_more {
        selected.drain(..selected.len() - limit);
    }
    let next_cursor = if has_more {
        selected.first().map(|record| record.seq)
    } else {
        None
    };

    TimelinePage {
        records: selected
            .into_iter()
            .map(|record| TimelineRecord {
                id: record.seq.to_string(),
                role: record.role.to_string(),
                time_created: record.time_created,
                model: record.model,
                completed: true,
                parts: record.parts,
                tokens: record.tokens,
            })
            .collect(),
        next_cursor,
        has_more,
    }
}

/// 逐行解析（宽容：任何解析不出的行直接跳过）。
fn parse_reader<R: BufRead>(reader: R) -> ParseResult<Vec<ParsedRecord>> {
    let mut records: Vec<ParsedRecord> = Vec::new();
    let mut current: Option<ParsedRecord> = None;
    // call_id → (记录下标, parts 下标)；用于把工具结果回填到对应工具卡片。
    let mut call_index: HashMap<String, (usize, usize)> = HashMap::new();

    for (index, line) in reader.lines().enumerate() {
        let line = line.map_err(|cause| format!("读取会话档案失败：{cause}"))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // 大块非消息行直接跳过（避免整行 JSON 解析）。
        if trimmed.contains("file-history-snapshot") || trimmed.contains("queue-operation") {
            continue;
        }
        let seq = index as i64 + 1;
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        let time_ms = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_iso_ms)
            .unwrap_or(0);
        let kind = value.get("type").and_then(Value::as_str).unwrap_or_default();
        let is_sidechain = value
            .get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        match kind {
            "user" => {
                if is_sidechain
                    || value.get("isMeta").and_then(Value::as_bool).unwrap_or(false)
                {
                    continue;
                }
                let Some(content) = value.pointer("/message/content") else {
                    continue;
                };
                // 工具结果回填：不切记录，只更新工具 part 状态。
                if let Some(blocks) = content.as_array() {
                    let mut has_result = false;
                    let mut has_other = false;
                    for block in blocks {
                        match block.get("type").and_then(Value::as_str) {
                            Some("tool_result") => {
                                has_result = true;
                                let call_id = block
                                    .get("tool_use_id")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                let is_error = block
                                    .get("is_error")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(false);
                                let status = if is_error { "error" } else { "completed" };
                                if let Some((record_index, part_index)) =
                                    call_index.get(call_id).copied()
                                {
                                    let record = if record_index < records.len() {
                                        records.get_mut(record_index)
                                    } else {
                                        current.as_mut()
                                    };
                                    if let Some(record) = record {
                                        if let Some(TimelinePart::Tool {
                                            status: part_status,
                                            ..
                                        }) = record.parts.get_mut(part_index)
                                        {
                                            *part_status = Some(status.to_string());
                                        }
                                    }
                                }
                            }
                            Some(_) | None => has_other = true,
                        }
                    }
                    if has_result && !has_other {
                        continue;
                    }
                }
                // 真实用户消息。
                let text = user_text(content);
                if !text.is_empty() {
                    flush(&mut records, &mut current);
                    records.push(ParsedRecord {
                        seq,
                        time_created: time_ms,
                        role: "user",
                        model: None,
                        parts: vec![TimelinePart::Text { text }],
                        tokens: None,
                    });
                }
            }
            "assistant" if !is_sidechain => {
                let message = value.get("message").unwrap_or(&Value::Null);
                let model = message
                    .get("model")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                    .map(str::to_string);
                let record = ensure_current(&mut current, seq, time_ms);
                if record.model.is_none() {
                    record.model = model;
                }
                // 每轮用量取该回合最后一次调用的值（追加即覆盖）。
                if let Some(usage) = message.get("usage") {
                    let get = |key: &str| {
                        usage
                            .get(key)
                            .and_then(Value::as_i64)
                            .unwrap_or(0)
                            .max(0)
                    };
                    let tokens = TurnTokens {
                        input: get("input_tokens"),
                        cache_read: get("cache_read_input_tokens"),
                        cache_write: get("cache_creation_input_tokens"),
                        output: get("output_tokens"),
                        reasoning: 0,
                        cost: None,
                        context_window: None,
                    };
                    if !tokens.is_empty() {
                        record.tokens = Some(tokens);
                    }
                }
                let Some(blocks) = message.get("content").and_then(Value::as_array) else {
                    continue;
                };
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let text = block
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .trim();
                            if !text.is_empty() && !is_noise_text(text) {
                                record.parts.push(TimelinePart::Text {
                                    text: text.to_string(),
                                });
                            }
                        }
                        Some("thinking") => {
                            let text = block
                                .get("thinking")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .trim();
                            if !text.is_empty() {
                                record.parts.push(TimelinePart::Reasoning {
                                    text: text.to_string(),
                                });
                            }
                        }
                        Some("tool_use") => {
                            let raw_name = block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("tool");
                            let call_id = block
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string();
                            let input = block.get("input").unwrap_or(&Value::Null);
                            let part_index = record.parts.len();
                            record.parts.push(TimelinePart::Tool {
                                // 统一小写名，与 OpenCode 工具命名（bash/edit/read…）对齐。
                                name: raw_name.to_ascii_lowercase(),
                                // 结果未到达前视为执行中（历史文件里 = 被中断 / 未完成）。
                                status: Some("running".to_string()),
                                summary: tool_summary(input),
                                call_id: call_id.clone(),
                            });
                            if !call_id.is_empty() {
                                call_index.insert(call_id, (records.len(), part_index));
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    flush(&mut records, &mut current);
    Ok(records)
}

fn flush(records: &mut Vec<ParsedRecord>, current: &mut Option<ParsedRecord>) {
    if let Some(record) = current.take() {
        if !record.parts.is_empty() {
            records.push(record);
        }
    }
}

fn ensure_current<'a>(
    current: &'a mut Option<ParsedRecord>,
    seq: i64,
    time_created: i64,
) -> &'a mut ParsedRecord {
    if current.is_none() {
        *current = Some(ParsedRecord {
            seq,
            time_created,
            role: "assistant",
            model: None,
            parts: Vec::new(),
            tokens: None,
        });
    }
    current.as_mut().expect("刚插入的记录必然存在")
}

/// 用户消息文本：字符串直接取；数组拼接 text 块；CLI 注入噪音块（system-reminder 等）跳过。
fn user_text(content: &Value) -> String {
    match content {
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() || is_noise_text(trimmed) {
                String::new()
            } else {
                trimmed.to_string()
            }
        }
        Value::Array(blocks) => {
            let chunks: Vec<&str> = blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .map(str::trim)
                .filter(|text| !text.is_empty() && !is_noise_text(text))
                .collect();
            chunks.join("\n\n")
        }
        _ => String::new(),
    }
}

/// CLI 注入的噪音文本（系统提醒 / 本地命令回显），展示层不需要。
fn is_noise_text(text: &str) -> bool {
    text.starts_with("<system-reminder")
        || text.starts_with("<command-name")
        || text.starts_with("<command-message")
        || text.starts_with("<local-command")
}

/// 工具卡片摘要：常见入参字段优先，取不到就不给摘要。
pub fn tool_summary(input: &Value) -> Option<String> {
    if !input.is_object() {
        return None;
    }
    let pick = |key: &str| -> Option<&str> {
        input
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
    };
    let text = pick("command")
        .or_else(|| pick("file_path"))
        .or_else(|| pick("path"))
        .or_else(|| pick("pattern"))
        .or_else(|| pick("query"))
        .or_else(|| pick("url"))
        .or_else(|| pick("description"))
        .or_else(|| pick("prompt"))?;
    Some(compact(text, SUMMARY_LIMIT))
}

/// 压平空白并截断。
fn compact(text: &str, limit: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = flat.chars().collect();
    if chars.len() <= limit {
        flat
    } else {
        format!("{}…", chars[..limit].iter().collect::<String>())
    }
}

// ---------------------------------------------------------------- 工具详情

/// 单个工具调用的完整详情（展开卡片时按需读取；全文件扫描 call_id 限定）。
pub fn read_tool_detail(path: &Path, _message_id: &str, call_id: &str) -> ParseResult<ToolDetail> {
    let file = File::open(path).map_err(|cause| format!("打开会话档案失败：{cause}"))?;
    let reader = BufReader::new(file);

    let mut input: Option<Value> = None;
    let mut output: Option<String> = None;
    let mut is_error = false;
    let mut metadata: Option<Value> = None;

    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if !line.contains(call_id) {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(blocks) = value.pointer("/message/content").and_then(Value::as_array) {
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("tool_use")
                        if block.get("id").and_then(Value::as_str) == Some(call_id) =>
                    {
                        input = block.get("input").cloned();
                    }
                    Some("tool_result")
                        if block.get("tool_use_id").and_then(Value::as_str) == Some(call_id) =>
                    {
                        is_error = block
                            .get("is_error")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        output = result_text(block.get("content"));
                    }
                    _ => {}
                }
            }
        }
        if metadata.is_none() {
            if let Some(result) = value.get("toolUseResult") {
                metadata = edit_metadata(result);
            }
        }
        if input.is_some() && output.is_some() && !is_error {
            break;
        }
    }

    let truncated = output
        .as_deref()
        .map(|text| text.chars().count() > TOOL_OUTPUT_LIMIT)
        .unwrap_or(false);
    let output = output.map(|text| truncate_chars(&text, TOOL_OUTPUT_LIMIT));
    // 失败结果整体进错误位展示。
    let (output, error) = if is_error { (None, output) } else { (output, None) };
    Ok(ToolDetail {
        input,
        output,
        error,
        metadata,
        truncated,
    })
}

/// 工具结果文本：字符串直接取；数组拼接 text 块（图块等跳过）。
fn result_text(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let chunks: Vec<&str> = blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect();
            if chunks.is_empty() {
                None
            } else {
                Some(chunks.join("\n"))
            }
        }
        _ => None,
    }
}

/// Edit / Write 的结构化结果（`toolUseResult.structuredPatch`）→ 统一 diff 文本，
/// 复用前端工具卡片的 `metadata.files[].patch` 渲染。
fn edit_metadata(result: &Value) -> Option<Value> {
    let file_path = result.get("filePath").and_then(Value::as_str)?;
    let hunks = result.get("structuredPatch").and_then(Value::as_array)?;
    if hunks.is_empty() {
        return None;
    }
    let mut patch = format!("--- a/{file_path}\n+++ b/{file_path}\n");
    for hunk in hunks {
        let old_start = hunk.get("oldStart").and_then(Value::as_i64).unwrap_or(0);
        let old_lines = hunk.get("oldLines").and_then(Value::as_i64).unwrap_or(0);
        let new_start = hunk.get("newStart").and_then(Value::as_i64).unwrap_or(0);
        let new_lines = hunk.get("newLines").and_then(Value::as_i64).unwrap_or(0);
        patch.push_str(&format!(
            "@@ -{old_start},{old_lines} +{new_start},{new_lines} @@\n"
        ));
        if let Some(lines) = hunk.get("lines").and_then(Value::as_array) {
            for line in lines {
                if let Some(text) = line.as_str() {
                    patch.push_str(text);
                    patch.push('\n');
                }
            }
        }
    }
    Some(json!({ "files": [{ "file": file_path, "patch": patch }] }))
}

fn truncate_chars(text: &str, limit: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= limit {
        return text.to_string();
    }
    chars[..limit].iter().collect()
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const FIXTURE: &str = concat!(
        r#"{"type":"user","message":{"role":"user","content":"帮我看下这个项目"},"uuid":"u1","parentUuid":null,"sessionId":"s1","timestamp":"2026-10-01T10:00:00.000Z","cwd":"/tmp/demo","gitBranch":"main","version":"2.0.14"}"#,
        "\n",
        r#"{"type":"assistant","message":{"id":"m1","type":"message","role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"thinking","thinking":"先看目录结构","signature":"sig"},{"type":"text","text":"我先看一下。"},{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls -la","description":"列出文件"}}],"usage":{"input_tokens":10,"output_tokens":21,"cache_creation_input_tokens":100,"cache_read_input_tokens":1000}},"uuid":"a1","parentUuid":"u1","sessionId":"s1","timestamp":"2026-10-01T10:00:03.000Z","cwd":"/tmp/demo"}"#,
        "\n",
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"total 8\ndrwxr-xr-x  3 me staff","is_error":false}]},"toolUseResult":{"stdout":"total 8"},"uuid":"u2","parentUuid":"a1","sessionId":"s1","timestamp":"2026-10-01T10:00:04.000Z","cwd":"/tmp/demo"}"#,
        "\n",
        r#"{"type":"assistant","message":{"id":"m2","type":"message","role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"text","text":"目录里有 a.py。"},{"type":"tool_use","id":"toolu_2","name":"Edit","input":{"file_path":"/tmp/demo/a.py","old_string":"x = 1","new_string":"x = 2","replace_all":false}}],"usage":{"input_tokens":5,"output_tokens":30,"cache_creation_input_tokens":0,"cache_read_input_tokens":2000}},"uuid":"a2","parentUuid":"u2","sessionId":"s1","timestamp":"2026-10-01T10:00:05.000Z","cwd":"/tmp/demo"}"#,
        "\n",
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_2","content":"已修改 a.py","is_error":false}]},"toolUseResult":{"filePath":"/tmp/demo/a.py","oldString":"x = 1","newString":"x = 2","structuredPatch":[{"oldStart":1,"oldLines":1,"newStart":1,"newLines":1,"lines":["-x = 1","+x = 2"]}]},"uuid":"u3","parentUuid":"a2","sessionId":"s1","timestamp":"2026-10-01T10:00:06.000Z","cwd":"/tmp/demo"}"#,
        "\n",
        r#"{"type":"assistant","message":{"id":"m3","type":"message","role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"text","text":"改好了。"}],"usage":{"input_tokens":8,"output_tokens":4,"cache_creation_input_tokens":0,"cache_read_input_tokens":3000}},"uuid":"a3","parentUuid":"u3","sessionId":"s1","timestamp":"2026-10-01T10:00:07.000Z","cwd":"/tmp/demo"}"#,
        "\n",
        r#"{"type":"file-history-snapshot","messageId":"x","snapshot":{"trackedFileBackups":{"a.py":{"backupFileName":null,"version":2}}},"isSnapshotUpdate":false}"#,
        "\n",
        r#"{"type":"ai-title","aiTitle":"目录查看与修改","sessionId":"s1"}"#,
        "\n",
        r#"{"type":"custom-title","customTitle":"修 a.py","sessionId":"s1"}"#,
        "\n",
        r#"{"type":"user","message":{"role":"user","content":"再看看 b.py"},"uuid":"u4","parentUuid":"a3","sessionId":"s1","timestamp":"2026-10-01T10:00:10.000Z","cwd":"/tmp/demo"}"#,
        "\n",
        r#"{"type":"assistant","isSidechain":true,"message":{"id":"m4","type":"message","role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"tool_use","id":"toolu_9","name":"Grep","input":{"pattern":"def main","path":"/tmp/demo"}}],"usage":{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}},"uuid":"sx","parentUuid":"a3","sessionId":"s1","timestamp":"2026-10-01T10:00:11.000Z","cwd":"/tmp/demo"}"#,
        "\n",
        r#"{"type":"system","subtype":"compact_boundary","content":"压缩边界","timestamp":"2026-10-01T10:00:12.000Z"}"#,
        "\n",
        r#"{"type":"assistant","message":{"id":"m5","type":"message","role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"tool_use","id":"toolu_4","name":"Read","input":{"file_path":"/tmp/demo/b.py"}}],"usage":{"input_tokens":2,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}},"uuid":"a4","parentUuid":"u4","sessionId":"s1","timestamp":"2026-10-01T10:00:13.000Z","cwd":"/tmp/demo"}"#,
        "\n",
    );

    fn parse_fixture() -> Vec<ParsedRecord> {
        parse_reader(BufReader::new(Cursor::new(FIXTURE))).expect("解析 fixture")
    }

    fn write_fixture_dir(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("prism-claude-{tag}-{}", std::process::id()));
        let dir = root.join("-tmp-demo");
        std::fs::create_dir_all(&dir).expect("临时目录");
        std::fs::write(dir.join("a1.jsonl"), FIXTURE).expect("写 fixture");
        std::fs::write(dir.join("agent-sub.jsonl"), FIXTURE).expect("写子代理档案");
        root
    }

    #[test]
    fn assistant_records_carry_last_call_usage() {
        let records = parse_fixture();
        // 第一个助手回合含 m1 → m2 → m3 三次调用，取最后一次（m3）。
        let tokens = records[1].tokens.expect("应有 token");
        assert_eq!(tokens.input, 8);
        assert_eq!(tokens.cache_read, 3000);
        assert_eq!(tokens.cache_write, 0);
        assert_eq!(tokens.output, 4);
        assert_eq!(tokens.reasoning, 0);
        // 第二个助手回合（m5）。
        let tokens = records[3].tokens.expect("应有 token");
        assert_eq!(tokens.input, 2);
        assert_eq!(tokens.output, 2);
    }

    #[test]
    fn groups_records_and_backfills_status() {
        let records = parse_fixture();
        assert_eq!(records.len(), 4, "用户×2 + 助手×2（工具结果不切记录）");
        assert_eq!(records[0].role, "user");
        assert_eq!(records[0].seq, 1);

        let assistant = &records[1];
        assert_eq!(assistant.role, "assistant");
        assert_eq!(assistant.seq, 2);
        assert_eq!(assistant.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(
            assistant.parts.len(),
            6,
            "思考+正文+工具+正文+工具+结尾正文"
        );
        assert!(matches!(assistant.parts[0], TimelinePart::Reasoning { .. }));
        match &assistant.parts[2] {
            TimelinePart::Tool {
                name,
                status,
                summary,
                call_id,
            } => {
                assert_eq!(name, "bash", "工具名统一小写");
                assert_eq!(status.as_deref(), Some("completed"), "tool_result 回填");
                assert_eq!(summary.as_deref(), Some("ls -la"));
                assert_eq!(call_id, "toolu_1");
            }
            other => panic!("应为工具 part：{other:?}"),
        }
        match &assistant.parts[4] {
            TimelinePart::Tool { name, status, .. } => {
                assert_eq!(name, "edit");
                assert_eq!(status.as_deref(), Some("completed"));
            }
            other => panic!("应为工具 part：{other:?}"),
        }

        // 第二条用户消息与其后的助手记录。
        assert_eq!(records[2].role, "user");
        assert_eq!(records[2].seq, 10);
        let tail = &records[3];
        assert_eq!(tail.seq, 13, "sidechain 助手行不参与");
        assert_eq!(tail.parts.len(), 1);
        match &tail.parts[0] {
            TimelinePart::Tool { name, status, .. } => {
                assert_eq!(name, "read");
                assert_eq!(status.as_deref(), Some("running"), "无结果 → 执行中");
                assert_eq!(tool_summary(&serde_json::json!({"file_path": "/tmp/demo/b.py"})).as_deref(), Some("/tmp/demo/b.py"));
            }
            other => panic!("应为工具 part：{other:?}"),
        }
    }

    #[test]
    fn renders_page_with_cursor() {
        let page = render_page(parse_fixture(), None, 2);
        assert!(page.has_more);
        assert_eq!(page.records.len(), 2);
        assert_eq!(page.next_cursor, Some(10));
        let page = render_page(parse_fixture(), Some(10), 10);
        assert!(!page.has_more);
        assert_eq!(page.records.len(), 2, "游标之前的两条");
        assert_eq!(page.records[0].id, "1");
    }

    #[test]
    fn scans_summary_fields() {
        let root = write_fixture_dir("scan");
        let scan = scan_file(&root.join("-tmp-demo").join("a1.jsonl")).expect("扫描");
        assert_eq!(scan.custom_title.as_deref(), Some("修 a.py"));
        assert_eq!(scan.ai_title.as_deref(), Some("目录查看与修改"));
        assert_eq!(scan.first_user.as_deref(), Some("帮我看下这个项目"));
        assert_eq!(scan.cwd.as_deref(), Some("/tmp/demo"));
        assert_eq!(scan.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(
            scan.first_ts,
            parse_iso_ms("2026-10-01T10:00:00.000Z"),
            "首个时间戳"
        );
        assert_eq!(scan.last_ts, parse_iso_ms("2026-10-01T10:00:13.000Z"));
        // 用量含子代理：1110+2005+3008+2 + sidechain 1 = 6126。
        assert_eq!(scan.tokens_input, 6126);
        assert_eq!(scan.tokens_output, 58);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn lists_projects_and_sessions() {
        let root = write_fixture_dir("list");
        let projects = list_projects(&root).expect("项目列表");
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].worktree, "/tmp/demo", "采信档案内 cwd");
        assert_eq!(projects[0].session_count, 1, "agent-*.jsonl 排除");
        assert_eq!(projects[0].id, "claude:-tmp-demo");

        let sessions = list_sessions(&root, "-tmp-demo", 10).expect("会话列表");
        assert_eq!(sessions.len(), 1);
        let session = &sessions[0];
        assert_eq!(session.id, "claude:a1");
        assert_eq!(session.title.as_deref(), Some("修 a.py"), "custom-title 优先");
        assert_eq!(session.directory, "/tmp/demo");
        assert_eq!(session.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(session.tokens_input, 6126);

        let summary = session_summary(&root, "a1").expect("单会话").expect("存在");
        assert_eq!(summary.id, "claude:a1");
        assert!(session_summary(&root, "missing").expect("查询").is_none());
        assert!(session_summary(&root, "../x").expect("查询").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reads_timeline_and_tool_detail() {
        let root = write_fixture_dir("detail");
        let path = root.join("-tmp-demo").join("a1.jsonl");
        let page = read_timeline(&path, None, 150).expect("时间线");
        assert_eq!(page.records.len(), 4);

        let detail = read_tool_detail(&path, "4", "toolu_2").expect("详情");
        assert_eq!(
            detail
                .input
                .as_ref()
                .and_then(|value| value.get("file_path"))
                .and_then(Value::as_str),
            Some("/tmp/demo/a.py")
        );
        assert_eq!(detail.output.as_deref(), Some("已修改 a.py"));
        let patch = detail
            .metadata
            .as_ref()
            .and_then(|value| value.pointer("/files/0/patch"))
            .and_then(Value::as_str)
            .expect("diff patch");
        assert!(patch.contains("@@ -1,1 +1,1 @@"), "hunk 头：{patch}");
        assert!(patch.contains("-x = 1"));
        assert!(patch.contains("+x = 2"));

        let missing = read_tool_detail(&path, "1", "toolu_404").expect("详情");
        assert!(missing.input.is_none() && missing.output.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn detects_noise_and_tool_summaries() {
        assert!(is_noise_text("<system-reminder>注意</system-reminder>"));
        assert!(!is_noise_text("正常内容"));
        let content = serde_json::json!([
            {"type": "text", "text": "<system-reminder>噪音</system-reminder>"},
            {"type": "text", "text": "真正的问题"}
        ]);
        assert_eq!(user_text(&content), "真正的问题");
        let tool_result_only = serde_json::json!([
            {"type": "tool_result", "tool_use_id": "t", "content": "x"}
        ]);
        assert_eq!(user_text(&tool_result_only), "");
        assert_eq!(
            tool_summary(&serde_json::json!({"command": "ls  -la"})).as_deref(),
            Some("ls -la")
        );
        assert_eq!(tool_summary(&serde_json::json!({"todos": []})), None);
    }

    #[test]
    fn claude_id_prefixes() {
        assert!(is_claude_session("claude:abc"));
        assert!(!is_claude_session("codex:abc"));
        assert_eq!(session_uuid_of("claude:abc"), Some("abc"));
        assert_eq!(dir_name_of("claude:-tmp-demo"), Some("-tmp-demo"));
        assert!(is_claude_project("claude:-tmp-demo"));
    }

    /// 依赖本机真实 Claude 数据：`cargo test -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn reads_real_claude_data() {
        let Some(root) = locator::projects_dir() else {
            eprintln!("未找到 ~/.claude，跳过");
            return;
        };
        if !root.is_dir() {
            eprintln!("{} 不存在，跳过", root.display());
            return;
        }
        let projects = list_projects(&root).expect("项目列表");
        println!("Claude 项目 {} 个", projects.len());
        for project in &projects {
            let dir_name = dir_name_of(&project.id).expect("项目 id 前缀");
            let sessions = list_sessions(&root, dir_name, 50).expect("会话列表");
            println!(
                "项目 {}（{} 会话，最新 {}）",
                project.worktree,
                project.session_count,
                sessions
                    .first()
                    .map(|session| format!(
                        "{:?} ↓{} ↑{}",
                        session.title, session.tokens_input, session.tokens_output
                    ))
                    .unwrap_or_else(|| "（空）".to_string())
            );
        }
        let Some(project) = projects.first() else {
            eprintln!("没有项目档案");
            return;
        };
        let dir_name = dir_name_of(&project.id).expect("项目 id 前缀");
        let sessions = list_sessions(&root, dir_name, 50).expect("会话列表");
        if let Some(session) = sessions.first() {
            if let Some((dir_name, path)) = find_session_file(&root, session_uuid_of(&session.id).unwrap_or_default()) {
                let page = read_timeline(&path, None, 150).expect("时间线");
                println!("{} 时间线 {} 条", dir_name, page.records.len());
            }
        }
    }
}
