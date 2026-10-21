//! dsh 会话日志解析：`<DSH_HOME>/sessions/<编码 cwd>/<会话 id>/session.vN.jsonl.zstd`。
//!
//! 格式是 append-only 的 zstd 压缩 JSONL，但不是"整文件一个压缩帧"——dsh 每次
//! flush 追加一帧，所以必须用能连续解多帧的解码器（`zstd::Decoder` 默认就是）。
//! 这一点如果搞错，只会读到第一行 header，看起来像"日志是空的"。
//!
//! 每行的形状（实测 v4）：
//!
//! ```text
//! {"type":"session","version":4,"id":"...","createdAt":...,"cwd":"/tmp"}   ← 首行 header
//! {"type":"user/message","seq":8,"time":...,"data":{"content":[...],"source":{"kind":"user"},"id":"..."}}
//! {"type":"request/context","seq":11,"data":{"provider":"...","model":"...","contextWindow":1000000}}
//! {"type":"tool/call","seq":a,"data":{"callId":"...","name":"bash","arguments":"{...}"}}
//! {"type":"tool/result","seq":b,"data":{"message":{"source":{"callId":"..."},"content":[...],"isError":false},"error":...}}
//! {"type":"assistant/message","seq":c,"data":{"message":{"id":"...","content":[{"type":"text","text":"PONG"}],
//!   "source":{"kind":"model","provider":"...","model":"..."}},"usage":{"inputTokens":5667,...}}}
//! {"type":"session/title","seq":12,"data":{"title":"...","source":{"kind":"fallback"}}}
//! ```
//!
//! 两条容易踩的坑：
//!
//! 1. 只有 `user/message` 里 `source.kind == "user"` 的才是人说的话；其余是运行时
//!    上下文注入（工作目录、文件策略、goal 提醒），不该出现在对话里。
//! 2. **工具调用不在 `assistant/message` 的 content 里**，是独立的 `tool/call` +
//!    `tool/result` 事件。要把它们按 seq 插回助手消息，否则界面上工具卡片会消失。

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use super::{locator, project_id};
use crate::engines::core::{
    ProjectItem, SessionSummary, TimelinePage, TimelinePart, TimelineRecord, ToolDetail, TurnTokens,
};

/// 会话日志文件名：`session.v4.jsonl.zstd`（版本号会涨，别写死）。
const SESSION_FILE_PREFIX: &str = "session.v";
const SESSION_FILE_SUFFIX: &str = ".jsonl.zstd";

/// 一份日志的摘要（扫描阶段就够填列表，不必解全量）。
#[derive(Debug, Clone)]
pub struct SessionMeta {
    pub id: String,
    pub cwd: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub title: Option<String>,
    pub model: Option<String>,
}

/// 扫描全部会话日志（最新在前）。
///
/// 分两步：先只走目录拿到路径 + mtime 排序，再按需解压最靠前的若干份。
/// 直接"见一个解一个"会在会话多的时候把启动拖成好几秒——解压是这里唯一的
/// 重活儿，不能为了一份排不进前 500 的老日志白干。
pub fn scan(limit: usize) -> Vec<SessionMeta> {
    let Some(root) = locator::sessions_dir().filter(|dir| dir.is_dir()) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    collect(&root, 0, &mut paths);
    // 先按 mtime 粗排（不用解压），再解压到够数为至。
    paths.sort_by_key(|path| std::cmp::Reverse(file_mtime(path)));
    let mut found = Vec::new();
    for path in paths {
        if found.len() >= limit {
            break;
        }
        if let Some(meta) = read_meta(&path) {
            found.push(meta);
        }
    }
    // 精确时间（事件里的 time 比 mtime 准）排一次，截断到 limit。
    found.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    found.truncate(limit);
    found
}

fn collect(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    if depth > 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, depth + 1, found);
            continue;
        }
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if name.starts_with(SESSION_FILE_PREFIX) && name.ends_with(SESSION_FILE_SUFFIX) {
            found.push(path);
        }
    }
}

/// 按会话 id 定位日志文件。
///
/// 直接顺着"两层目录 + 文件名"找，不解压任何东西：时间线每翻一页都会调一次，
/// 走 `scan` 的话翻三页就解压三遍全部会话。
pub fn find_log(raw_id: &str) -> Option<PathBuf> {
    let root = locator::sessions_dir().filter(|dir| dir.is_dir())?;
    for bucket in std::fs::read_dir(&root).ok()?.flatten() {
        let bucket = bucket.path();
        if !bucket.is_dir() {
            continue;
        }
        let session_dir = bucket.join(raw_id);
        let Ok(entries) = std::fs::read_dir(&session_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            if name.starts_with(SESSION_FILE_PREFIX) && name.ends_with(SESSION_FILE_SUFFIX) {
                return Some(path);
            }
        }
    }
    None
}

/// 读一份日志的摘要。解析失败（写了一半、版本不认识）就当作没有。
pub fn read_meta(path: &Path) -> Option<SessionMeta> {
    let text = read_log(path).ok()?;
    meta_from_log(&text, path)
}

/// 摘要解析的纯函数版本（测试直接喂字符串）。
fn meta_from_log(text: &str, path: &Path) -> Option<SessionMeta> {
    let mut header_id = None;
    let mut cwd = None;
    let mut created_at = 0;
    let mut updated_at = file_mtime(path);
    let mut title = None;
    let mut model = None;

    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let kind = event.get("type").and_then(Value::as_str).unwrap_or_default();
        if let Some(time) = event.get("time").and_then(Value::as_i64) {
            updated_at = updated_at.max(time);
        }
        match kind {
            "session" => {
                header_id = event.get("id").and_then(Value::as_str).map(str::to_string);
                cwd = event.get("cwd").and_then(Value::as_str).map(str::to_string);
                created_at = event.get("createdAt").and_then(Value::as_i64).unwrap_or(0);
            }
            "session/title" => {
                title = event
                    .pointer("/data/title")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            "request/context" => {
                if let Some(value) = event.pointer("/data/model").and_then(Value::as_str) {
                    model = Some(value.to_string());
                }
            }
            _ => {}
        }
    }

    let id = header_id?;
    Some(SessionMeta {
        id,
        cwd: cwd.unwrap_or_default(),
        created_at,
        updated_at,
        title,
        model,
    })
}

/// 解压并读出整份日志。
///
/// dsh 是多帧追加，`Decoder` 默认按"流"连续解，所以这里能一次拿到全部行。
pub fn read_log(path: &Path) -> Result<String, String> {
    let file = File::open(path).map_err(|cause| format!("打开 {} 失败：{cause}", path.display()))?;
    let mut decoder = zstd::stream::read::Decoder::new(BufReader::new(file))
        .map_err(|cause| format!("{} 不是 zstd 流：{cause}", path.display()))?;
    let mut text = String::new();
    decoder
        .read_to_string(&mut text)
        .map_err(|cause| format!("解压 {} 失败：{cause}", path.display()))?;
    Ok(text)
}

fn file_mtime(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

/// 项目列表：按 cwd 聚合。
pub fn list_projects(limit: usize) -> Vec<ProjectItem> {
    let mut projects: Vec<ProjectItem> = Vec::new();
    for meta in scan(limit) {
        if meta.cwd.is_empty() {
            continue;
        }
        let id = project_id(&meta.cwd);
        match projects.iter_mut().find(|item| item.id == id) {
            Some(existing) => {
                existing.session_count += 1;
                existing.time_updated = existing.time_updated.max(meta.updated_at);
            }
            None => projects.push(ProjectItem {
                engine: locator::ENGINE_ID.to_string(),
                id,
                name: project_name(&meta.cwd),
                worktree: meta.cwd.clone(),
                session_count: 1,
                time_updated: meta.updated_at,
            }),
        }
    }
    projects
}

/// 目录名最后一段（`/` 结尾也能取到；根目录没有段就叫 `/`）。
fn project_name(cwd: &str) -> String {
    let trimmed = cwd.trim_end_matches('/');
    if trimmed.is_empty() {
        return if cwd.is_empty() {
            String::new()
        } else {
            "/".to_string()
        };
    }
    trimmed
        .rsplit('/')
        .next()
        .filter(|segment| !segment.is_empty())
        .unwrap_or(trimmed)
        .to_string()
}

/// 某个项目的会话列表。
pub fn list_sessions(project_id_value: &str, limit: usize) -> Vec<SessionSummary> {
    let Some(cwd) = super::directory_of(project_id_value) else {
        return Vec::new();
    };
    scan(limit)
        .into_iter()
        .filter(|meta| meta.cwd == cwd)
        .map(|meta| summary_of(&meta))
        .collect()
}

/// 单个会话摘要。
pub fn session_summary(session_id: &str) -> Option<SessionSummary> {
    let raw = super::raw_session_id(session_id)?;
    let path = find_log(raw)?;
    let meta = read_meta(&path)?;
    Some(summary_of(&meta))
}

fn summary_of(meta: &SessionMeta) -> SessionSummary {
    SessionSummary {
        engine: locator::ENGINE_ID.to_string(),
        id: super::session_id_of(&meta.id),
        project_id: project_id(&meta.cwd),
        title: meta.title.clone().filter(|title| !title.trim().is_empty()),
        directory: meta.cwd.clone(),
        model: meta.model.clone(),
        // dsh 的日志不落成本；token 只在单条 assistant/message 的 usage 里，
        // 而列表要的是全会话汇总。为了列表去解压求和得不偿失，这里留 0。
        cost: 0.0,
        tokens_input: 0,
        tokens_output: 0,
        tokens_reasoning: 0,
        time_created: meta.created_at,
        time_updated: meta.updated_at,
    }
}

/// 读时间线。`before_seq` 是上一页给回的游标（事件 seq），返回按时间正序。
pub fn read_timeline(
    session_id: &str,
    before_seq: Option<i64>,
    limit: i64,
) -> Result<TimelinePage, String> {
    let raw = super::raw_session_id(session_id).ok_or_else(|| "非法的 dsh 会话 id".to_string())?;
    let path = find_log(raw)
        .ok_or_else(|| format!("找不到 dsh 会话 {raw}（日志可能已被清理）"))?;
    let text = read_log(&path)?;
    Ok(timeline_from_log(&text, before_seq, limit))
}

/// 工具调用的完整详情（展开卡片时按需加载）。
///
/// 入参 `call_id` 就是日志里 `tool/call` 的 `callId`；`message_id` 用不上
/// （dsh 的工具事件不挂在消息下），保留是为了和另外几家的签名一致。
pub fn read_tool_detail(session_id: &str, call_id: &str) -> Result<ToolDetail, String> {
    let raw = super::raw_session_id(session_id).ok_or_else(|| "非法的 dsh 会话 id".to_string())?;
    let path = find_log(raw).ok_or_else(|| format!("找不到 dsh 会话 {raw}"))?;
    let text = read_log(&path)?;

    let mut input: Option<Value> = None;
    let mut output: Option<String> = None;
    let mut error: Option<String> = None;

    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match event.get("type").and_then(Value::as_str).unwrap_or_default() {
            "tool/call" => {
                if event.pointer("/data/callId").and_then(Value::as_str) != Some(call_id) {
                    continue;
                }
                // `arguments` 是原始 JSON 文本：能解析就展开，解析不了就留原文，
                // 界面上至少还能看到模型想传什么。
                let raw_args = event
                    .pointer("/data/arguments")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                input = Some(
                    serde_json::from_str::<Value>(raw_args).unwrap_or(Value::String(
                        raw_args.to_string(),
                    )),
                );
            }
            "tool/result" => {
                if event.pointer("/data/message/source/callId").and_then(Value::as_str)
                    != Some(call_id)
                {
                    continue;
                }
                let content = event
                    .pointer("/data/message/content")
                    .and_then(Value::as_array)
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter_map(|block| block.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                if !content.is_empty() {
                    output = Some(content);
                }
                if let Some(reason) = event.pointer("/data/error") {
                    error = Some(match reason.as_str() {
                        Some(text) => text.to_string(),
                        None => reason.to_string(),
                    });
                }
            }
            _ => {}
        }
    }

    if input.is_none() && output.is_none() && error.is_none() {
        return Err(format!("这条会话里找不到工具调用 {call_id}"));
    }
    Ok(ToolDetail {
        input,
        output,
        error,
        metadata: None,
        truncated: false,
    })
}

/// 把日志文本翻成时间线分页（纯函数，测试直接喂字符串）。
///
/// 组装顺序按 seq：`tool/call` 先攒着，等同一 step 的 `assistant/message` 到了
/// 再拼到它前面——这样界面上就是"工具卡片在前、回复文字在后"，与执行顺序一致。
/// 回合结束时仍没等到助手消息（被中断 / 工具后模型没回话），剩下的工具调用单独
/// 成一条记录，不能丢。
pub fn timeline_from_log(text: &str, before_seq: Option<i64>, limit: i64) -> TimelinePage {
    let mut records: Vec<(i64, TimelineRecord)> = Vec::new();
    let mut model: Option<String> = None;
    let mut context_window: Option<i64> = None;
    // callId → 待挂到助手消息上的工具片段（含调用时间，用于排序稳定）。
    let mut pending_tools: Vec<(i64, TimelinePart)> = Vec::new();

    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let kind = event.get("type").and_then(Value::as_str).unwrap_or_default();
        let seq = event.get("seq").and_then(Value::as_i64).unwrap_or_default();
        let time = event.get("time").and_then(Value::as_i64).unwrap_or_default();

        match kind {
            "request/context" => {
                if let Some(value) = event.pointer("/data/model").and_then(Value::as_str) {
                    model = Some(value.to_string());
                }
                context_window = event
                    .pointer("/data/contextWindow")
                    .and_then(Value::as_i64)
                    .or(context_window);
            }
            "tool/call" => {
                let name = event
                    .pointer("/data/name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_string();
                let call_id = event
                    .pointer("/data/callId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                pending_tools.push((
                    seq,
                    TimelinePart::Tool {
                        name: name.clone(),
                        status: Some("running".to_string()),
                        summary: tool_summary(&event),
                        call_id,
                    },
                ));
            }
            "tool/result" => {
                // 结果到了：把同名同 id 的那条改成终态，不新增卡片。
                let call_id = event
                    .pointer("/data/message/source/callId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let failed = event
                    .pointer("/data/message/isError")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    || event.get("data").and_then(|data| data.get("error")).is_some();
                for (_, part) in pending_tools.iter_mut() {
                    if let TimelinePart::Tool {
                        status,
                        call_id: id,
                        ..
                    } = part
                    {
                        if id == call_id {
                            *status = Some(if failed { "failed" } else { "completed" }.to_string());
                        }
                    }
                }
            }
            "user/message" if is_human_message(&event) => {
                let parts = message_parts(event.pointer("/data/content"));
                if parts.is_empty() {
                    continue;
                }
                records.push((
                    seq,
                    TimelineRecord {
                        id: event
                            .pointer("/data/id")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("user-{seq}")),
                        role: "user".to_string(),
                        time_created: time,
                        model: None,
                        model_provider: None,
                        variant: None,
                        completed: true,
                        parts,
                        tokens: None,
                    },
                ));
            }
            "assistant/message" => {
                let parts = message_parts(event.pointer("/data/message/content"));
                let mut merged: Vec<TimelinePart> = Vec::new();
                // 本 step 攒下的工具卡片排前面。
                merged.extend(pending_tools.drain(..).map(|(_, part)| part));
                merged.extend(parts);
                if merged.is_empty() {
                    continue;
                }
                records.push((
                    seq,
                    TimelineRecord {
                        id: event
                            .pointer("/data/message/id")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("assistant-{seq}")),
                        role: "assistant".to_string(),
                        time_created: time,
                        model: event
                            .pointer("/data/message/source/model")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .or_else(|| model.clone()),
                        model_provider: event
                            .pointer("/data/message/source/provider")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        variant: None,
                        completed: true,
                        parts: merged,
                        tokens: usage_of(&event, context_window),
                    },
                ));
            }
            _ => {}
        }
    }

    // 收尾：只剩工具、没有助手消息的尾巴也要露出来。
    if !pending_tools.is_empty() {
        let seq = pending_tools.iter().map(|(seq, _)| *seq).min().unwrap_or_default();
        let parts: Vec<TimelinePart> = pending_tools.drain(..).map(|(_, part)| part).collect();
        records.push((
            seq,
            TimelineRecord {
                id: format!("tools-{seq}"),
                role: "assistant".to_string(),
                time_created: 0,
                model: model.clone(),
                model_provider: None,
                variant: None,
                completed: false,
                parts,
                tokens: None,
            },
        ));
    }

    records.sort_by_key(|(seq, _)| *seq);
    let limit = limit.clamp(1, 500) as usize;

    // 游标语义与另外三家一致：从末尾往前取一页，next_cursor 指向更早的一条。
    let end = match before_seq {
        Some(cursor) => records
            .iter()
            .position(|(seq, _)| *seq >= cursor)
            .unwrap_or(records.len()),
        None => records.len(),
    };
    let start = end.saturating_sub(limit);
    let page: Vec<TimelineRecord> = records[start..end]
        .iter()
        .map(|(_, record)| record.clone())
        .collect();
    TimelinePage {
        records: page,
        next_cursor: if start > 0 {
            records.get(start).map(|(seq, _)| *seq)
        } else {
            None
        },
        has_more: start > 0,
    }
}

/// 单轮 token 用量。dsh 的 `usage` 与另外几家口径一致：cacheRead 单独算。
fn usage_of(event: &Value, context_window: Option<i64>) -> Option<TurnTokens> {
    let usage = event.pointer("/data/usage")?;
    let read = |key: &str| usage.get(key).and_then(Value::as_i64).unwrap_or(0);
    let tokens = TurnTokens {
        input: read("inputTokens"),
        cache_read: read("cacheReadTokens"),
        cache_write: read("cacheWriteTokens"),
        output: read("outputTokens"),
        reasoning: 0,
        cost: None,
        context_window,
    };
    if tokens.is_empty() {
        None
    } else {
        Some(tokens)
    }
}

/// 工具卡片副标题：优先用模型给的 description，退而求其次给参数摘要。
fn tool_summary(event: &Value) -> Option<String> {
    let raw = event
        .pointer("/data/arguments")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let args: Value = serde_json::from_str(raw).ok()?;
    args.get("description")
        .and_then(Value::as_str)
        .or_else(|| args.get("command").and_then(Value::as_str))
        .or_else(|| args.get("file_path").and_then(Value::as_str))
        .or_else(|| args.get("pattern").and_then(Value::as_str))
        .map(|text| text.lines().next().unwrap_or(text).chars().take(80).collect())
}

/// 只有真人输入才进对话；运行时上下文（工作目录、文件策略）是注入的。
fn is_human_message(event: &Value) -> bool {
    event
        .pointer("/data/source/kind")
        .and_then(Value::as_str)
        .map(|kind| kind == "user")
        .unwrap_or(false)
}

/// dsh 的 content 是块数组：text / reasoning / 图片 / 工具调用（罕见）。
fn message_parts(content: Option<&Value>) -> Vec<TimelinePart> {
    let Some(blocks) = content.and_then(Value::as_array) else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter_map(|block| {
            let kind = block.get("type").and_then(Value::as_str).unwrap_or_default();
            match kind {
                "text" => {
                    let text = block
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    // 空文本块在 dsh 里很常见（reasoning 被裁空），不进时间线。
                    (!text.trim().is_empty()).then_some(TimelinePart::Text { text })
                }
                "reasoning" => {
                    let text = block
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    (!text.trim().is_empty()).then_some(TimelinePart::Reasoning { text })
                }
                "tool-call" | "tool_use" => Some(TimelinePart::Tool {
                    name: block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_string(),
                    status: Some("completed".to_string()),
                    summary: None,
                    call_id: block
                        .get("id")
                        .or_else(|| block.get("callId"))
                        .and_then(Value::as_str)
                        .unwrap_or("call")
                        .to_string(),
                }),
                _ => None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = concat!(
        r#"{"type":"session","version":4,"id":"abc","createdAt":100,"cwd":"/tmp/demo"}"#,
        "\n",
        r#"{"type":"user/message","seq":1,"time":120,"data":{"content":[{"type":"text","text":"你好"}],"source":{"kind":"user"},"id":"u1"}}"#,
        "\n",
        r#"{"type":"user/message","seq":2,"time":121,"data":{"content":[{"type":"text","text":"Current working directory: /tmp/demo"}],"source":{"kind":"runtime"}}}"#,
        "\n",
        r#"{"type":"request/context","seq":3,"time":122,"data":{"provider":"deepseek-official","model":"deepseek-v4-flash","contextWindow":1000000}}"#,
        "\n",
        r#"{"type":"tool/call","seq":4,"time":123,"data":{"callId":"c1","name":"bash","arguments":"{\"description\":\"列一遍目录\",\"command\":\"ls\"}","turn":1,"step":1}}"#,
        "\n",
        r#"{"type":"tool/result","seq":5,"time":124,"data":{"message":{"source":{"callId":"c1"},"content":[{"type":"text","text":"a.txt"}],"isError":false}}}"#,
        "\n",
        r#"{"type":"assistant/message","seq":6,"time":130,"data":{"turn":1,"step":1,"message":{"role":"assistant","content":[{"type":"reasoning","text":"想一下"},{"type":"text","text":"PONG"}],"source":{"kind":"model","provider":"deepseek-official","model":"deepseek-v4-flash"},"id":"a1"},"usage":{"inputTokens":5667,"outputTokens":3,"cacheReadTokens":0,"cacheWriteTokens":0,"totalTokens":5670}}}"#,
        "\n",
        r#"{"type":"session/title","seq":7,"time":131,"data":{"title":"你好"}}"#,
        "\n",
    );

    #[test]
    fn runtime_context_is_not_a_user_message() {
        let page = timeline_from_log(LOG, None, 50);
        let roles: Vec<&str> = page.records.iter().map(|r| r.role.as_str()).collect();
        assert_eq!(roles, vec!["user", "assistant"]);
        assert!(!page.records[0].parts.iter().any(|part| matches!(
            part,
            TimelinePart::Text { text } if text.contains("Current working directory")
        )));
    }

    #[test]
    fn assistant_parts_keep_reasoning_and_text() {
        let page = timeline_from_log(LOG, None, 50);
        let assistant = &page.records[1];
        assert!(matches!(&assistant.parts[0], TimelinePart::Tool { name, status, .. }
            if name == "bash" && status.as_deref() == Some("completed")));
        assert!(matches!(&assistant.parts[1], TimelinePart::Reasoning { text } if text == "想一下"));
        assert!(matches!(&assistant.parts[2], TimelinePart::Text { text } if text == "PONG"));
        assert_eq!(assistant.model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(assistant.id, "a1");
    }

    #[test]
    fn tool_call_rides_on_the_assistant_message() {
        let page = timeline_from_log(LOG, None, 50);
        let assistant = &page.records[1];
        let tool = assistant
            .parts
            .iter()
            .find_map(|part| match part {
                TimelinePart::Tool {
                    summary, call_id, ..
                } => Some((summary.clone(), call_id.clone())),
                _ => None,
            })
            .expect("工具卡片应当在助手消息里");
        assert_eq!(tool.0.as_deref(), Some("列一遍目录"));
        assert_eq!(tool.1, "c1");
    }

    #[test]
    fn usage_becomes_turn_tokens() {
        let page = timeline_from_log(LOG, None, 50);
        let tokens = page.records[1].tokens.expect("应当带上 token 用量");
        assert_eq!(tokens.input, 5667);
        assert_eq!(tokens.output, 3);
        assert_eq!(tokens.context_window, Some(1_000_000));
    }

    #[test]
    fn orphan_tool_calls_still_show_up() {
        let log = concat!(
            r#"{"type":"session","version":4,"id":"abc","createdAt":1,"cwd":"/tmp"}"#,
            "\n",
            r#"{"type":"tool/call","seq":2,"time":10,"data":{"callId":"c9","name":"read","arguments":"{}"}}"#,
            "\n",
        );
        let page = timeline_from_log(log, None, 50);
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0].role, "assistant");
        assert!(!page.records[0].completed);
        assert!(matches!(&page.records[0].parts[0], TimelinePart::Tool { name, .. } if name == "read"));
    }

    #[test]
    fn pagination_walks_backwards() {
        let page = timeline_from_log(LOG, None, 1);
        assert_eq!(page.records.len(), 1);
        assert!(page.has_more);
        let cursor = page.next_cursor.expect("应当有更早的游标");
        let older = timeline_from_log(LOG, Some(cursor), 1);
        assert_eq!(older.records[0].role, "user");
    }

    #[test]
    fn meta_reads_header_title_and_model() {
        let meta = meta_from_log(LOG, Path::new("/tmp/fake.zstd")).expect("能解析出摘要");
        assert_eq!(meta.id, "abc");
        assert_eq!(meta.cwd, "/tmp/demo");
        assert_eq!(meta.created_at, 100);
        assert_eq!(meta.title.as_deref(), Some("你好"));
        assert_eq!(meta.model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(meta.updated_at, 131);
    }

    #[test]
    fn project_name_survives_a_trailing_slash() {
        assert_eq!(project_name("/tmp/demo"), "demo");
        assert_eq!(project_name("/tmp/demo/"), "demo");
        assert_eq!(project_name("/"), "/");
    }

    /// 真机冒烟：拿本机真实的 dsh 日志走一遍"扫描 → 定位 → 时间线 → 工具详情"。
    ///
    /// ```text
    /// DSH_HOME=~/.dsh cargo test --lib dsh_real_log -- --ignored --nocapture
    /// ```
    ///
    /// 默认忽略：CI 上没有 dsh 日志，跑它只会红。
    #[test]
    #[ignore = "需要本机真实的 dsh 会话日志（DSH_HOME）"]
    fn real_log_scan_and_timeline() {
        let metas = scan(20);
        println!("扫到 {} 份会话日志", metas.len());
        assert!(!metas.is_empty(), "DSH_HOME 下没有找到任何会话日志");
        for meta in &metas {
            println!(
                "  {}  cwd={}  title={:?}  model={:?}  updated={}",
                meta.id, meta.cwd, meta.title, meta.model, meta.updated_at
            );
            let path = find_log(&meta.id).expect("按 id 应当能定位回日志");
            let text = read_log(&path).expect("应当能解压多帧 zstd");
            let page = timeline_from_log(&text, None, 50);
            println!("    时间线 {} 条（has_more={}）", page.records.len(), page.has_more);
            for record in &page.records {
                println!(
                    "      [{}] {} parts={} tokens={:?}",
                    record.role,
                    record.time_created,
                    record.parts.len(),
                    record.tokens
                );
            }
        }
    }
}
