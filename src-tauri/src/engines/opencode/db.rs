//! OpenCode 历史数据（opencode.db）的只读读取。
//!
//! 原则：一律以只读方式打开数据库；引擎原始数据不做任何写入，
//! Prism 的增值数据（标题、标签等）将来另行存放。

use std::path::Path;

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row};
use serde_json::Value;

use crate::engines::core::{
    ProjectItem, SessionSummary, TimelinePage, TimelinePart, TimelineRecord, ToolDetail,
    TurnTokens,
};
use crate::ledger::{count_lines, RawEvent};

type DbResult<T> = Result<T, String>;

const SESSION_LIMIT: i64 = 500;
/// 一页时间线最多返回多少条映射后的记录。
const TIMELINE_PAGE: i64 = 150;
/// 工具详情输出上限（字符），超出截断。
const TOOL_OUTPUT_LIMIT: usize = 200_000;
/// 单条消息入全文索引的文本上限（字符），超出截断。
const INDEX_TEXT_LIMIT: usize = 20_000;

pub fn session_limit() -> i64 {
    SESSION_LIMIT
}

pub fn timeline_page_size() -> i64 {
    TIMELINE_PAGE
}

fn open_readonly(path: &Path) -> DbResult<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|cause| format!("打开 OpenCode 数据库失败：{cause}"))?;
    let _ = connection.busy_timeout(std::time::Duration::from_millis(2000));
    Ok(connection)
}

/// 项目列表：只保留有会话的项目，按最近会话活动排序。
pub fn list_projects(db_path: &Path) -> DbResult<Vec<ProjectItem>> {
    let connection = open_readonly(db_path)?;
    let mut statement = connection
        .prepare(
            "SELECT p.id, p.worktree, COALESCE(p.name, ''), p.time_updated,
                    COUNT(s.id) AS session_count
             FROM project p
             JOIN session_v2 s
               ON s.project_id = p.id
              AND s.parent_id IS NULL
              AND s.time_archived IS NULL
             WHERE p.time_archived IS NULL
             GROUP BY p.id
             ORDER BY MAX(s.time_updated) DESC",
        )
        .map_err(|cause| format!("查询项目失败：{cause}"))?;

    let rows = statement
        .query_map([], |row| {
            Ok(ProjectItem {
                engine: super::locator::ENGINE_ID.to_string(),
                id: row.get(0)?,
                worktree: row.get(1)?,
                name: row.get(2)?,
                time_updated: row.get(3)?,
                session_count: row.get(4)?,
            })
        })
        .map_err(|cause| format!("查询项目失败：{cause}"))?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|cause| format!("读取项目失败：{cause}"))
}

/// 某项目下的顶层会话（子代理会话不单列）。
pub fn list_sessions(db_path: &Path, project_id: &str, limit: i64) -> DbResult<Vec<SessionSummary>> {
    let connection = open_readonly(db_path)?;
    let mut statement = connection
        .prepare(
            "SELECT id, project_id, title, directory, model, cost,
                    tokens_input, tokens_output, tokens_reasoning,
                    time_created, time_updated
             FROM session_v2
             WHERE project_id = ?1
               AND parent_id IS NULL
               AND time_archived IS NULL
             ORDER BY time_updated DESC
             LIMIT ?2",
        )
        .map_err(|cause| format!("查询会话失败：{cause}"))?;

    let rows = statement
        .query_map(params![project_id, limit], map_session_row)
        .map_err(|cause| format!("查询会话失败：{cause}"))?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|cause| format!("读取会话失败：{cause}"))
}

/// 按 id 读取单个会话（工作台恢复列时用；不限制父级 / 归档状态）。
pub fn get_session(db_path: &Path, session_id: &str) -> DbResult<Option<SessionSummary>> {
    let connection = open_readonly(db_path)?;
    connection
        .query_row(
            "SELECT id, project_id, title, directory, model, cost,
                    tokens_input, tokens_output, tokens_reasoning,
                    time_created, time_updated
             FROM session_v2
             WHERE id = ?1",
            params![session_id],
            map_session_row,
        )
        .optional()
        .map_err(|cause| format!("查询会话失败：{cause}"))
}

fn map_session_row(row: &Row<'_>) -> rusqlite::Result<SessionSummary> {
    let raw_model: Option<String> = row.get(4)?;
    Ok(SessionSummary {
        engine: super::locator::ENGINE_ID.to_string(),
        id: row.get(0)?,
        project_id: row.get(1)?,
        title: row.get(2)?,
        directory: row.get(3)?,
        model: raw_model.as_deref().and_then(parse_model),
        cost: row.get(5)?,
        tokens_input: row.get(6)?,
        tokens_output: row.get(7)?,
        tokens_reasoning: row.get(8)?,
        time_created: row.get(9)?,
        time_updated: row.get(10)?,
    })
}

/// 增量索引用：自 `since` 之后有更新的顶层会话（id / 项目 / 更新时间）。
pub fn list_sessions_updated_since(
    db_path: &Path,
    since: i64,
) -> DbResult<Vec<(String, String, i64)>> {
    let connection = open_readonly(db_path)?;
    let mut statement = connection
        .prepare(
            "SELECT id, project_id, time_updated
             FROM session_v2
             WHERE parent_id IS NULL
               AND time_archived IS NULL
               AND time_updated > ?1
             ORDER BY time_updated ASC",
        )
        .map_err(|cause| format!("查询增量会话失败：{cause}"))?;

    let rows = statement
        .query_map(params![since], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|cause| format!("查询增量会话失败：{cause}"))?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|cause| format!("读取增量会话失败：{cause}"))
}

/// 全部顶层会话（搜索的标题查询 / 命中展示用）。
pub fn list_all_sessions(db_path: &Path) -> DbResult<Vec<SessionSummary>> {
    let connection = open_readonly(db_path)?;
    let mut statement = connection
        .prepare(
            "SELECT id, project_id, title, directory, model, cost,
                    tokens_input, tokens_output, tokens_reasoning,
                    time_created, time_updated
             FROM session_v2
             WHERE parent_id IS NULL
               AND time_archived IS NULL
             ORDER BY time_updated DESC
             LIMIT 2000",
        )
        .map_err(|cause| format!("查询会话失败：{cause}"))?;

    let rows = statement
        .query_map([], map_session_row)
        .map_err(|cause| format!("查询会话失败：{cause}"))?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|cause| format!("读取会话失败：{cause}"))
}

/// 一条进全文索引的消息文档。
#[derive(Debug, Clone)]
pub struct IndexDoc {
    pub message_id: String,
    pub role: String,
    pub seq: i64,
    pub time_created: i64,
    pub text: String,
}

/// 单个会话里可索引的消息文本（user 全文 + assistant 文本/推理拼接）。
pub fn list_index_docs(db_path: &Path, session_id: &str) -> DbResult<Vec<IndexDoc>> {
    let connection = open_readonly(db_path)?;
    let mut statement = connection
        .prepare(
            "SELECT id, seq, type, time_created, data
             FROM session_message
             WHERE session_id = ?1
               AND type IN ('user', 'assistant')
             ORDER BY seq ASC",
        )
        .map_err(|cause| format!("查询消息失败：{cause}"))?;

    let rows = statement
        .query_map(params![session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(|cause| format!("查询消息失败：{cause}"))?;

    let mut docs = Vec::new();
    for row in rows {
        let (message_id, seq, kind, time_created, data) =
            row.map_err(|cause| format!("读取消息失败：{cause}"))?;
        let Ok(value) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        let text = match kind.as_str() {
            "user" => user_text(&value),
            "assistant" => assistant_text(&value),
            _ => None,
        };
        let Some(text) = text else { continue };
        docs.push(IndexDoc {
            message_id,
            role: kind,
            seq,
            time_created,
            text: truncate_chars(&text, INDEX_TEXT_LIMIT),
        });
    }
    Ok(docs)
}

/// 用户消息的可读文本。
fn user_text(data: &Value) -> Option<String> {
    let text = data
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| data.pointer("/metadata/displayText").and_then(Value::as_str))
        .unwrap_or("");
    if text.trim().is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// 助手消息的正文 + 推理文本（工具输出不入索引）。
fn assistant_text(data: &Value) -> Option<String> {
    let mut chunks: Vec<&str> = Vec::new();
    if let Some(content) = data.get("content").and_then(Value::as_array) {
        for part in content {
            match part.get("type").and_then(Value::as_str) {
                Some("text") | Some("reasoning") => {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        if !text.trim().is_empty() {
                            chunks.push(text);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    if chunks.is_empty() {
        None
    } else {
        Some(chunks.join("\n\n"))
    }
}

/// 会话时间线分页：从最新往回取一页，按时间正序返回。
///
/// `before_seq` 为上一页的游标（更早消息的边界，严格递减）。
pub fn read_timeline(
    db_path: &Path,
    session_id: &str,
    before_seq: Option<i64>,
    limit: i64,
) -> DbResult<TimelinePage> {
    let connection = open_readonly(db_path)?;
    let mut statement = connection
        .prepare(
            "SELECT id, seq, type, time_created, data
             FROM session_message
             WHERE session_id = ?1
               AND type IN ('user', 'assistant')
               AND (?2 IS NULL OR seq < ?2)
             ORDER BY seq DESC
             LIMIT ?3",
        )
        .map_err(|cause| format!("查询消息失败：{cause}"))?;

    let limit = limit.max(1) as usize;
    // DESC 顺序收集，最后反转成时间正序。
    let mut collected: Vec<TimelineRecord> = Vec::new();
    let mut cursor = before_seq;
    let mut last_consumed_seq: Option<i64> = None;
    let mut has_more = false;

    while collected.len() < limit {
        let rows = statement
            .query_map(params![session_id, cursor, limit as i64], |row| {
                let id: String = row.get(0)?;
                let seq: i64 = row.get(1)?;
                let kind: String = row.get(2)?;
                let time_created: i64 = row.get(3)?;
                let data: String = row.get(4)?;
                Ok((id, seq, kind, time_created, data))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map_err(|cause| format!("查询消息失败：{cause}"))?;

        if rows.is_empty() {
            break;
        }
        let source_exhausted = rows.len() < limit;

        let mut rows = rows.into_iter();
        while let Some((id, seq, kind, time_created, data)) = rows.next() {
            last_consumed_seq = Some(seq);
            let Ok(value) = serde_json::from_str::<Value>(&data) else {
                continue;
            };
            let record = match kind.as_str() {
                "user" => map_user_record(id, time_created, &value),
                "assistant" => map_assistant_record(id, time_created, &value),
                _ => None,
            };
            if let Some(record) = record {
                collected.push(record);
                if collected.len() >= limit {
                    // 页已满：本批还有更早的行，或源头还有更早的批次。
                    has_more = rows.len() > 0 || !source_exhausted;
                    break;
                }
            }
        }

        if has_more || source_exhausted {
            break;
        }
        cursor = last_consumed_seq;
    }

    collected.reverse();
    Ok(TimelinePage {
        records: collected,
        next_cursor: if has_more { last_consumed_seq } else { None },
        has_more,
    })
}

/// 单个工具调用的完整详情（展开卡片时按需读取）。
pub fn read_tool_detail(
    db_path: &Path,
    session_id: &str,
    message_id: &str,
    call_id: &str,
) -> DbResult<ToolDetail> {
    let connection = open_readonly(db_path)?;
    let data: Option<String> = connection
        .prepare("SELECT data FROM session_message WHERE id = ?1 AND session_id = ?2")
        .and_then(|mut statement| {
            statement
                .query_row(params![message_id, session_id], |row| row.get(0))
                .optional()
        })
        .map_err(|cause| format!("查询工具调用失败：{cause}"))?;

    let data = data.ok_or_else(|| "未找到该消息".to_string())?;
    let value: Value =
        serde_json::from_str(&data).map_err(|cause| format!("解析消息失败：{cause}"))?;

    let part = value
        .get("content")
        .and_then(Value::as_array)
        .and_then(|parts| {
            parts.iter().find(|part| {
                part.get("type").and_then(Value::as_str) == Some("tool")
                    && part.get("id").and_then(Value::as_str) == Some(call_id)
            })
        })
        .ok_or_else(|| "未找到该工具调用".to_string())?;

    let state = part.get("state").filter(|state| !state.is_null());
    let input = state
        .and_then(|state| state.get("input"))
        .filter(|input| !input.is_null())
        .cloned();
    let metadata = state
        .and_then(|state| state.get("metadata"))
        .filter(|metadata| !metadata.is_null())
        .cloned();
    let error = state.and_then(extract_tool_error);
    let (output, truncated) = match state.and_then(extract_tool_output) {
        Some(output) => truncate_long(&output, TOOL_OUTPUT_LIMIT),
        None => (None, false),
    };

    Ok(ToolDetail {
        input,
        output,
        error,
        metadata,
        truncated,
    })
}

/// v2 的输出在 `state.content` 文本数组；兼容 `state.output` 字符串。
fn extract_tool_output(state: &Value) -> Option<String> {
    if let Some(Value::String(text)) = state.get("output") {
        return Some(text.clone());
    }
    let parts = state.get("content").and_then(Value::as_array)?;
    let texts: Vec<&str> = parts
        .iter()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect();
    if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n"))
    }
}

/// 错误可能是字符串，也可能是 `{type, message}` 对象。
fn extract_tool_error(state: &Value) -> Option<String> {
    let error = state.get("error")?;
    match error {
        Value::Null => None,
        Value::String(text) if text.trim().is_empty() => None,
        Value::String(text) => Some(text.clone()),
        other => other
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| serde_json::to_string(other).ok()),
    }
}

fn truncate_long(text: &str, max: usize) -> (Option<String>, bool) {
    if text.chars().count() <= max {
        return (Some(text.to_string()), false);
    }
    (Some(text.chars().take(max).collect()), true)
}

/// session_v2.model 是 JSON（{"id","providerID",...}），展示为 "provider/id"。
fn parse_model(raw: &str) -> Option<String> {
    match serde_json::from_str::<Value>(raw) {
        Ok(value) => {
            let id = value.get("id").and_then(Value::as_str);
            let provider = value.get("providerID").and_then(Value::as_str);
            match (provider, id) {
                (Some(provider), Some(id)) => Some(format!("{provider}/{id}")),
                (None, Some(id)) => Some(id.to_string()),
                _ => None,
            }
        }
        Err(_) => Some(raw.to_string()),
    }
}

fn map_user_record(id: String, time_created: i64, data: &Value) -> Option<TimelineRecord> {
    let text = user_text(data)?;
    Some(TimelineRecord {
        id,
        role: "user".to_string(),
        time_created,
        model: None,
        model_provider: None,
        variant: None,
        completed: true,
        parts: vec![TimelinePart::Text { text }],
        tokens: None,
    })
}

fn map_assistant_record(id: String, time_created: i64, data: &Value) -> Option<TimelineRecord> {
    let completed = data.pointer("/time/completed").is_some();
    // `model` 是对象：{"id","providerID","variant"}；variant 即推理档位。
    let model = data
        .pointer("/model/id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let model_provider = data
        .pointer("/model/providerID")
        .and_then(Value::as_str)
        .map(str::to_string);
    let variant = data
        .pointer("/model/variant")
        .and_then(Value::as_str)
        .map(str::to_string);
    let tokens = parse_turn_tokens(data);

    let mut parts = Vec::new();
    if let Some(content) = data.get("content").and_then(Value::as_array) {
        for part in content {
            match part.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        if !text.trim().is_empty() {
                            parts.push(TimelinePart::Text {
                                text: text.to_string(),
                            });
                        }
                    }
                }
                Some("reasoning") => {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        if !text.trim().is_empty() {
                            parts.push(TimelinePart::Reasoning {
                                text: text.to_string(),
                            });
                        }
                    }
                }
                Some("tool") => {
                    let name = part
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_string();
                    let call_id = part
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let status = part
                        .pointer("/state/status")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let summary = part
                        .pointer("/state/input")
                        .and_then(summarize_tool_input);
                    parts.push(TimelinePart::Tool {
                        name,
                        status,
                        summary,
                        call_id,
                    });
                }
                _ => {}
            }
        }
    }

    if parts.is_empty() {
        return None;
    }

    Some(TimelineRecord {
        id,
        role: "assistant".to_string(),
        time_created,
        model,
        model_provider,
        variant,
        completed,
        parts,
        tokens,
    })
}

/// assistant 消息里的单轮用量（v2：`tokens.{input,output,reasoning,cache.{read,write}}` + `cost`）。
fn parse_turn_tokens(data: &Value) -> Option<TurnTokens> {
    let tokens = data.get("tokens")?;
    let get = |key: &str| {
        tokens
            .get(key)
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .max(0)
    };
    let cache = |key: &str| {
        tokens
            .get("cache")
            .and_then(|value| value.get(key))
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .max(0)
    };
    let parsed = TurnTokens {
        input: get("input"),
        cache_read: cache("read"),
        cache_write: cache("write"),
        output: get("output"),
        reasoning: get("reasoning"),
        cost: data
            .get("cost")
            .and_then(Value::as_f64)
            .filter(|value| *value > 0.0),
        context_window: None,
    };
    if parsed.is_empty() {
        None
    } else {
        Some(parsed)
    }
}

/// 工具卡片的一行摘要：优先取常见字段，取不到就压平 JSON。
fn summarize_tool_input(input: &Value) -> Option<String> {
    const PREFERRED_KEYS: [&str; 9] = [
        "command",
        "filePath",
        "file_path",
        "path",
        "pattern",
        "query",
        "url",
        "description",
        "task",
    ];

    let raw = PREFERRED_KEYS
        .iter()
        .find_map(|key| input.get(*key).and_then(Value::as_str))
        .map(str::to_string)
        .or_else(|| {
            if input.as_object().is_some_and(|object| object.is_empty()) {
                None
            } else {
                serde_json::to_string(input).ok()
            }
        })?;

    let compact = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(truncate_chars(&compact, 180))
}

fn truncate_chars(text: &str, max: usize) -> String {
    let mut truncated: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        truncated.push('…');
    }
    truncated
}

/// 单条消息里的文件操作（edit 的 metadata.files 最全：补丁 + 增删行数）。
pub fn ledger_events_from_message(id: &str, time_created: i64, data: &Value) -> Vec<RawEvent> {
    let mut events = Vec::new();
    let Some(content) = data.get("content").and_then(Value::as_array) else {
        return events;
    };
    for part in content {
        if part.get("type").and_then(Value::as_str) != Some("tool") {
            continue;
        }
        let name = part.get("name").and_then(Value::as_str).unwrap_or_default();
        let call_id = part
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let input = part.pointer("/state/input").unwrap_or(&Value::Null);
        let metadata_files = part
            .pointer("/state/metadata/files")
            .and_then(Value::as_array);
        let push = |events: &mut Vec<RawEvent>, path: &str, kind: &str, added: i64, removed: i64| {
            events.push(RawEvent {
                path: path.to_string(),
                record_id: id.to_string(),
                call_id: call_id.clone(),
                time: time_created,
                kind: kind.to_string(),
                tool: name.to_string(),
                added,
                removed,
            });
        };

        match name {
            "edit" => {
                if let Some(files) = metadata_files {
                    for file in files {
                        let Some(path) = file.get("file").and_then(Value::as_str) else {
                            continue;
                        };
                        let kind = match file
                            .get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("modified")
                        {
                            "added" => "write",
                            "deleted" => "delete",
                            _ => "edit",
                        };
                        push(
                            &mut events,
                            path,
                            kind,
                            file.get("additions").and_then(Value::as_i64).unwrap_or(0),
                            file.get("deletions").and_then(Value::as_i64).unwrap_or(0),
                        );
                    }
                } else if let Some(path) = input.get("path").and_then(Value::as_str) {
                    // 旧数据没有 metadata.files：用输入里的新旧文本估算行数。
                    push(
                        &mut events,
                        path,
                        "edit",
                        count_lines(
                            input
                                .get("newString")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                        ),
                        count_lines(
                            input
                                .get("oldString")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                        ),
                    );
                }
            }
            "write" => {
                if let Some(path) = input.get("path").and_then(Value::as_str) {
                    push(
                        &mut events,
                        path,
                        "write",
                        count_lines(input.get("content").and_then(Value::as_str).unwrap_or_default()),
                        0,
                    );
                }
            }
            "read" => {
                if let Some(path) = input.get("path").and_then(Value::as_str) {
                    push(&mut events, path, "read", 0, 0);
                }
            }
            // shell / grep / glob 等命令不记账本（动了什么文件无法可靠归因）。
            _ => {}
        }
    }
    events
}

/// 整个会话的账本事件（按消息顺序；聚合交给 ledger::aggregate）。
pub fn read_ledger(db_path: &Path, session_id: &str) -> DbResult<Vec<RawEvent>> {
    let connection = open_readonly(db_path)?;
    let mut statement = connection
        .prepare(
            "SELECT id, time_created, data
             FROM session_message
             WHERE session_id = ?1 AND type = 'assistant'
             ORDER BY seq ASC",
        )
        .map_err(|cause| format!("查询消息失败：{cause}"))?;
    let rows = statement
        .query_map(params![session_id], |row| {
            let id: String = row.get(0)?;
            let time_created: i64 = row.get(1)?;
            let data: String = row.get(2)?;
            Ok((id, time_created, data))
        })
        .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        .map_err(|cause| format!("查询消息失败：{cause}"))?;

    let mut events = Vec::new();
    for (id, time_created, data) in rows {
        let Ok(value) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        events.extend(ledger_events_from_message(&id, time_created, &value));
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_model_reads_json_pair() {
        assert_eq!(
            parse_model(r#"{"id":"gpt-5","providerID":"openai"}"#).as_deref(),
            Some("openai/gpt-5")
        );
        assert_eq!(parse_model(r#"{"id":"only-id"}"#).as_deref(), Some("only-id"));
    }

    #[test]
    fn parse_model_falls_back_to_raw_string() {
        assert_eq!(parse_model("plain-model").as_deref(), Some("plain-model"));
    }

    #[test]
    fn summarize_prefers_command_and_compacts_whitespace() {
        let input = json!({"command": "ls -la\n  pwd"});
        assert_eq!(summarize_tool_input(&input).as_deref(), Some("ls -la pwd"));
    }

    #[test]
    fn summarize_prefers_path_when_no_command() {
        let input = json!({"path": "/tmp/x", "extra": 1});
        assert_eq!(summarize_tool_input(&input).as_deref(), Some("/tmp/x"));
    }

    #[test]
    fn summarize_empty_object_is_none() {
        assert_eq!(summarize_tool_input(&json!({})), None);
    }

    #[test]
    fn assistant_record_skips_empty_content() {
        let data = json!({"time": {"created": 1}, "content": []});
        assert!(map_assistant_record("m1".to_string(), 0, &data).is_none());
    }

    #[test]
    fn assistant_record_parses_turn_tokens() {
        let data = json!({
            "time": {"created": 1, "completed": 2},
            "model": {"id": "deepseek-v4.1-flash-free"},
            "content": [{"type": "text", "text": "hi"}],
            "cost": 0.012,
            "tokens": {"input": 1648, "output": 470, "reasoning": 3491, "cache": {"read": 174080, "write": 12}}
        });
        let record = map_assistant_record("m1".to_string(), 5, &data).expect("record");
        let tokens = record.tokens.expect("应有 token");
        assert_eq!(tokens.input, 1648);
        assert_eq!(tokens.cache_read, 174080);
        assert_eq!(tokens.cache_write, 12);
        assert_eq!(tokens.output, 470);
        assert_eq!(tokens.reasoning, 3491);
        assert_eq!(tokens.cost, Some(0.012));
        assert!(tokens.context_window.is_none());
    }

    #[test]
    fn assistant_record_without_tokens_is_none() {
        let data = json!({
            "time": {"created": 1, "completed": 2},
            "content": [{"type": "text", "text": "hi"}]
        });
        let record = map_assistant_record("m1".to_string(), 5, &data).expect("record");
        assert!(record.tokens.is_none());
    }

    #[test]
    fn assistant_record_maps_parts_and_completion() {
        let data = json!({
            "time": {"created": 1, "completed": 2},
            "model": {"id": "deepseek-v4.1-flash-free", "providerID": "opencode", "variant": "max"},
            "content": [
                {"type": "reasoning", "text": "think…"},
                {"type": "text", "text": "hello"},
                {"type": "tool", "id": "call_1", "name": "read", "state": {"status": "completed", "input": {"path": "/tmp/x"}}},
                {"type": "unknown", "payload": 1}
            ]
        });
        let record = map_assistant_record("m1".to_string(), 5, &data).expect("record");
        assert!(record.completed);
        assert_eq!(record.role, "assistant");
        assert_eq!(record.model.as_deref(), Some("deepseek-v4.1-flash-free"));
        assert_eq!(record.model_provider.as_deref(), Some("opencode"));
        assert_eq!(record.variant.as_deref(), Some("max"));
        assert_eq!(record.parts.len(), 3);
        assert!(matches!(record.parts[0], TimelinePart::Reasoning { .. }));
        assert!(matches!(record.parts[1], TimelinePart::Text { .. }));
        match &record.parts[2] {
            TimelinePart::Tool {
                name,
                status,
                summary,
                call_id,
            } => {
                assert_eq!(name, "read");
                assert_eq!(status.as_deref(), Some("completed"));
                assert_eq!(summary.as_deref(), Some("/tmp/x"));
                assert_eq!(call_id, "call_1");
            }
            other => panic!("expected tool part, got {other:?}"),
        }
    }

    #[test]
    fn assistant_record_marks_unfinished_as_incomplete() {
        let data = json!({
            "time": {"created": 1},
            "content": [{"type": "text", "text": "半句"}]
        });
        let record = map_assistant_record("m1".to_string(), 0, &data).expect("record");
        assert!(!record.completed);
    }

    #[test]
    fn user_record_uses_text_field() {
        let data = json!({"text": "你好", "metadata": {"displayText": "ignored"}});
        let record = map_user_record("m1".to_string(), 7, &data).expect("record");
        assert_eq!(record.role, "user");
        match &record.parts[0] {
            TimelinePart::Text { text } => assert_eq!(text, "你好"),
            other => panic!("expected text part, got {other:?}"),
        }
    }

    #[test]
    fn user_record_skips_empty_text() {
        let data = json!({"text": "   "});
        assert!(map_user_record("m1".to_string(), 7, &data).is_none());
    }

    #[test]
    fn tool_error_extracts_object_message() {
        let state = json!({"error": {"type": "tool.execution", "message": "timed out"}});
        assert_eq!(extract_tool_error(&state).as_deref(), Some("timed out"));
    }

    #[test]
    fn tool_error_ignores_null_and_blank() {
        assert_eq!(extract_tool_error(&json!({"error": null})), None);
        assert_eq!(extract_tool_error(&json!({"error": "  "})), None);
        assert_eq!(extract_tool_error(&json!({})), None);
    }

    #[test]
    fn tool_output_joins_content_texts() {
        let state = json!({
            "content": [
                {"type": "text", "text": "第一行"},
                {"type": "text", "text": "第二行"}
            ]
        });
        assert_eq!(extract_tool_output(&state).as_deref(), Some("第一行\n第二行"));
        assert_eq!(
            extract_tool_output(&json!({"output": "直接输出"})).as_deref(),
            Some("直接输出")
        );
        assert_eq!(extract_tool_output(&json!({})), None);
    }

    #[test]
    fn truncate_long_marks_oversize() {
        let (text, truncated) = truncate_long("abcdef", 3);
        assert_eq!(text.as_deref(), Some("abc"));
        assert!(truncated);

        let (text, truncated) = truncate_long("abc", 3);
        assert_eq!(text.as_deref(), Some("abc"));
        assert!(!truncated);
    }

    /// 依赖本机真实 OpenCode 数据的冒烟测试：`cargo test -- --ignored`
    #[test]
    #[ignore]
    fn reads_real_database() {
        let Some(db_path) = crate::engines::opencode::locator::database_path() else {
            panic!("未找到本机 opencode.db");
        };
        let projects = list_projects(&db_path).expect("list_projects");
        assert!(!projects.is_empty(), "至少应有一个含会话的项目");
        println!("项目数: {}", projects.len());

        let project = &projects[0];
        let sessions = list_sessions(&db_path, &project.id, 5).expect("list_sessions");
        assert!(!sessions.is_empty(), "含会话的项目应有顶层会话");
        let session = &sessions[0];
        println!(
            "会话: {} 标题={:?} 模型={:?}",
            session.id, session.title, session.model
        );

        // 单会话读取：与列表结果一致；不存在的 id 返回 None。
        let one = get_session(&db_path, &session.id).expect("get_session");
        let one = one.expect("真实会话应能按 id 读到");
        assert_eq!(one.id, session.id);
        assert_eq!(one.title, session.title);
        assert!(get_session(&db_path, "ses_prism_not_exist")
            .expect("get_session missing")
            .is_none());

        let page = read_timeline(&db_path, &session.id, None, 150).expect("read_timeline");
        println!(
            "时间线记录数: {} has_more={}",
            page.records.len(),
            page.has_more
        );
        for record in &page.records {
            assert!(record.role == "user" || record.role == "assistant");
        }
        // 最近一轮的模型上下文：界面用它回显「模型 · 档位」。
        // 这个会话可能还没有助手记录，找一个有的来核对。
        for candidate in list_sessions(&db_path, &project.id, 30).expect("probe sessions") {
            let probe = read_timeline(&db_path, &candidate.id, None, 150).expect("probe timeline");
            let Some(last) = probe.records.iter().rev().find(|record| record.model.is_some()) else {
                continue;
            };
            println!(
                "最近一轮模型: {:?} / provider={:?} / 档位={:?}",
                last.model, last.model_provider, last.variant
            );
            assert!(
                last.model_provider.is_some(),
                "OpenCode 的模型记录应带 providerID"
            );
            break;
        }

        // 分页：翻到更早一页，两页不重叠且时间有序。
        if page.has_more {
            let cursor = page.next_cursor.expect("有更早消息时应有游标");
            let older =
                read_timeline(&db_path, &session.id, Some(cursor), 150).expect("older page");
            println!("更早一页: {} 条", older.records.len());
            let page_ids: std::collections::HashSet<&str> =
                page.records.iter().map(|r| r.id.as_str()).collect();
            assert!(older
                .records
                .iter()
                .all(|r| !page_ids.contains(r.id.as_str())));
            if let (Some(first_of_page), Some(last_of_older)) =
                (page.records.first(), older.records.last())
            {
                assert!(last_of_older.time_created <= first_of_page.time_created);
            }
        }

        // 工具详情：找一个工具调用，验证按需加载。
        let found = page.records.iter().find_map(|record| {
            record.parts.iter().find_map(|part| match part {
                TimelinePart::Tool { call_id, .. } => Some((record.id.clone(), call_id.clone())),
                _ => None,
            })
        });
        if let Some((message_id, call_id)) = found {
            let detail =
                read_tool_detail(&db_path, &session.id, &message_id, &call_id).expect("tool detail");
            println!(
                "工具详情: input={} output_len={:?} error={}",
                detail.input.is_some(),
                detail.output.as_ref().map(|text| text.len()),
                detail.error.is_some()
            );
            assert!(
                detail.input.is_some()
                    || detail.output.is_some()
                    || detail.error.is_some()
                    || detail.metadata.is_some()
            );
        }
    }
}

#[cfg(test)]
mod ledger_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn edit_uses_metadata_files_with_patch_counts() {
        let data = json!({
            "content": [{
                "type": "tool",
                "id": "call_1",
                "name": "edit",
                "state": {
                    "status": "completed",
                    "input": {
                        "path": "/repo/src/a.ts",
                        "oldString": "const x = 1",
                        "newString": "const x = 2\nconst y = 3"
                    },
                    "metadata": { "files": [
                        {"file": "src/a.ts", "status": "modified", "additions": 2, "deletions": 1,
                         "patch": "--- a/src/a.ts\n+++ b/src/a.ts\n@@\n-const x = 1\n+const x = 2\n+const y = 3"},
                        {"file": "src/new.ts", "status": "added", "additions": 4, "deletions": 0, "patch": ""}
                    ]}
                }
            }]
        });
        let events = ledger_events_from_message("msg_1", 1234, &data);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].path, "src/a.ts");
        assert_eq!(events[0].kind, "edit");
        assert_eq!((events[0].added, events[0].removed), (2, 1));
        assert_eq!(events[0].record_id, "msg_1");
        assert_eq!(events[0].call_id.as_deref(), Some("call_1"));
        assert_eq!(events[1].kind, "write", "added 状态归 write");
    }

    #[test]
    fn edit_without_metadata_falls_back_to_input_lines() {
        let data = json!({
            "content": [{
                "type": "tool",
                "id": "call_2",
                "name": "edit",
                "state": { "input": { "path": "src/b.ts", "oldString": "a\nb", "newString": "a\nb\nc" } }
            }]
        });
        let events = ledger_events_from_message("msg_2", 10, &data);
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].added, events[0].removed), (3, 2));
        assert_eq!(events[0].kind, "edit");
    }

    #[test]
    #[ignore]
    fn reads_real_ledger_from_local_db() {
        let Some(db_path) = crate::engines::opencode::locator::database_path() else {
            panic!("未找到本机 opencode.db");
        };
        let projects = list_projects(&db_path).expect("list_projects");
        let project = projects
            .iter()
            .find(|item| item.id.contains("prism"))
            .unwrap_or(&projects[0]);
        let sessions = list_sessions(&db_path, &project.id, 40).expect("list_sessions");
        let mut best: Option<(String, usize, i64)> = None;
        for session in sessions {
            let events = read_ledger(&db_path, &session.id).expect("read_ledger");
            let changed = events.iter().filter(|event| event.kind != "read").count();
            if best.as_ref().map_or(true, |(_, count, _)| changed > *count) {
                best = Some((session.id.clone(), changed, events.len() as i64));
            }
        }
        let (session_id, changed, total) = best.expect("至少一个会话");
        println!("会话 {session_id}: {total} 次文件操作（{changed} 次改动）");
        assert!(total > 0, "真机数据应能扫出文件操作");
    }

    #[test]
    fn write_counts_content_and_read_is_recorded() {
        let data = json!({
            "content": [
                {"type": "tool", "id": "c1", "name": "write",
                 "state": {"input": {"path": "docs/x.md", "content": "1\n2\n3"}}},
                {"type": "tool", "id": "c2", "name": "read",
                 "state": {"input": {"path": "src/c.ts"}}},
                {"type": "tool", "id": "c3", "name": "shell",
                 "state": {"input": {"command": "rm -rf /tmp/x"}}}
            ]
        });
        let events = ledger_events_from_message("msg_3", 77, &data);
        assert_eq!(events.len(), 2, "shell 不记账本");
        assert_eq!((events[0].kind.as_str(), events[0].added), ("write", 3));
        assert_eq!(events[1].kind, "read");
    }
}
