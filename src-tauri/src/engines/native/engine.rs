//! 原生引擎的 `Engine` 适配器。
//!
//! 原生引擎和另外三家形态不同：没有 CLI、没有外部数据库，会话落在
//! `~/.prism/native/sessions/`。所以在 `Engine` trait 上：
//! - `health`：配好模型服务即就绪；
//! - `create_session`：直接写自己的会话库（不拉进程）；
//! - `catalog`：从配置里读"勾过的模型"，按服务分组。
//!
//! 无内部状态：会话状态都在 `store` 的文件里，Agent 循环在 `agent` 里，
//! 所以这里可以自由构造、无需全局单例。

use crate::catalog::{EngineCatalog, EngineModel};
use crate::engines::core::EngineHealth;
use crate::engines::engine::{CreatedSession, Engine};

use super::{config, store, ENGINE_ID, SESSION_PREFIX};

/// 原生引擎适配器（无状态）。
pub struct NativeEngine;

impl NativeEngine {
    pub fn new() -> Self {
        NativeEngine
    }
}

impl Default for NativeEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine for NativeEngine {
    fn id(&self) -> &'static str {
        ENGINE_ID
    }

    fn display_name(&self) -> &'static str {
        "Prism 原生"
    }

    fn health(&self) -> EngineHealth {
        // 没有 CLI，也没有别人的数据库——配好 API 就算就绪。
        EngineHealth {
            engine_id: ENGINE_ID.to_string(),
            display_name: "Prism 原生".to_string(),
            ready: config::load().ready(),
            database_path: config::dir().map(|path| path.display().to_string()),
            cli_path: None,
        }
    }

    fn create_session(&self, directory: &str) -> Result<CreatedSession, String> {
        let meta = store::create(directory)?;
        Ok(CreatedSession::new(
            format!("{SESSION_PREFIX}{}", meta.id),
            Some(meta.directory),
        ))
    }

    fn catalog(&self, _directory: &str) -> Result<EngineCatalog, String> {
        // 模型目录就是设置里勾过的模型，按服务分组（provider 字段）。
        let config = config::load();
        let ready = config.ready();
        let active = config
            .selection()
            .map(|(provider, model)| (provider.id.clone(), model.id.clone()));
        let mut models = Vec::new();
        for provider in &config.providers {
            for model in &provider.models {
                let is_default = active
                    .as_ref()
                    .map(|(provider_id, model_id)| {
                        provider_id == &provider.id && model_id == &model.id
                    })
                    .unwrap_or(false);
                models.push(EngineModel {
                    id: model.id.clone(),
                    label: model.display().to_string(),
                    provider: Some(provider.id.clone()),
                    variants: Vec::new(),
                    default_variant: None,
                    is_default,
                });
            }
        }
        Ok(EngineCatalog {
            engine: ENGINE_ID.to_string(),
            available: ready,
            reason: if ready {
                None
            } else {
                Some("还没配置模型服务".to_string())
            },
            models,
            commands: Vec::new(),
        })
    }
}
