//! Claude Code 引擎适配：历史浏览（`~/.claude/projects` JSONL）。
//! 运行轴（`claude` CLI stream-json）随 M9 后续接入，需本机安装 CLI 后真机验证。

pub mod engine;
pub mod locator;
pub mod transcript;
