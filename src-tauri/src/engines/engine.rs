//! 引擎统一抽象：把"多引擎适配器"从散落的硬编码 match 收敛成 trait + 注册表。
//!
//! 设计目标（对应"一切皆插件"的宿主层）：
//! - 上层（ipc / lib）只面向 `Engine` trait，不感知具体引擎实现；
//! - 加一个新引擎 = 实现 trait + 往注册表塞一行，不再改散落各处的 match；
//! - 能力形状不同的引擎（只读历史 vs 自己跑循环）用 trait 的"可选方法"表达，
//!   默认实现返回"不支持"，而不是硬塞一个假实现。
//!
//! 引擎的"数据形状"已经统一在 `core`（EngineHealth / SessionSummary 等），
//! 这里统一的是"行为"。

use std::sync::{mpsc, Arc, Mutex};

use serde_json::Value;

use crate::catalog::EngineCatalog;
use crate::engines::core::EngineHealth;

/// 会话创建结果（opencode / codex 同形；见 opencode::runtime::CreatedSession）。
pub struct CreatedSession {
    pub session_id: String,
    pub directory: Option<String>,
}

/// 引擎运行时能力的最小公共契约。
///
/// 方法按"是否通用"分两档：
/// - `id` / `display_name` / `health` / `shutdown` 每个引擎必有；
/// - 会话类能力（建会话、列模型）用默认实现返回"不支持"，由真正支持的引擎覆盖。
///
/// 刻意不放 `list_sessions` / `read_timeline`：这些走历史库（opencode.db /
/// codex state 库 / claude 目录 / native store），是"读数据"而非"引擎运行时"
/// 能力，仍由各引擎的 locator/db 模块独立承担，避免 trait 过度膨胀。
pub trait Engine: Send + Sync {
    /// 引擎 id（`opencode` / `codex` / `native` …），前端按它分发。
    fn id(&self) -> &'static str;

    /// 展示名。
    ///
    /// 目前健康状态里已带展示名，所以这一项留给"引擎选择器只取元信息、
    /// 不触发探测"的场景（前端引擎列表即将接入）。
    #[allow(dead_code)]
    fn display_name(&self) -> &'static str;

    /// 健康状态（历史数据与运行时 CLI 分别探测）。
    fn health(&self) -> EngineHealth;

    /// 在指定目录新建会话。默认"不支持"（Claude 只读历史即走这条）。
    fn create_session(&self, _directory: &str) -> Result<CreatedSession, String> {
        Err(format!("{} 引擎暂不支持新建会话", self.id()))
    }

    /// 引擎能力目录（模型 + 斜杠命令）。默认"不可用"。
    fn catalog(&self, _directory: &str) -> Result<EngineCatalog, String> {
        Ok(crate::catalog::unavailable(
            self.id(),
            format!("{} 引擎暂不支持能力目录", self.id()),
        ))
    }

    /// 订阅过程事件流（一轮执行中的增量）。
    ///
    /// 默认返回一个**立即断开**的 channel：没有事件源的引擎，调用方会
    /// 顺着超时路径收尾，而不是卡在等事件上。
    fn subscribe(&self) -> mpsc::Receiver<Value> {
        let (_tx, rx) = mpsc::channel();
        rx
    }

    /// 往已有会话发一条提示词，返回用户消息 id（没有 id 的引擎返回空串）。
    fn send_prompt(&self, _session_id: &str, _text: &str) -> Result<String, String> {
        Err(format!("{} 引擎暂不支持发送消息", self.id()))
    }

    /// 打断正在跑的一轮。
    fn interrupt(&self, _session_id: &str) -> Result<(), String> {
        Err(format!("{} 引擎暂不支持打断", self.id()))
    }

    /// 关停运行时（结束子进程 / 事件线程）。默认空实现。
    fn shutdown(&self) {}
}

impl CreatedSession {
    /// 便捷构造：从 opencode/codex 运行时已有的同形结果转过来。
    pub fn new(session_id: String, directory: Option<String>) -> Self {
        CreatedSession {
            session_id,
            directory,
        }
    }
}

/// 引擎注册表：启动时装配、运行期按 id 查找。
///
/// 用 `Arc<dyn Engine>` 存，允许各引擎持有自己的内部状态（子进程句柄、
/// 事件订阅者等），克隆句柄即可跨线程共享。
pub struct EngineRegistry {
    engines: Vec<Arc<dyn Engine>>,
}

impl EngineRegistry {
    pub fn new() -> Self {
        EngineRegistry {
            engines: Vec::new(),
        }
    }

    /// 注册一个引擎（幂等按 id 去重：后注册的覆盖先注册的）。
    pub fn register(&mut self, engine: Arc<dyn Engine>) {
        if let Some(existing) = self
            .engines
            .iter_mut()
            .find(|item| item.id() == engine.id())
        {
            *existing = engine;
        } else {
            self.engines.push(engine);
        }
    }

    /// 按 id 取引擎。
    pub fn get(&self, id: &str) -> Option<Arc<dyn Engine>> {
        self.engines
            .iter()
            .find(|engine| engine.id() == id)
            .cloned()
    }

    /// 全部引擎（前端引擎选择器的数据源，即将接入）。
    #[allow(dead_code)]
    pub fn all(&self) -> &[Arc<dyn Engine>] {
        &self.engines
    }

    /// 全部引擎的健康状态（`engine_health` 的数据源）。
    pub fn health(&self) -> Vec<EngineHealth> {
        self.engines.iter().map(|engine| engine.health()).collect()
    }
}

impl Default for EngineRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Tauri 托管状态：`Mutex` 包一层以支持运行期重装配（未来配置驱动热插拔）。
pub type RegistryState = Mutex<EngineRegistry>;

#[cfg(test)]
mod tests {
    use super::*;

    struct StubEngine {
        id: &'static str,
    }

    impl Engine for StubEngine {
        fn id(&self) -> &'static str {
            self.id
        }
        fn display_name(&self) -> &'static str {
            "stub"
        }
        fn health(&self) -> EngineHealth {
            EngineHealth {
                engine_id: self.id.to_string(),
                display_name: "stub".to_string(),
                database_path: None,
                cli_path: None,
                ready: true,
            }
        }
    }

    #[test]
    fn registry_registers_and_dedups_by_id() {
        let mut registry = EngineRegistry::new();
        registry.register(Arc::new(StubEngine { id: "a" }));
        registry.register(Arc::new(StubEngine { id: "b" }));
        registry.register(Arc::new(StubEngine { id: "a" })); // 覆盖
        assert_eq!(registry.all().len(), 2);
        assert!(registry.get("a").is_some());
        assert!(registry.get("b").is_some());
        assert!(registry.get("c").is_none());
    }

    #[test]
    fn unknown_engine_returns_unsupported_defaults() {
        let engine: Arc<dyn Engine> = Arc::new(StubEngine { id: "a" });
        assert!(engine.create_session("/tmp").is_err());
        assert!(!engine.catalog("/tmp").unwrap().available);
    }
}
