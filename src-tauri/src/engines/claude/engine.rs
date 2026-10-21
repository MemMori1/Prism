//! Claude Code 引擎适配（只读历史；运行轴后置）。
//!
//! `Engine` trait 的最小实现：只承担"健康探测"与"能力目录（不可用）"。
//! 会话创建走 trait 默认的"不支持"——这正是 trait 用默认方法表达
//! "能力形状不一致"的价值：Claude 现阶段本来就没有运行轴，不需要
//! 在 ipc 层写一个 `Some("claude") => Err(...)` 的硬编码分支。
//!
//! 历史数据读取仍由 `transcript` 独立承担（它不属于"引擎运行时"能力）。

use crate::catalog::EngineCatalog;
use crate::engines::core::EngineHealth;
use crate::engines::engine::Engine;

use super::locator;

/// Claude 引擎的只读适配器（无内部状态，可自由构造）。
pub struct ClaudeEngine;

impl ClaudeEngine {
    pub fn new() -> Self {
        ClaudeEngine
    }
}

impl Default for ClaudeEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine for ClaudeEngine {
    fn id(&self) -> &'static str {
        locator::ENGINE_ID
    }

    fn display_name(&self) -> &'static str {
        locator::DISPLAY_NAME
    }

    fn health(&self) -> EngineHealth {
        // 浏览历史只要求档案目录存在；CLI 决定运行轴（后置里程碑）。
        let root = locator::projects_dir().filter(|path| path.is_dir());
        let cli = locator::cli_path();
        EngineHealth {
            engine_id: locator::ENGINE_ID.to_string(),
            display_name: locator::DISPLAY_NAME.to_string(),
            ready: root.is_some(),
            database_path: root.map(|path| path.display().to_string()),
            cli_path: cli.map(|path| path.display().to_string()),
        }
    }

    fn catalog(&self, _directory: &str) -> Result<EngineCatalog, String> {
        Ok(crate::catalog::unavailable(
            locator::ENGINE_ID,
            "未检测到 Claude Code CLI（运行轴后置），先用 OpenCode / Codex 开工",
        ))
    }
}
