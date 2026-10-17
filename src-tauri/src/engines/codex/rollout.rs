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

use crate::engines::core::{
    TimelinePage, TimelinePart, TimelineRecord, ToolDetail, TurnTokens,
};
use crate::engines::time::parse_iso_ms;
use crate::ledger::RawEvent;

type ParseResult<T> = Result<T, String>;

/// 工具详情输出上限（字符），超出截断。
const TOOL_OUTPUT_LIMIT: usize = 200_000;
/// 工具摘要最大长度（字符）。
const SUMMARY_LIMIT: usize = 80;
/// 尾读 token_count 的最大扫描字节数（取最后一次统计）。
const TAIL_SCAN_BYTES: u64 = 256 * 1024;
/// 头读会话信息的最大扫描字节数（session_meta + 首个 turn_context）。
const HEAD_SCAN_BYTES: u64 = 256 * 1024;

/// 内部解析结果：seq = 行号（1 起），游标对齐 OpenCode 的分页语义。
struct ParsedRecord {
    seq: i64,
    time_created: i64,
    role: &'static str,
    parts: Vec<TimelinePart>,
    /// 该轮的模型与推理档位（取自同轮的 `turn_context`）。
    model: Option<String>,
    variant: Option<String>,
    /// 该轮最后一次模型调用的用量（token_count 事件回填）。
    tokens: Option<TurnTokens>,
}

/// 当前轮的模型上下文（`turn_context` 事件更新）。
#[derive(Debug, Clone, Default)]
struct TurnMeta {
    model: Option<String>,
    /// 推理强度（Codex 称 effort）。
    variant: Option<String>,
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
                model: record.model,
                model_provider: None,
                variant: record.variant,
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
    // 当前轮的模型上下文：每次 `turn_context` 覆盖，随记录一起落到时间线上。
    let mut turn = TurnMeta::default();
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
            ("turn_context", _) => {
                turn.model = payload
                    .get("model")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .map(str::to_string);
                turn.variant = payload
                    .get("effort")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .map(str::to_string);
            }
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
                        model: turn.model.clone(),
                        variant: turn.variant.clone(),
                        tokens: None,
                    });
                }
            }
            ("response_item", Some("reasoning")) => {
                let text = reasoning_text(payload);
                if !text.is_empty() {
                    ensure_current(&mut current, seq, time_ms, &turn)
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
                        ensure_current(&mut current, seq, time_ms, &turn)
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
                let record = ensure_current(&mut current, seq, time_ms, &turn);
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
            ("event_msg", Some("token_count")) => {
                // 回填到当前助手记录：一个回合内可能多次上报，取最后一次。
                let Some(info) = payload.get("info") else {
                    continue;
                };
                let Some(tokens) = turn_tokens_from_info(info) else {
                    continue;
                };
                if let Some(record) = current.as_mut() {
                    if !record.parts.is_empty() {
                        record.tokens = Some(tokens);
                    }
                }
            }
            _ => {}
        }
    }

    flush(&mut records, &mut current);
    Ok(records)
}

/// `payload.info` → 本轮用量（last_token_usage；缓存读从输入中扣除）。
///
/// 只认 last_token_usage：它是当轮调用的真实用量；旧版 rollout 缺该字段时
/// 本轮不回填（宁缺毋错）。
fn turn_tokens_from_info(info: &Value) -> Option<TurnTokens> {
    let usage = info.get("last_token_usage")?;
    let get = |key: &str| {
        usage
            .get(key)
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .max(0)
    };
    let cached_input = get("cached_input_tokens");
    let tokens = TurnTokens {
        input: get("input_tokens").saturating_sub(cached_input),
        cache_read: cached_input,
        cache_write: 0,
        output: get("output_tokens"),
        reasoning: get("reasoning_output_tokens"),
        cost: None,
        context_window: info
            .get("model_context_window")
            .and_then(Value::as_i64)
            .filter(|value| *value > 0),
    };
    if tokens.is_empty() {
        None
    } else {
        Some(tokens)
    }
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
    turn: &TurnMeta,
) -> &'a mut ParsedRecord {
    if current.is_none() {
        *current = Some(ParsedRecord {
            seq,
            time_created,
            role: "assistant",
            parts: Vec::new(),
            model: turn.model.clone(),
            variant: turn.variant.clone(),
            tokens: None,
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

/// rollout 尾部的 token_count 明细（累计值；缓存读是输入的子集）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenCount {
    /// 事件时间戳（毫秒）；缺失或解析失败为 0。
    pub at_ms: i64,
    /// 累计输入（OpenAI 语义：含缓存命中部分）。
    pub input: i64,
    /// 累计输入中命中缓存的部分。
    pub cached_input: i64,
    /// 累计输出 / 累计思考。
    pub output: i64,
    pub reasoning: i64,
}

/// 从 rollout 尾部读最后一次 token_count 明细。
pub fn tail_token_count(path: &Path) -> Option<TokenCount> {
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
        let get = |key: &str| usage.get(key).and_then(Value::as_i64).unwrap_or(0);
        let at_ms = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_iso_ms)
            .unwrap_or(0);
        return Some(TokenCount {
            at_ms,
            input: get("input_tokens"),
            cached_input: get("cached_input_tokens"),
            output: get("output_tokens"),
            reasoning: get("reasoning_output_tokens"),
        });
    }
    None
}

/// 从 rollout 尾部读最后一次 token_count（输入 / 输出 / 思考；兼容旧调用）。
pub fn tail_token_usage(path: &Path) -> Option<(i64, i64, i64)> {
    tail_token_count(path).map(|count| (count.input, count.output, count.reasoning))
}

/// 从 rollout 头部读会话信息 `(cwd, model)`；模型取首个 turn_context，缺失记「未知」。
pub fn head_session_info(path: &Path) -> Option<(String, String)> {
    let file = File::open(path).ok()?;
    let mut buffer = String::new();
    file.take(HEAD_SCAN_BYTES).read_to_string(&mut buffer).ok()?;

    let mut cwd: Option<String> = None;
    let mut model: Option<String> = None;
    for line in buffer.lines() {
        if cwd.is_none() && line.contains("session_meta") {
            if let Ok(value) = serde_json::from_str::<Value>(line) {
                cwd = value
                    .pointer("/payload/cwd")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                    .map(str::to_string);
            }
        }
        if model.is_none() && line.contains("turn_context") {
            if let Ok(value) = serde_json::from_str::<Value>(line) {
                model = value
                    .pointer("/payload/model")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                    .map(str::to_string);
            }
        }
        if cwd.is_some() && model.is_some() {
            break;
        }
    }
    let cwd = cwd?;
    Some((cwd, model.unwrap_or_else(|| "未知".to_string())))
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
    fn attaches_last_token_usage_to_current_turn() {
        let fixture = concat!(
            r#"{"timestamp":"2026-06-11T13:04:07.400Z","type":"event_msg","payload":{"type":"user_message","message":"你好"}}"#,
            "\n",
            r#"{"timestamp":"2026-06-11T13:04:11.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"你好呀"}]}}"#,
            "\n",
            r#"{"timestamp":"2026-06-11T13:04:12.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1500,"cached_input_tokens":400,"output_tokens":120,"reasoning_output_tokens":30},"last_token_usage":{"input_tokens":1500,"cached_input_tokens":400,"output_tokens":120,"reasoning_output_tokens":30},"model_context_window":272000}}}"#,
            "\n",
            r#"{"timestamp":"2026-06-11T13:04:20.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":2000,"cached_input_tokens":500,"output_tokens":200,"reasoning_output_tokens":50},"last_token_usage":{"input_tokens":500,"cached_input_tokens":100,"output_tokens":80,"reasoning_output_tokens":20},"model_context_window":272000}}}"#,
            "\n",
        );
        let records = parse_reader(BufReader::new(Cursor::new(fixture))).expect("解析");
        let tokens = records[1].tokens.expect("应回填 token");
        // 一个回合内可能多次上报：取最后一次的 last_token_usage。
        assert_eq!(tokens.input, 400, "500-100=400（未缓存输入）");
        assert_eq!(tokens.cache_read, 100);
        assert_eq!(tokens.output, 80);
        assert_eq!(tokens.reasoning, 20);
        assert_eq!(tokens.context_window, Some(272000));
    }

    #[test]
    fn token_count_without_last_usage_attaches_nothing() {
        let records = parse_fixture();
        assert!(records[1].tokens.is_none(), "只有累计值时本轮不回填");
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
                    model: record.model,
                    variant: record.variant,
                    tokens: record.tokens,
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
    fn turn_context_fills_model_and_effort() {
        let text = concat!(
            r#"{"timestamp":"2026-06-11T13:04:07.400Z","type":"turn_context","payload":{"model":"gpt-5.2","effort":"high"}}"#,
            "\n",
            r#"{"timestamp":"2026-06-11T13:04:07.500Z","type":"event_msg","payload":{"type":"user_message","message":"hi"}}"#,
            "\n",
            r#"{"timestamp":"2026-06-11T13:04:08.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}}"#,
            "\n",
        );
        let records = parse_reader(BufReader::new(Cursor::new(text))).expect("解析");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].model.as_deref(), Some("gpt-5.2"));
        assert_eq!(records[0].variant.as_deref(), Some("high"));

        let page = render_page(records, None, 10);
        assert_eq!(page.records[1].model.as_deref(), Some("gpt-5.2"));
        assert_eq!(page.records[1].variant.as_deref(), Some("high"));
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

    #[test]
    fn tail_token_count_and_head_info() {
        let dir = std::env::temp_dir().join(format!("prism-rollout-usage-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("临时目录");
        let path = dir.join("rollout.jsonl");
        let fixture = concat!(
            r#"{"timestamp":"2026-06-11T13:04:07.118Z","type":"session_meta","payload":{"id":"t1","cwd":"/tmp/demo","cli_version":"0.133.0"}}"#,
            "\n",
            r#"{"timestamp":"2026-06-11T13:04:08.000Z","type":"turn_context","payload":{"model":"gpt-5-codex"}}"#,
            "\n",
            r#"{"timestamp":"2026-06-11T13:05:00.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"cached_input_tokens":400,"output_tokens":50,"reasoning_output_tokens":5,"total_tokens":1055}}}}"#,
            "\n",
            r#"{"timestamp":"2026-06-11T13:09:00.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":2000,"cached_input_tokens":800,"output_tokens":90,"reasoning_output_tokens":12,"total_tokens":2090}}}}"#,
            "\n"
        );
        std::fs::write(&path, fixture).expect("写 fixture");

        let count = tail_token_count(&path).expect("明细");
        assert_eq!(count.input, 2000);
        assert_eq!(count.cached_input, 800);
        assert_eq!(count.output, 90);
        assert_eq!(count.reasoning, 12);
        assert!(count.at_ms > 0, "时间戳应可解析");

        let (cwd, model) = head_session_info(&path).expect("头部信息");
        assert_eq!(cwd, "/tmp/demo");
        assert_eq!(model, "gpt-5-codex");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ---------------- 文件账本 ----------------

/// V4A patch 的一节头部：`*** Update File: path`（容忍结尾的 ` ***` 装饰）。
fn patch_section_header(line: &str) -> Option<(&'static str, String)> {
    let rest = line.strip_prefix("*** ")?;
    let rest = rest.trim_end_matches("***").trim();
    let (label, path) = rest.split_once(':')?;
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    let kind = match label.trim() {
        "Update File" => "update",
        "Add File" | "Create File" => "add",
        "Delete File" => "delete",
        "Move to" => "move",
        _ => return None,
    };
    Some((kind, path.to_string()))
}

/// patch 文本 → 逐文件事件（一节一文件；`+`/`-` 行计增删）。
pub fn apply_patch_events(patch: &str, record_id: &str, time: i64) -> Vec<RawEvent> {
    let mut events: Vec<RawEvent> = Vec::new();
    let mut open = true;
    for line in patch.lines() {
        let line = line.trim_end();
        if line.starts_with("***") {
            if line == "*** End Patch" {
                open = false;
            } else if let Some((kind, path)) = patch_section_header(line) {
                let normalized = match kind {
                    "add" => "write",
                    "delete" => "delete",
                    // move 视为对目标路径的一次编辑。
                    _ => "edit",
                };
                events.push(RawEvent {
                    path,
                    record_id: record_id.to_string(),
                    call_id: None,
                    time,
                    kind: normalized.to_string(),
                    tool: "apply_patch".to_string(),
                    added: 0,
                    removed: 0,
                });
                open = true;
            }
            // 其余 *** 元数据行（Description / Preview 等）：不改变当前段状态。
            continue;
        }
        let Some(last) = events.last_mut() else {
            continue;
        };
        if !open {
            continue;
        }
        if line.starts_with('+') && !line.starts_with("+++") {
            last.added += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            last.removed += 1;
        }
    }
    events
}

/// 从任意参数文本里切出 patch 段（Begin/End 优先；否则从首个段落头到结尾）。
fn extract_patch_text(raw: &str) -> Option<String> {
    let segment = if let Some(start) = raw.find("*** Begin Patch") {
        match raw[start..].find("*** End Patch") {
            Some(rel) => &raw[start..start + rel + "*** End Patch".len()],
            None => &raw[start..],
        }
    } else {
        let start = [
            "*** Update File:",
            "*** Add File:",
            "*** Create File:",
            "*** Delete File:",
            "*** Move to:",
        ]
        .iter()
        .filter_map(|marker| raw.find(marker))
        .min()?;
        &raw[start..]
    };
    // 嵌套一层 repr 时换行是转义态（\n 两个字符）：没有真实换行时还原。
    if segment.contains('\n') {
        Some(segment.to_string())
    } else {
        Some(segment.replace("\\n", "\n"))
    }
}

/// 递归找 patch 文本：字符串直接用，数组/对象按常见键下钻（depth 限 4 层）。
fn patch_from_value(value: &Value, depth: usize) -> Option<String> {
    if depth > 4 {
        return None;
    }
    match value {
        Value::String(text) => extract_patch_text(text),
        Value::Array(items) => items
            .iter()
            .find_map(|item| patch_from_value(item, depth + 1)),
        Value::Object(map) => ["content", "cmd", "command", "patch", "arguments"]
            .iter()
            .find_map(|key| map.get(*key).and_then(|inner| patch_from_value(inner, depth + 1))),
        _ => None,
    }
}

/// `apply_patch` 的调用参数（形态随 Codex 版本而变）→ patch 文本。
fn patch_from_arguments(arguments: &str) -> Option<String> {
    let value: Value = serde_json::from_str(arguments).ok()?;
    patch_from_value(&value, 0)
}

/// 经 shell 执行 `apply_patch`（heredoc）的命令 → patch 文本。
fn patch_from_command(arguments: &str) -> Option<String> {
    let value: Value = serde_json::from_str(arguments).ok()?;
    let cmd = value.get("cmd").and_then(Value::as_str)?;
    if !cmd.contains("apply_patch") {
        return None;
    }
    extract_patch_text(cmd)
}

/// 整个会话的账本事件（只扫 function_call；记录边界与 parse_reader 一致）。
pub fn read_ledger(path: &Path) -> ParseResult<Vec<RawEvent>> {
    let file = File::open(path).map_err(|cause| format!("打开 rollout 文件失败：{cause}"))?;
    let reader = BufReader::new(file);
    let mut events: Vec<RawEvent> = Vec::new();
    // 当前助手记录的首行 seq（与解析时间线的 ensure_current 语义一致）。
    let mut current_seq: Option<i64> = None;

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
                    current_seq = None;
                }
            }
            ("response_item", Some("reasoning")) => {
                if !reasoning_text(payload).is_empty() {
                    current_seq.get_or_insert(seq);
                }
            }
            ("response_item", Some("message")) => {
                if payload.get("role").and_then(Value::as_str) == Some("assistant")
                    && !message_text(payload).is_empty()
                {
                    current_seq.get_or_insert(seq);
                }
            }
            ("response_item", Some("function_call")) => {
                let name = payload
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool");
                let record_id = current_seq.get_or_insert(seq).to_string();
                let call_id = payload
                    .get("call_id")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string);
                let arguments = payload
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let patch = match name {
                    "apply_patch" => {
                        patch_from_arguments(arguments).or_else(|| patch_from_command(arguments))
                    }
                    "exec_command" => patch_from_command(arguments),
                    _ => None,
                };
                if let Some(patch) = patch {
                    for mut event in apply_patch_events(&patch, &record_id, time_ms) {
                        event.call_id = call_id.clone();
                        event.tool = name.to_string();
                        events.push(event);
                    }
                }
            }
            _ => {}
        }
    }

    Ok(events)
}

#[cfg(test)]
mod ledger_tests {
    use super::*;
    use serde_json::json;
    use std::io::Write as _;

    #[test]
    fn patch_events_count_lines_per_section() {
        let patch = "*** Update File: src/a.ts ***\n@@ ... @@ ctx\n-old\n+new\n+extra\n*** Add File: src/b.ts\n+one\n+two\n*** Delete File: src/c.ts\n";
        let events = apply_patch_events(patch, "12", 99);
        assert_eq!(events.len(), 3);
        assert_eq!((events[0].path.as_str(), events[0].kind.as_str()), ("src/a.ts", "edit"));
        assert_eq!((events[0].added, events[0].removed), (2, 1), "只计 +/- 行");
        assert_eq!((events[1].path.as_str(), events[1].kind.as_str()), ("src/b.ts", "write"));
        assert_eq!(events[1].added, 2);
        assert_eq!((events[2].path.as_str(), events[2].kind.as_str()), ("src/c.ts", "delete"));
        assert_eq!(events[0].record_id, "12");
    }

    #[test]
    fn apply_patch_with_cmd_arguments_is_parsed() {
        // 部分版本 name=apply_patch，但参数是 {"cmd": "apply_patch\n*** Begin Patch..."}
        let arguments = "{\"cmd\":\"apply_patch\\n*** Begin Patch\\n*** Update File: a.txt\\n-old\\n+new\\n*** End Patch\\n\"}";
        let patch = patch_from_arguments(arguments).expect("cmd 形态");
        let events = apply_patch_events(&patch, "5", 0);
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].added, events[0].removed), (1, 1));
    }

    #[test]
    fn command_key_with_repr_escaped_newlines() {
        // 实测形态：arguments.command 是一层 repr，换行以转义态出现，且无 apply_patch 字样。
        let arguments = "{\"command\":\"[{'command': 'apply_patch', 'content': '*** Begin Patch\\n*** Create File: a.swift\\n+import SwiftUI\\n*** End Patch'}]\"}";
        let patch = patch_from_arguments(arguments).expect("command 形态");
        let events = apply_patch_events(&patch, "9", 0);
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].path.as_str(), events[0].kind.as_str()), ("a.swift", "write"));
        assert_eq!(events[0].added, 1);
    }

    #[test]
    fn description_lines_do_not_close_section() {
        let patch = "*** Begin Patch\n*** Update File: a.py\n*** Description: 加类型注解\n+from typing import Optional\n-x = 1\n+x = 2\n*** End Patch";
        let events = apply_patch_events(patch, "3", 0);
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].added, events[0].removed), (2, 1), "元数据行不打断计数");
    }

    #[test]
    fn end_patch_stops_counting() {
        let patch = "*** Begin Patch\n*** Update File: a.txt\n+x\n*** End Patch\n+should-not-count\n";
        let events = apply_patch_events(patch, "1", 0);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].added, 1);
    }

    #[test]
    #[ignore]
    fn reads_real_ledger_from_local_rollouts() {
        let root = std::path::PathBuf::from(std::env::var("HOME").expect("HOME"))
            .join(".codex/sessions");
        fn collect(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect(&path, out);
                } else if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
                    out.push(path);
                }
            }
        }
        let mut files = Vec::new();
        collect(&root, &mut files);
        assert!(!files.is_empty(), "本机应有 Codex rollout");

        let mut changed = 0usize;
        let mut total = 0usize;
        for file in files {
            let events = read_ledger(&file).expect("read_ledger");
            changed += events.iter().filter(|event| event.kind != "read").count();
            total += events.len();
        }
        println!("Codex rollout 账本：{total} 次文件操作（{changed} 次改动）");
        assert!(total > 0, "至少一个 rollout 应有补丁操作");
    }

    #[test]
    fn read_ledger_walks_rollout_and_group_record_ids() {
        let dir = std::env::temp_dir().join(format!("prism-ledger-rollout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("临时目录");
        let path = dir.join("rollout.jsonl");
        let mut file = std::fs::File::create(&path).expect("建文件");
        let lines = [
            json!({"timestamp":"2026-10-01T10:00:00.000Z","type":"event_msg","payload":{"type":"user_message","message":"改一下配置"}}),
            json!({"timestamp":"2026-10-01T10:00:01.000Z","type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"看一下"}]}}),
            json!({"timestamp":"2026-10-01T10:00:02.000Z","type":"response_item","payload":{"type":"function_call","name":"apply_patch","call_id":"c1","arguments":"{\"patch\":\"*** Update File: cfg.toml\\n-x = 1\\n+x = 2\"}"}}),
            json!({"timestamp":"2026-10-01T10:00:03.000Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"c2","arguments":"{\"cmd\":\"apply_patch <<'PATCH'\\n*** Begin Patch\\n*** Add File: new.txt\\n+hi\\n*** End Patch\\nPATCH\"}"}}),
            json!({"timestamp":"2026-10-01T10:00:04.000Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"c3","arguments":"{\"cmd\":\"ls -la\"}"}}),
        ];
        for line in lines {
            writeln!(file, "{line}").expect("写行");
        }
        drop(file);

        let events = read_ledger(&path).expect("读账本");
        assert_eq!(events.len(), 2, "ls 不产生事件");
        assert_eq!(events[0].path, "cfg.toml");
        assert_eq!(events[0].record_id, "2", "助手记录首行 seq");
        assert_eq!(events[0].call_id.as_deref(), Some("c1"));
        assert_eq!(events[1].path, "new.txt");
        assert_eq!(events[1].call_id.as_deref(), Some("c2"));
        assert_eq!(events[1].kind, "write");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
