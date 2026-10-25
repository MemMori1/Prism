//! 原生引擎自己的会话库（`~/.prism/native/sessions/<id>/`）。
//!
//! 目录结构刻意做得像"一个会话一个文件夹"：
//! - `meta.json`      标题 / 目录 / 模型 / 时间 / 累计 token
//! - `messages.jsonl` 逐行追加，一行一条消息（含工具调用原文）
//!
//! 用 JSONL 而不是 SQLite 的理由：写入只有追加，损坏时最多丢最后一行，
//! 而且调试时 `tail` 就能看。历史消息量上来了再换库也不影响上层接口。

use crate::engines::core::{TimelinePage, TimelinePart, TimelineRecord, ToolDetail, TurnTokens};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;

/// 会话元信息（`meta.json`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMeta {
    /// 裸 id（不带 `native:` 前缀）。
    pub id: String,
    pub directory: String,
    pub title: Option<String>,
    pub model: Option<String>,
    /// 该模型属于哪个服务（会话头回显时要拼 `provider::model` 的 key）。
    #[serde(default)]
    pub model_provider: Option<String>,
    pub time_created: i64,
    pub time_updated: i64,
    #[serde(default)]
    pub tokens_input: i64,
    #[serde(default)]
    pub tokens_output: i64,
    /// 累计命中缓存的输入 token；与 tokens_input 相除就是命中率。
    #[serde(default)]
    pub tokens_cached: i64,
    #[serde(default)]
    pub cost: f64,
}

/// 消息片段：比 `TimelinePart` 多了工具入参/输出，展开详情时不用再查别处。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum StoredPart {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
    },
    /// 用户附的图片：正文只存引用，字节在 `<session>/images/` 下。
    Image {
        name: String,
        mime: String,
        path: String,
        bytes: i64,
    },
    Tool {
        name: String,
        call_id: String,
        status: String,
        #[serde(default)]
        summary: Option<String>,
        #[serde(default)]
        input: Option<Value>,
        #[serde(default)]
        output: Option<String>,
        #[serde(default)]
        error: Option<String>,
        /// 引擎附带的元数据（子代理的步骤轨迹、edit 的 diff 等）。
        #[serde(default)]
        metadata: Option<Value>,
    },
}

impl StoredPart {
    fn to_timeline(&self) -> TimelinePart {
        match self {
            StoredPart::Text { text } => TimelinePart::Text { text: text.clone() },
            StoredPart::Image {
                name,
                mime,
                path,
                bytes,
            } => TimelinePart::Image {
                name: name.clone(),
                mime: mime.clone(),
                path: path.clone(),
                bytes: *bytes,
            },
            StoredPart::Reasoning { text } => TimelinePart::Reasoning { text: text.clone() },
            StoredPart::Tool {
                name,
                call_id,
                status,
                summary,
                ..
            } => TimelinePart::Tool {
                name: name.clone(),
                status: Some(status.clone()),
                summary: summary.clone(),
                call_id: call_id.clone(),
            },
        }
    }
}

/// 模型请求过的工具调用（原样保留，下一轮要回传给模型）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredToolCall {
    pub id: String,
    pub name: String,
    /// 原始 JSON 字符串（模型给的就是文本，不在这里解析）。
    #[serde(default)]
    pub arguments: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StoredTokens {
    #[serde(default)]
    pub input: i64,
    #[serde(default)]
    pub output: i64,
    /// 其中命中前缀缓存的输入（`input` 含这一部分）。旧会话文件没有这个
    /// 字段，反序列化补 0，不影响别的账。
    #[serde(default)]
    pub cached: i64,
}

/// 一行 = 一条消息。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredMessage {
    pub id: String,
    /// user / assistant / tool
    pub role: String,
    pub time: i64,
    /// 模型看到的那段文本（assistant 消息也不含工具入参）。
    #[serde(default)]
    pub text: String,
    /// 界面要展示的片段（含工具卡）。
    #[serde(default)]
    pub parts: Vec<StoredPart>,
    #[serde(default)]
    pub tool_calls: Vec<StoredToolCall>,
    /// role=tool 时对应的调用 id。
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub tokens: Option<StoredTokens>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
}

fn root() -> Result<PathBuf, String> {
    let dir = crate::engines::opencode::search::prism_dir()
        .ok_or_else(|| "无法定位 ~/.prism 目录".to_string())?
        .join("native")
        .join("sessions");
    std::fs::create_dir_all(&dir).map_err(|error| format!("创建会话目录失败：{error}"))?;
    Ok(dir)
}

fn session_dir(raw_id: &str) -> Result<PathBuf, String> {
    // 只允许我们自己生成的 id 形状，挡住 `../` 之类的路径穿越。
    if raw_id.is_empty()
        || raw_id.len() > 40
        || !raw_id.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(format!("非法的原生会话 id：{raw_id}"));
    }
    Ok(root()?.join(raw_id))
}

/// 新会话 id：毫秒 + 纳秒尾巴，单机唯一且天然按时间排序。
pub fn new_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or(0);
    format!("n{}", nanos)
}

pub fn create(directory: &str) -> Result<SessionMeta, String> {
    let id = new_id();
    let dir = session_dir(&id)?;
    std::fs::create_dir_all(&dir).map_err(|error| format!("创建会话目录失败：{error}"))?;
    let meta = SessionMeta {
        id: id.clone(),
        directory: directory.to_string(),
        title: None,
        model: None,
        model_provider: None,
        time_created: now_ms(),
        time_updated: now_ms(),
        tokens_input: 0,
        tokens_output: 0,
        tokens_cached: 0,
        cost: 0.0,
    };
    write_meta(&meta)?;
    Ok(meta)
}

fn meta_path(raw_id: &str) -> Result<PathBuf, String> {
    Ok(session_dir(raw_id)?.join("meta.json"))
}

fn messages_path(raw_id: &str) -> Result<PathBuf, String> {
    Ok(session_dir(raw_id)?.join("messages.jsonl"))
}

pub fn load_meta(raw_id: &str) -> Option<SessionMeta> {
    let path = meta_path(raw_id).ok()?;
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_meta(meta: &SessionMeta) -> Result<(), String> {
    let path = meta_path(&meta.id)?;
    let text = serde_json::to_string_pretty(meta).map_err(|error| error.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|error| format!("写入 {} 失败：{error}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|error| format!("重命名失败：{error}"))
}

/// 追加一条消息，并就地更新 meta（标题 / 时间 / token 累计）。
/// 把用户附的图片落到会话目录（`images/<毫秒>-<文件名>`），返回绝对路径与字节数。
///
/// 消息体里只存引用：图片进了 `messages.jsonl` 会让每次读时间线都多几百 KB，
/// 而绝大多数时候界面根本不需要它。
pub fn save_image(raw_id: &str, name: &str, data: &[u8], mime: &str) -> Result<(String, i64), String> {
    let dir = session_dir(raw_id)?.join("images");
    std::fs::create_dir_all(&dir).map_err(|error| format!("创建图片目录失败：{error}"))?;
    let ext = match mime {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "png",
    };
    // 文件名只留安全字符：原始名字可能是截图工具给的奇怪串。
    let stem: String = name
        .rsplit('/')
        .next()
        .unwrap_or("image")
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' { ch } else { '-' })
        .collect();
    let stem = stem.trim_matches('-');
    let stem = if stem.is_empty() { "image" } else { stem };
    let file = format!("{}-{stem}.{ext}", now_ms());
    let path = dir.join(&file);
    std::fs::write(&path, data).map_err(|error| format!("写入图片失败：{error}"))?;
    Ok((path.to_string_lossy().to_string(), data.len() as i64))
}

pub fn append(raw_id: &str, message: &StoredMessage) -> Result<(), String> {
    let path = messages_path(raw_id)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| format!("打开 {} 失败：{error}", path.display()))?;
    let line = serde_json::to_string(message).map_err(|error| error.to_string())?;
    writeln!(file, "{line}").map_err(|error| format!("写入消息失败：{error}"))?;

    let Some(mut meta) = load_meta(raw_id) else {
        return Ok(());
    };
    meta.time_updated = message.time;
    if meta.title.is_none() && message.role == "user" && !message.text.trim().is_empty() {
        meta.title = Some(title_of(&message.text));
    }
    if let Some(tokens) = message.tokens {
        meta.tokens_input += tokens.input;
        meta.tokens_output += tokens.output;
        meta.tokens_cached += tokens.cached;
    }
    write_meta(&meta)
}

/// 整份重写消息（`/compact` 用）。
///
/// 原文先复制成 `messages.jsonl.bak`（只留最近一次）——压缩是破坏性的，
/// 摘要写歪了至少还能手工把这一个文件换回去。写入走 tmp + rename，
/// 半路断电也不会留下半份会话。
///
/// `input` / `output` / `cached` 是摘要那次调用的用量：它也是真金白银，记进
/// 会话累计（`cached` 是那次调用自己命中的缓存输入）。
pub fn replace_messages(
    raw_id: &str,
    messages: &[StoredMessage],
    input: i64,
    output: i64,
    cached: i64,
) -> Result<(), String> {
    let path = messages_path(raw_id)?;
    if path.exists() {
        let backup = session_dir(raw_id)?.join("messages.jsonl.bak");
        std::fs::copy(&path, &backup).map_err(|error| format!("备份原会话失败：{error}"))?;
    }
    let mut body = String::new();
    for message in messages {
        let line = serde_json::to_string(message).map_err(|error| error.to_string())?;
        body.push_str(&line);
        body.push('\n');
    }
    let tmp = session_dir(raw_id)?.join("messages.jsonl.tmp");
    std::fs::write(&tmp, body).map_err(|error| format!("写入会话失败：{error}"))?;
    std::fs::rename(&tmp, &path).map_err(|error| format!("替换会话失败：{error}"))?;

    let Some(mut meta) = load_meta(raw_id) else {
        return Ok(());
    };
    meta.tokens_input += input;
    meta.tokens_output += output;
    meta.tokens_cached += cached;
    meta.time_updated = now_ms();
    write_meta(&meta)
}

/// 记录这一轮实际用的模型与服务（会话头回显用）。
pub fn set_model(raw_id: &str, model: &str, provider: &str) -> Result<(), String> {
    let Some(mut meta) = load_meta(raw_id) else {
        return Ok(());
    };
    if meta.model.as_deref() == Some(model) && meta.model_provider.as_deref() == Some(provider) {
        return Ok(());
    }
    meta.model = Some(model.to_string());
    meta.model_provider = if provider.is_empty() {
        None
    } else {
        Some(provider.to_string())
    };
    write_meta(&meta)
}

/// 标题取首个非空行，压到 60 字符。
fn title_of(text: &str) -> String {    let first = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("新会话");
    let mut out: String = first.chars().take(60).collect();
    if first.chars().count() > 60 {
        out.push('…');
    }
    out
}

pub fn messages(raw_id: &str) -> Vec<StoredMessage> {
    let Ok(path) = messages_path(raw_id) else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<StoredMessage>(line).ok())
        .collect()
}

/// 所有原生会话（按更新时间倒序）。
fn all_metas() -> Vec<SessionMeta> {
    let Ok(root) = root() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut list: Vec<SessionMeta> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| {
            let id = entry.file_name().to_string_lossy().to_string();
            load_meta(&id)
        })
        .collect();
    list.sort_by(|a, b| b.time_updated.cmp(&a.time_updated));
    list
}

fn summary_of(meta: &SessionMeta) -> crate::engines::core::SessionSummary {
    crate::engines::core::SessionSummary {
        engine: super::ENGINE_ID.to_string(),
        id: super::session_id_of(&meta.id),
        project_id: super::project_id_of(&meta.directory),
        title: meta.title.clone(),
        directory: meta.directory.clone(),
        model: meta.model.clone(),
        cost: meta.cost,
        tokens_input: meta.tokens_input,
        tokens_output: meta.tokens_output,
        tokens_cached: meta.tokens_cached,
        tokens_reasoning: 0,
        time_created: meta.time_created,
        time_updated: meta.time_updated,
    }
}

/// 按目录聚合成项目（没有会话的目录不会出现）。
pub fn list_projects() -> Vec<crate::engines::core::ProjectItem> {
    use std::collections::BTreeMap;
    let mut grouped: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    for meta in all_metas() {
        let entry = grouped
            .entry(meta.directory)
            .or_insert((0, 0));
        entry.0 += 1;
        entry.1 = entry.1.max(meta.time_updated);
    }
    grouped
        .into_iter()
        .map(|(directory, (count, updated))| {
            let name = std::path::Path::new(&directory)
                .file_name()
                .map(|value| value.to_string_lossy().to_string())
                .unwrap_or_else(|| directory.clone());
            crate::engines::core::ProjectItem {
                engine: super::ENGINE_ID.to_string(),
                id: super::project_id_of(&directory),
                worktree: directory,
                name,
                session_count: count,
                time_updated: updated,
            }
        })
        .collect()
}

pub fn list_sessions(project_id: &str, limit: usize) -> Vec<crate::engines::core::SessionSummary> {
    let Some(directory) = super::directory_of(project_id) else {
        return Vec::new();
    };
    all_metas()
        .iter()
        .filter(|meta| meta.directory == directory)
        .take(limit)
        .map(summary_of)
        .collect()
}

pub fn summary(session_id: &str) -> Option<crate::engines::core::SessionSummary> {
    let raw = super::raw_session_id(session_id)?;
    load_meta(raw).map(|meta| summary_of(&meta))
}

fn record_of(message: &StoredMessage) -> TimelineRecord {
    TimelineRecord {
        id: message.id.clone(),
        role: message.role.clone(),
        time_created: message.time,
        model: None,
        model_provider: None,
        variant: None,
        completed: true,
        parts: message.parts.iter().map(StoredPart::to_timeline).collect(),
        tokens: message.tokens.map(|tokens| TurnTokens {
            input: tokens.input,
            cache_read: 0,
            cache_write: 0,
            output: tokens.output,
            reasoning: 0,
            cost: None,
            context_window: None,
        }),
    }
}

/// 时间线：MVP 一次全给（原生会话体量小），游标留空表示没有更早的。
pub fn timeline(session_id: &str) -> TimelinePage {
    let Some(raw) = super::raw_session_id(session_id) else {
        return TimelinePage {
            records: Vec::new(),
            next_cursor: None,
            has_more: false,
        };
    };
    let mut records: Vec<TimelineRecord> = messages(raw)
        .iter()
        // 工具结果已经是工具卡片的一部分，不再单列成一条记录。
        .filter(|message| message.role != "tool")
        .map(record_of)
        .collect();
    // 这一轮用的模型记在 meta 里（写消息时不重复存），回放到时间线上，
    // 会话头的模型下拉才能回显。
    if let Some(meta) = load_meta(raw) {
        if let Some(model) = meta.model {
            for record in records.iter_mut().filter(|item| item.role == "assistant") {
                record.model = Some(model.clone());
                record.model_provider = meta.model_provider.clone();
            }
        }
    }
    TimelinePage {
        records,
        next_cursor: None,
        has_more: false,
    }
}

pub fn tool_detail(session_id: &str, call_id: &str) -> Option<ToolDetail> {
    let raw = super::raw_session_id(session_id)?;
    for message in messages(raw) {
        for part in message.parts {
            if let StoredPart::Tool {
                call_id: id,
                input,
                output,
                error,
                metadata,
                ..
            } = part
            {
                if id == call_id {
                    return Some(ToolDetail {
                        input,
                        output,
                        error,
                        metadata,
                        truncated: false,
                    });
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_traversal_ids() {
        assert!(session_dir("../etc").is_err());
        assert!(session_dir("a/b").is_err());
        assert!(session_dir("").is_err());
    }

    #[test]
    fn title_takes_first_line_and_truncates() {
        assert_eq!(title_of("\n\n  修一下 README  \n第二行"), "修一下 README");
        let long = "字".repeat(80);
        let title = title_of(&long);
        assert_eq!(title.chars().count(), 61);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn stored_part_maps_to_timeline_part() {
        let part = StoredPart::Tool {
            name: "read_file".to_string(),
            call_id: "c1".to_string(),
            status: "completed".to_string(),
            summary: Some("src/main.rs".to_string()),
            input: None,
            output: Some("ok".to_string()),
            error: None,
            metadata: None,
        };
        match part.to_timeline() {
            TimelinePart::Tool { name, call_id, .. } => {
                assert_eq!(name, "read_file");
                assert_eq!(call_id, "c1");
            }
            other => panic!("应为工具片段：{other:?}"),
        }
    }
}
