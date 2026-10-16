//! OpenCode 历史数据（opencode.db）的只读读取。
//!
//! 原则：一律以只读方式打开数据库；引擎原始数据不做任何写入，
//! Prism 的增值数据（标题、标签等）将来另行存放。

use std::path::Path;

use rusqlite::{params, Connection, OpenFlags};
use serde_json::Value;

use crate::engines::core::{ProjectItem, SessionSummary, TimelinePart, TimelineRecord};

type DbResult<T> = Result<T, String>;

const SESSION_LIMIT: i64 = 500;
const TIMELINE_LIMIT: i64 = 3000;

pub fn session_limit() -> i64 {
    SESSION_LIMIT
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
        .query_map(params![project_id, limit], |row| {
            let raw_model: Option<String> = row.get(4)?;
            Ok(SessionSummary {
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
        })
        .map_err(|cause| format!("查询会话失败：{cause}"))?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|cause| format!("读取会话失败：{cause}"))
}

/// 会话时间线：按 seq 顺序还原用户与助手的消息。
pub fn read_timeline(db_path: &Path, session_id: &str) -> DbResult<Vec<TimelineRecord>> {
    let connection = open_readonly(db_path)?;
    let mut statement = connection
        .prepare(
            "SELECT id, type, time_created, data
             FROM session_message
             WHERE session_id = ?1 AND type IN ('user', 'assistant')
             ORDER BY seq ASC
             LIMIT ?2",
        )
        .map_err(|cause| format!("查询消息失败：{cause}"))?;

    let rows = statement
        .query_map(params![session_id, TIMELINE_LIMIT], |row| {
            let id: String = row.get(0)?;
            let kind: String = row.get(1)?;
            let time_created: i64 = row.get(2)?;
            let data: String = row.get(3)?;
            Ok((id, kind, time_created, data))
        })
        .map_err(|cause| format!("查询消息失败：{cause}"))?;

    let mut records = Vec::new();
    for row in rows {
        let (id, kind, time_created, data) =
            row.map_err(|cause| format!("读取消息失败：{cause}"))?;
        let Ok(value) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        let record = match kind.as_str() {
            "user" => map_user_record(id, time_created, &value),
            "assistant" => map_assistant_record(id, time_created, &value),
            _ => None,
        };
        if let Some(record) = record {
            records.push(record);
        }
    }
    Ok(records)
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
    let text = data
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| data.pointer("/metadata/displayText").and_then(Value::as_str))
        .unwrap_or("");
    if text.trim().is_empty() {
        return None;
    }
    Some(TimelineRecord {
        id,
        role: "user".to_string(),
        time_created,
        model: None,
        completed: true,
        parts: vec![TimelinePart::Text {
            text: text.to_string(),
        }],
    })
}

fn map_assistant_record(id: String, time_created: i64, data: &Value) -> Option<TimelineRecord> {
    let completed = data.pointer("/time/completed").is_some();
    let model = data
        .pointer("/model/id")
        .and_then(Value::as_str)
        .map(str::to_string);

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
        completed,
        parts,
    })
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
    fn assistant_record_maps_parts_and_completion() {
        let data = json!({
            "time": {"created": 1, "completed": 2},
            "model": {"id": "deepseek-v4.1-flash-free"},
            "content": [
                {"type": "reasoning", "text": "think…"},
                {"type": "text", "text": "hello"},
                {"type": "tool", "name": "read", "state": {"status": "completed", "input": {"path": "/tmp/x"}}},
                {"type": "unknown", "payload": 1}
            ]
        });
        let record = map_assistant_record("m1".to_string(), 5, &data).expect("record");
        assert!(record.completed);
        assert_eq!(record.role, "assistant");
        assert_eq!(record.model.as_deref(), Some("deepseek-v4.1-flash-free"));
        assert_eq!(record.parts.len(), 3);
        assert!(matches!(record.parts[0], TimelinePart::Reasoning { .. }));
        assert!(matches!(record.parts[1], TimelinePart::Text { .. }));
        match &record.parts[2] {
            TimelinePart::Tool { name, status, summary } => {
                assert_eq!(name, "read");
                assert_eq!(status.as_deref(), Some("completed"));
                assert_eq!(summary.as_deref(), Some("/tmp/x"));
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

    /// 依赖本机真实 OpenCode 数据的冒烟测试：`cargo test -- --ignored`
    #[test]
    #[ignore]
    fn reads_real_database() {
        let Some(db_path) = crate::engines::opencode::locator::database_path() else {
            panic!("未找到本机 opencode.db");
        };
        let projects = list_projects(&db_path).expect("list_projects");
        assert!(!projects.is_empty(), "至少应有一个项目");
        println!("项目数: {}", projects.len());

        let Some(project) = projects.iter().find(|p| p.session_count > 0) else {
            println!("没有含会话的项目，跳过");
            return;
        };
        let sessions = list_sessions(&db_path, &project.id, 5).expect("list_sessions");
        assert!(!sessions.is_empty(), "含会话的项目应有顶层会话");
        let session = &sessions[0];
        println!(
            "会话: {} 标题={:?} 模型={:?}",
            session.id, session.title, session.model
        );

        let records = read_timeline(&db_path, &session.id).expect("read_timeline");
        println!("时间线记录数: {}", records.len());
        for record in &records {
            assert!(record.role == "user" || record.role == "assistant");
        }
        if !records.is_empty() {
            assert!(records.iter().any(|r| r.role == "user"));
        }
    }
}
