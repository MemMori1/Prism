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
    /// 其中命中前缀缓存的输入（只有原生引擎拿得到这个数；其余记 0）。
    /// 与 tokens_input 相除就是缓存命中率。
    pub tokens_cached: i64,
    pub tokens_reasoning: i64,
    pub time_created: i64,
    pub time_updated: i64,
}

/// 时间线记录里的一个片段。
#[derive(Debug, Clone, Serialize)]
// `rename_all` 只作用于变体名（→ kind 的取值），变体里的字段要单独声明
// `rename_all_fields`，否则 call_id 会以 snake_case 发到前端，调用详情时报缺参。
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum TimelinePart {
    Text { text: String },
    /// 用户发进来的图片：落盘在会话目录下，界面按 path 取图。
    Image {
        name: String,
        mime: String,
        path: String,
        bytes: i64,
    },
    Reasoning { text: String },
    Tool {
        name: String,
        status: Option<String>,
        summary: Option<String>,
        /// 工具调用 id，展开详情时用于定位。
        call_id: String,
    },
}

/// 单轮 token 用量（各引擎归一到同一口径：缓存读不与未缓存输入重叠）。
#[derive(Debug, Clone, Copy, Serialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TurnTokens {
    /// 未缓存输入。
    pub input: i64,
    /// 缓存读（已从输入中扣除）。
    pub cache_read: i64,
    /// 缓存写 / 缓存创建。
    pub cache_write: i64,
    /// 输出。
    pub output: i64,
    /// 思考（与输出的包含关系随引擎原样呈现）。
    pub reasoning: i64,
    /// 该轮成本（引擎提供时）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    /// 模型上下文窗口（已知时，用于上下文用量条）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<i64>,
}

impl TurnTokens {
    /// 全部为零（且无成本）时视为无数据。
    pub fn is_empty(&self) -> bool {
        self.input == 0
            && self.cache_read == 0
            && self.cache_write == 0
            && self.output == 0
            && self.reasoning == 0
            && self.cost.unwrap_or(0.0) <= 0.0
    }
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
    /// 模型提供方（OpenCode 的 providerID；其他引擎为空）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    /// 该轮推理档位（OpenCode variant / Codex effort）；界面用来回显当前选择。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    /// 助手消息是否已完结（无 time.completed 视为进行中/中断）。
    pub completed: bool,
    pub parts: Vec<TimelinePart>,
    /// 该轮 token 用量（引擎数据可得时）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TurnTokens>,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 前端按 `part.callId` 取工具调用 id；字段名必须是 camelCase，
    /// 否则展开详情时 invoke 会缺 `callId` 参数。
    #[test]
    fn tool_part_serializes_camel_case_fields() {
        let part = TimelinePart::Tool {
            name: "shell".to_string(),
            status: Some("completed".to_string()),
            summary: None,
            call_id: "call_1".to_string(),
        };
        let json = serde_json::to_string(&part).expect("序列化工具片段");
        assert!(json.contains("\"kind\":\"tool\""), "kind 取值：{json}");
        assert!(json.contains("\"callId\":\"call_1\""), "callId 字段：{json}");
        assert!(!json.contains("call_id"), "不应出现 snake_case：{json}");
    }
}
