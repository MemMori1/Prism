//! 引擎适配层的中立模型。
//!
//! 上层界面只消费这些类型；引擎差异（OpenCode / Claude Code / Codex ...）
//! 一律封在各自适配器内，不允许泄漏到 IPC 之外。

use serde::Serialize;

/// 引擎健康状态（界面顶部状态条）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineHealth {
    pub engine_id: String,
    pub display_name: String,
    /// 引擎历史数据位置（如 opencode.db）；未找到为 None。
    pub database_path: Option<String>,
    /// 运行时 CLI 位置；未找到为 None。
    pub cli_path: Option<String>,
    /// 历史与运行时都可用。
    pub ready: bool,
}

/// 项目（工作目录）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectItem {
    /// 所属引擎 id（opencode / codex …）。
    pub engine: String,
    pub id: String,
    pub worktree: String,
    pub name: String,
    pub session_count: i64,
    pub time_updated: i64,
}

/// 会话概要（列表用）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    /// 所属引擎 id（opencode / codex …）。
    pub engine: String,
    pub id: String,
    pub project_id: String,
    pub title: Option<String>,
    pub directory: String,
    pub model: Option<String>,
    pub cost: f64,
    pub tokens_input: i64,
    pub tokens_output: i64,
    pub tokens_reasoning: i64,
    pub time_created: i64,
    pub time_updated: i64,
}

/// 时间线记录里的一个片段。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum TimelinePart {
    Text { text: String },
    Reasoning { text: String },
    Tool {
        name: String,
        status: Option<String>,
        summary: Option<String>,
        /// 工具调用 id，展开详情时用于定位。
        call_id: String,
    },
}

/// 时间线记录（一条用户消息或一条助手消息）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineRecord {
    pub id: String,
    /// user / assistant / system
    pub role: String,
    pub time_created: i64,
    pub model: Option<String>,
    /// 助手消息是否已完结（无 time.completed 视为进行中/中断）。
    pub completed: bool,
    pub parts: Vec<TimelinePart>,
}

/// 时间线分页（从最新往回取）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelinePage {
    /// 按时间正序（最新一页在末尾）。
    pub records: Vec<TimelineRecord>,
    /// 继续加载更早消息的游标（不透明）；没有更早消息时为 None。
    pub next_cursor: Option<i64>,
    pub has_more: bool,
}

/// 工具调用的完整详情（展开卡片时按需加载）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDetail {
    pub input: Option<serde_json::Value>,
    /// 输出文本（state.content 拼接 / state.output）。
    pub output: Option<String>,
    /// 失败原因（state.error）。
    pub error: Option<String>,
    /// 引擎附带的元数据（如 edit 的 unified diff patch）。
    pub metadata: Option<serde_json::Value>,
    /// 输出因过大被截断。
    pub truncated: bool,
}

/// 搜索命中（Prism 自有索引的查询结果）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    /// 消息 id；「仅标题」命中时为 None。
    pub message_id: Option<String>,
    pub session_id: String,
    pub session_title: Option<String>,
    pub project_id: String,
    /// user / assistant；标题命中时为 None。
    pub role: Option<String>,
    pub seq: Option<i64>,
    pub time_created: i64,
    /// 命中片段；高亮段用 \u{1} / \u{2} 控制字符包裹（前端拆段渲染，天然防注入）。
    pub snippet: String,
}

/// 搜索索引状态。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchStats {
    pub indexed_sessions: i64,
    pub indexed_messages: i64,
    /// 最近一次索引构建时间（毫秒时间戳；从未构建为 0）。
    pub updated_at: i64,
}
