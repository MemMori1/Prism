//! Codex rollout（`rollout-*.jsonl`）解析：行 → 中立时间线。
//!
//! 格式（codex-cli 0.133 观察值，解析层保持宽容）：
//! - `session_meta`：会话 id / cwd / 版本（首行）
//! - `turn_context`：每轮模型等上下文
//! - `response_item`：`reasoning`（思考）/ `message`（assistant 正文）/
//!   `function_call` + `function_call_output`（工具调用与输出）
//! - `event_msg`：`user_message`（用户输入）；`agent_message` 等是正文副本（跳过）
//!
//! 记录切分：每条 `user_message` 一条用户记录；两条用户消息之间的所有
//! assistant 侧条目合并成一条助手记录（parts 保序）。

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

use crate::engines::core::{TimelinePage, TimelinePart, TimelineRecord, ToolDetail};

type ParseResult<T> = Result<T, String>;

/// 工具详情输出上限（字符），超出截断。
const TOOL_OUTPUT_LIMIT: usize = 200_000;
/// 工具摘要最大长度（字符）。
const SUMMARY_LIMIT: usize = 80;
/// 尾读 token_count 的最大扫描字节数（取最后一次统计）。
const TAIL_SCAN_BYTES: u64 = 256 * 1024;

/// 内部解析结果：seq = 行号（1 起），游标对齐 OpenCode 的分页语义。
struct ParsedRecord {
    seq: i64,
    time_created: i64,
    role: &'static str,
    parts: Vec<TimelinePart>,
}

/// 时间线分页：与 OpenCode 语义一致（`before_seq` 严格递减；正序返回）。
pub fn read_timeline(
    path: &Path,
    before_seq: Option<i64>,
    limit: i64,
) -> ParseResult<TimelinePage> {
    let file = File::open(path).map_err(|cause| format!("打开 rollout 文件失败：{cause}"))?;
    let records = parse_reader(BufReader::new(file))?;
    Ok(render_page(records, before_seq, limit))
}

/// 纯分页逻辑（独立出来便于测试）。
fn render_page(
    records: Vec<ParsedRecord>,
    before_seq: Option<i64>,
    limit: i64,
) -> TimelinePage {
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
                model: None,
                completed: true,
                parts: record.parts,
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
    // call_id → (记录下标, parts 下标)；用于把输出回填到对应工具卡片。
    let mut call_index: HashMap<String, (usize, usize)> = HashMap::new();

    for (index, line) in reader.lines().enumerate() {
        let line = line.map_err(|cause| format!("读取 rollout 失败：{cause}"))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
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
        let payload = value.get("payload").unwrap_or(&Value::Null);
        let payload_type = payload.get("type").and_then(Value::as_str);

        match (kind, payload_type) {
            ("event_msg", Some("user_message")) => {
                let text = payload
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim();
                if !text.is_empty() {
                    flush(&mut records, &mut current);
                    records.push(ParsedRecord {
                        seq,
                        time_created: time_ms,
                        role: "user",
                        parts: vec![TimelinePart::Text {
                            text: text.to_string(),
                        }],
                    });
                }
            }
            ("response_item", Some("reasoning")) => {
                let text = reasoning_text(payload);
                if !text.is_empty() {
                    ensure_current(&mut current, seq, time_ms)
                        .parts
                        .push(TimelinePart::Reasoning { text });
                }
            }
            ("response_item", Some("message")) => {
                // 只收 assistant 正文；developer / user 是上下文注入或与
                // event_msg/user_message 重复的内容。
                if payload.get("role").and_then(Value::as_str) == Some("assistant") {
                    let text = message_text(payload);
                    if !text.is_empty() {
                        ensure_current(&mut current, seq, time_ms)
                            .parts
                            .push(TimelinePart::Text { text });
                    }
                }
            }
            ("response_item", Some("function_call")) => {
                let name = payload
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_string();
                let call_id = payload
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let arguments = payload
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let summary = tool_summary(arguments);
                let record = ensure_current(&mut current, seq, time_ms);
                let part_index = record.parts.len();
                record.parts.push(TimelinePart::Tool {
                    name,
                    // 输出未到达前视为执行中（历史文件里 = 尚未完成 / 被中断）。
                    status: Some("running".to_string()),
                    summary,
                    call_id: call_id.clone(),
                });
                if !call_id.is_empty() {
                    call_index.insert(call_id, (records.len(), part_index));
                }
            }
            ("response_item", Some("function_call_output")) => {
                let call_id = payload
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let output = payload
                    .get("output")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if let Some((record_index, part_index)) = call_index.get(call_id).copied() {
                    let record = if record_index < records.len() {
                        records.get_mut(record_index)
                    } else {
                        current.as_mut()
                    };
                    if let Some(record) = record {
                        if let Some(TimelinePart::Tool { status, .. }) =
                            record.parts.get_mut(part_index)
                        {
                            *status = Some(output_status(output).to_string());
                        }
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
            parts: Vec::new(),
        });
    }
    current.as_mut().expect("刚插入的记录必然存在")
}

/// 思考内容：`summary[].text` 拼接（content 一般为空）。
fn reasoning_text(payload: &Value) -> String {
    let mut chunks: Vec<&str> = Vec::new();
    if let Some(summary) = payload.get("summary").and_then(Value::as_array) {
        for item in summary {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                if !text.trim().is_empty() {
                    chunks.push(text);
                }
            }
        }
    }
    chunks.join("\n\n")
}

/// assistant 正文：content[].text 拼接。
fn message_text(payload: &Value) -> String {
    let mut chunks: Vec<&str> = Vec::new();
    if let Some(content) = payload.get("content").and_then(Value::as_array) {
        for item in content {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                if !text.trim().is_empty() {
                    chunks.push(text);
                }
            }
        }
    }
    chunks.join("\n\n")
}

/// 工具卡片摘要：优先常见字段（cmd / path / …），取不到就压平 JSON。
pub fn tool_summary(arguments: &str) -> Option<String> {
    let value: Value = serde_json::from_str(arguments).ok()?;
    let pick = |key: &str| -> Option<String> {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map(str::to_string)
    };
    let text = pick("cmd")
        .or_else(|| pick("command"))
        .or_else(|| pick("path"))
        .or_else(|| pick("file_path"))
        .or_else(|| pick("query"))
        .or_else(|| pick("input"))
        .unwrap_or_else(|| arguments.to_string());
    Some(compact(&text, SUMMARY_LIMIT))
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

/// 从工具输出关键字推断状态：非 0 退出码 → error。
fn output_status(output: &str) -> &'static str {
    if let Some(rest) = output.split("Process exited with code ").nth(1) {
        let code: i32 = rest
            .split_whitespace()
            .next()
            .and_then(|text| text.parse().ok())
            .unwrap_or(0);
        if code != 0 {
            return "error";
        }
    }
    "completed"
}

/// 单个工具调用的完整详情（展开卡片时按需读取；全文件扫描 path 限定）。
pub fn read_tool_detail(path: &Path, _message_id: &str, call_id: &str) -> ParseResult<ToolDetail> {
    let file = File::open(path).map_err(|cause| format!("打开 rollout 文件失败：{cause}"))?;
    let reader = BufReader::new(file);

    let mut input: Option<Value> = None;
    let mut output: Option<String> = None;

    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if !line.contains(call_id) {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let payload = value.get("payload").unwrap_or(&Value::Null);
        let same_call = payload.get("call_id").and_then(Value::as_str) == Some(call_id);
        if !same_call {
            continue;
        }
        match payload.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                input = payload
                    .get("arguments")
                    .and_then(Value::as_str)
                    .and_then(|text| serde_json::from_str(text).ok());
            }
            Some("function_call_output") => {
                output = payload
                    .get("output")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            _ => {}
        }
        if input.is_some() && output.is_some() {
            break;
        }
    }

    let truncated = output
        .as_ref()
        .map(|text| text.chars().count() > TOOL_OUTPUT_LIMIT)
        .unwrap_or(false);
    Ok(ToolDetail {
        input,
        output: output.map(|text| truncate_chars(&text, TOOL_OUTPUT_LIMIT)),
        error: None,
        metadata: None,
        truncated,
    })
}

fn truncate_chars(text: &str, limit: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= limit {
        return text.to_string();
    }
    chars[..limit].iter().collect()
}

/// 从 rollout 尾部读最后一次 token_count（四类用量）。
pub fn tail_token_usage(path: &Path) -> Option<(i64, i64, i64)> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_SCAN_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buffer = String::new();
    file.take(TAIL_SCAN_BYTES).read_to_string(&mut buffer).ok()?;

    for line in buffer.lines().rev() {
        if !line.contains("token_count") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(usage) = value.pointer("/payload/info/total_token_usage") else {
            continue;
        };
        let input = usage
            .get("input_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let output = usage
            .get("output_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let reasoning = usage
            .get("reasoning_output_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        return Some((input, output, reasoning));
    }
    None
}

/// RFC3339 → 毫秒时间戳（宽容：缺毫秒按 0；支持 Z 与 ±HH:MM 时区）。
pub fn parse_iso_ms(text: &str) -> Option<i64> {
    if text.len() < 19 {
        return None;
    }
    let year: i64 = text.get(0..4)?.parse().ok()?;
    let month: i64 = text.get(5..7)?.parse().ok()?;
    let day: i64 = text.get(8..10)?.parse().ok()?;
    let hour: i64 = text.get(11..13)?.parse().ok()?;
    let minute: i64 = text.get(14..16)?.parse().ok()?;
    let second: i64 = text.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let millis: i64 = text
        .get(19..20)
        .filter(|sep| *sep == ".")
        .and_then(|_| text.get(20..23))
        .and_then(|fraction| fraction.parse().ok())
        .unwrap_or(0);
    let offset_minutes: i64 = match text.get(19..20) {
        Some("+") | Some("-") => {
            let sign = if text.get(19..20) == Some("+") { 1 } else { -1 };
            let hh: i64 = text.get(20..22)?.parse().ok()?;
            let mm: i64 = text.get(23..25)?.parse().ok()?;
            sign * (hh * 60 + mm)
        }
        _ => 0,
    };
    let days = days_from_civil(year, month, day);
    Some((days * 86_400 + hour * 3600 + minute * 60 + second - offset_minutes * 60) * 1000 + millis)
}

/// 天数差（Howard Hinnant days_from_civil，1970-01-01 为 0）。
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const FIXTURE: &str = r#"{"timestamp":"2026-06-11T13:04:07.003Z","type":"session_meta","payload":{"id":"t1","cwd":"/tmp/x"}}
{"timestamp":"2026-06-11T13:04:07.100Z","type":"event_msg","payload":{"type":"task_started","turn_id":"a"}}
{"timestamp":"2026-06-11T13:04:07.200Z","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<permissions instructions>"}]}}
{"timestamp":"2026-06-11T13:04:07.300Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>"}]}}
{"timestamp":"2026-06-11T13:04:07.400Z","type":"event_msg","payload":{"type":"user_message","message":"帮我跑个脚本"}}
{"timestamp":"2026-06-11T13:04:08.000Z","type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"先写文件再跑"}]}}
{"timestamp":"2026-06-11T13:04:09.000Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\": \"cd /tmp/x && python3 run.py 2>&1\"}","call_id":"call_1"}}
{"timestamp":"2026-06-11T13:04:10.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_1","output":"Process exited with code 2\nOutput:\nboom"}}
{"timestamp":"2026-06-11T13:04:11.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"已经跑过了。"}]}}
{"timestamp":"2026-06-11T13:04:11.500Z","type":"event_msg","payload":{"type":"agent_message","message":"已经跑过了。"}}
{"timestamp":"2026-06-11T13:04:12.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":20,"reasoning_output_tokens":5}}}}
{"timestamp":"2026-06-11T13:04:30.000Z","type":"event_msg","payload":{"type":"user_message","message":"再来一次"}}
{"timestamp":"2026-06-11T13:04:31.000Z","type":"response_item","payload":{"type":"function_call","name":"apply_patch","arguments":"{\"input\":\"*** Begin Patch\\n*** Update File: run.py\"}","call_id":"call_2"}}
{"timestamp":"2026-06-11T13:04:32.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_2","output":"Success"}}
"#;

    fn parse_fixture() -> Vec<ParsedRecord> {
        parse_reader(BufReader::new(Cursor::new(FIXTURE))).expect("解析 fixture")
    }

    #[test]
    fn groups_records_and_parts() {
        let records = parse_fixture();
        assert_eq!(records.len(), 4, "用户×2 + 助手×2");
        assert_eq!(records[0].role, "user");
        let assistant = &records[1];
        assert_eq!(assistant.role, "assistant");
        assert_eq!(assistant.parts.len(), 3, "思考 + 工具 + 正文");
        assert!(matches!(assistant.parts[0], TimelinePart::Reasoning { .. }));
        match &assistant.parts[1] {
            TimelinePart::Tool {
                name,
                status,
                summary,
                call_id,
            } => {
                assert_eq!(name, "exec_command");
                assert_eq!(status.as_deref(), Some("error"), "退出码 2 → error");
                assert_eq!(summary.as_deref(), Some("cd /tmp/x && python3 run.py 2>&1"));
                assert_eq!(call_id, "call_1");
            }
            other => panic!("应为工具 part：{other:?}"),
        }
        assert!(matches!(assistant.parts[2], TimelinePart::Text { .. }));
        // agent_message 是副本，不应再出现一次正文。
        let text_parts = assistant
            .parts
            .iter()
            .filter(|part| matches!(part, TimelinePart::Text { .. }))
            .count();
        assert_eq!(text_parts, 1);
    }

    #[test]
    fn renders_page_with_cursor() {
        let records = parse_fixture(); // seq: 5(用户) 6-9(助手) 12(用户) 13-14(助手)
        let page = render_page(
            records
                .into_iter()
                .map(|record| ParsedRecord {
                    seq: record.seq,
                    time_created: record.time_created,
                    role: record.role,
                    parts: record.parts,
                })
                .collect(),
            None,
            2,
        );
        assert!(page.has_more);
        assert_eq!(page.records.len(), 2);
        assert_eq!(page.next_cursor, Some(12));
        assert_eq!(page.records[0].role, "user");
        assert_eq!(page.records[1].role, "assistant");
    }

    #[test]
    fn second_page_uses_cursor() {
        let records = parse_fixture();
        let page = render_page(records, Some(12), 10);
        assert!(!page.has_more);
        assert_eq!(page.records.len(), 2);
        assert_eq!(page.next_cursor, None);
        assert!(page.records[0].id == "5");
    }

    #[test]
    fn parses_iso_timestamps() {
        assert_eq!(parse_iso_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso_ms("2026-06-11T13:04:07.118Z"), Some(1_781_183_047_118));
        assert_eq!(parse_iso_ms("2026-06-11T21:04:07+08:00"), Some(1_781_183_047_000));
        assert_eq!(parse_iso_ms("2026-06-11T13:04:07"), Some(1_781_183_047_000));
        assert_eq!(parse_iso_ms("bad"), None);
    }

    #[test]
    fn tool_summary_prefers_cmd_then_path() {
        assert_eq!(
            tool_summary("{\"cmd\":\"ls  -la\"}").as_deref(),
            Some("ls -la")
        );
        assert_eq!(
            tool_summary("{\"path\":\"/tmp/a b.md\"}").as_deref(),
            Some("/tmp/a b.md")
        );
        let compacted = tool_summary("{\"input\":\"*** Begin Patch\\n*** Update File: run.py\"}")
            .expect("摘要");
        assert!(compacted.contains("*** Begin Patch"));
        assert_eq!(tool_summary("not json"), None);
    }

    #[test]
    fn tool_detail_reads_call_and_output() {
        let dir = std::env::temp_dir().join(format!("prism-rollout-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("临时目录");
        let path = dir.join("rollout.jsonl");
        std::fs::write(&path, FIXTURE).expect("写 fixture");
        let detail = read_tool_detail(&path, "7", "call_1").expect("详情");
        assert_eq!(
            detail.input.as_ref().and_then(|v| v.get("cmd")).and_then(|v| v.as_str()),
            Some("cd /tmp/x && python3 run.py 2>&1")
        );
        assert!(detail.output.as_deref().unwrap_or_default().contains("boom"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tail_token_usage_reads_last_count() {
        let dir = std::env::temp_dir().join(format!("prism-rollout-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("临时目录");
        let path = dir.join("rollout.jsonl");
        std::fs::write(&path, FIXTURE).expect("写 fixture");
        assert_eq!(tail_token_usage(&path), Some((100, 20, 5)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
