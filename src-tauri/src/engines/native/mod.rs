//! Prism 内置 agent（原生引擎）。
//!
//! 与另外三家不同：这里不是"读别人的会话库"，而是**自己跑 turn 循环、
//! 自己落库**。所以 id 一律带 `native:` 前缀，避免和历史引擎混淆。
//!
//! 分层：
//! - `config`   模型服务（多端点 + 模型清单 + 默认模型，`~/.prism/native/config.json`）
//! - `store`    会话与消息落盘（`~/.prism/native/sessions/<id>/`）
//! - `provider` OpenAI 兼容的流式客户端（SSE 解析 + 增量拼装 + `/models` 拉取）
//! - `tools`    内置工具（读写文件、列目录、检索、跑命令）
//! - `guard`    命令闸门：受保护路径 / 危险命令 / 环境清洗 / 回收站 / 文本检测
//! - `runtime`  脚本运行时：固定环境 + macOS seatbelt 隔离（Windows 走策略层）
//! - `web`      出站抓取（`fetch`：只 GET、有超时与上限、HTML 抽正文）
//! - `browser_tools` Agent 的浏览器（`browser.open/read/act`，操作面板里那块真 webview）
//! - `artifact_tools` 制品读写（只在 DAG 节点作用域里出现）
//! - `forensics` 安全研究线的本机工具（`bin.meta.identify` 等）
//! - `backends` 外部程序后端：探测 / 统一执行 / 作业目录（upx、r2、Ghidra）
//! - `overflow` 上下文超限的两道闸：工具结果限流 + 拒收后的机械瘦身
//! - `analysis`  安全研究线的分析工具（xref / 导入导出 / 密码特征 / 差异）
//! - `dag_tools` Planner 的七工具（改图），只在 DAG 作用域里出现
//! - `compact`  `/compact`：把历史压成摘要再重写会话（省上下文）
//! - `agent`    turn 循环：模型 → 工具 → 模型，事件原样喂给前端 liveTurn

pub mod agent;
pub mod analysis;
pub mod approval;
pub mod artifact_tools;
pub mod backends;
pub mod browser_tools;
pub mod compact;
pub mod config;
pub mod context;
pub mod dag_tools;
pub mod dex;
pub mod engine;
pub mod forensics;
pub mod guard;
pub mod overflow;
pub mod provider;
pub mod runtime;
pub mod store;
pub mod subagent;
pub mod tools;
pub mod web;

/// 往引擎事件通道推一条事件。
///
/// 权限请求（`permission.asked`）不是 turn 循环内部的事，这里对外开一个口。
pub fn emit_event(app: &tauri::AppHandle, event_type: &str, data: serde_json::Value) {
    use tauri::Emitter;
    let _ = app.emit(
        agent::EVENT_NAME,
        serde_json::json!({ "type": event_type, "data": data }),
    );
}

/// 引擎 id（与前端的 engine 字段一致）。
pub const ENGINE_ID: &str = "native";
/// 会话 / 项目 id 前缀。
pub const SESSION_PREFIX: &str = "native:";
pub const PROJECT_PREFIX: &str = "native:";

/// 把裸 id 包成前端认的会话 id。
pub fn session_id_of(raw: &str) -> String {
    format!("{SESSION_PREFIX}{raw}")
}

pub fn is_native_session(session_id: &str) -> bool {
    session_id.starts_with(SESSION_PREFIX)
}

pub fn raw_session_id(session_id: &str) -> Option<&str> {
    session_id.strip_prefix(SESSION_PREFIX)
}

/// 项目 id 用工作目录做键（与 Codex 的做法一致）。
pub fn project_id_of(directory: &str) -> String {
    format!("{PROJECT_PREFIX}{directory}")
}

pub fn is_native_project(project_id: &str) -> bool {
    project_id.starts_with(PROJECT_PREFIX)
}

pub fn directory_of(project_id: &str) -> Option<&str> {
    project_id.strip_prefix(PROJECT_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_round_trip() {
        let id = session_id_of("abc");
        assert!(is_native_session(&id));
        assert_eq!(raw_session_id(&id), Some("abc"));
        assert!(!is_native_session("codex:abc"));
    }

    #[test]
    fn project_ids_round_trip() {
        let id = project_id_of("/tmp/demo");
        assert!(is_native_project(&id));
        assert_eq!(directory_of(&id), Some("/tmp/demo"));
    }
}
