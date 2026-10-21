//! `Engine` trait 的 dsh 实现。
//!
//! 只做"把运行轴接到注册表上"这一件事：真正的活儿（ACP 连接、事件翻译、历史
//! 解析）都在 `runtime` / `acp` / `history` 里。放在单独文件是为了让 `runtime.rs`
//! 保持"纯运行轴"——它不该知道 `EngineCatalog` 长什么样。

use std::sync::mpsc;

use serde_json::Value;

use super::{locator, runtime::Runtime};
use crate::catalog::{self, EngineCatalog, EngineModel};
use crate::engines::core::EngineHealth;
use crate::engines::engine::{CreatedSession, Engine};

impl Engine for Runtime {
    fn id(&self) -> &'static str {
        locator::ENGINE_ID
    }

    fn display_name(&self) -> &'static str {
        locator::DISPLAY_NAME
    }

    fn health(&self) -> EngineHealth {
        let cli = locator::cli_path();
        let home = locator::home();
        // 历史和运行时是两件事：装了 dsh 但还没跑过任何会话时，`sessions` 目录
        // 不存在也是"可用"的（第一条消息会创建它），只是列表暂时是空的。
        EngineHealth {
            engine_id: locator::ENGINE_ID.to_string(),
            display_name: locator::DISPLAY_NAME.to_string(),
            ready: cli.is_some(),
            database_path: home.map(|path| path.display().to_string()),
            cli_path: cli.map(|path| path.display().to_string()),
        }
    }

    fn create_session(&self, directory: &str) -> Result<CreatedSession, String> {
        let session_id = Runtime::create_session(self, directory)?;
        Ok(CreatedSession::new(
            session_id,
            Some(directory.to_string()),
        ))
    }

    /// 能力目录：模型按 ACP 的 `configOptions` 翻，推理档位当变体。
    ///
    /// dsh 没有斜杠命令目录（`available_commands_update` 只是运行时提示，不能枚举
    /// 全量），所以 commands 恒为空——前端会自然地把命令面板收起来。
    fn catalog(&self, directory: &str) -> Result<EngineCatalog, String> {
        if locator::cli_path().is_none() {
            return Ok(catalog::unavailable(
                locator::ENGINE_ID,
                "未找到 dsh 命令：装好 DeepSeek Harness 后重启 Prism",
            ));
        }
        let options = match self.config_options(directory) {
            Ok(options) => options,
            // 读不到配置不是"引擎不可用"：至少会话还能建、还能聊。
            Err(error) => {
                eprintln!("[prism] dsh 读取能力目录失败：{error}");
                None
            }
        };
        let models = match options {
            Some(options) => parse_models(&options),
            // 这个目录还没会话过：先把"可用"给出去，模型等第一次建会话后再出现。
            None => Vec::new(),
        };
        Ok(EngineCatalog {
            engine: locator::ENGINE_ID.to_string(),
            available: true,
            reason: if models.is_empty() {
                Some("发送第一条消息后即可选择模型（dsh 的模型列表挂在会话配置上）".to_string())
            } else {
                None
            },
            models,
            // dsh 没有斜杠命令目录（`available_commands_update` 只是运行时提示，
            // 不能枚举全量），命令面板会自然收起。
            commands: Vec::new(),
        })
    }

    fn subscribe(&self) -> mpsc::Receiver<Value> {
        Runtime::subscribe(self)
    }

    fn send_prompt(&self, session_id: &str, text: &str) -> Result<String, String> {
        Runtime::send_prompt(self, session_id, text)
    }

    fn interrupt(&self, session_id: &str) -> Result<(), String> {
        Runtime::interrupt(self, session_id)
    }

    fn shutdown(&self) {
        Runtime::shutdown(self);
    }
}

/// `configOptions` → Prism 的模型列表。
///
/// 两个字段的取值都是**字符串**：模型是 `["provider","model"]` 的 JSON 文本，
/// 推理档位是 `off` / `low` / `high` / `max`。前端只认 id + 变体，所以这里把
/// 复合值拆开，provider 单独放。
pub fn parse_models(options: &Value) -> Vec<EngineModel> {
    let Some(list) = options.as_array() else {
        return Vec::new();
    };
    let model_option = list
        .iter()
        .find(|item| item.get("id").and_then(Value::as_str) == Some("model"));
    let effort_option = list
        .iter()
        .find(|item| item.get("id").and_then(Value::as_str) == Some("reasoning_effort"));

    let variants: Vec<String> = effort_option
        .and_then(|item| item.get("options"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.get("value")
                        .or_else(|| item.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    let default_variant = effort_option
        .and_then(|item| item.get("currentValue"))
        .and_then(Value::as_str)
        .map(str::to_string);

    let current_model = model_option
        .and_then(|item| item.get("currentValue"))
        .and_then(Value::as_str)
        .map(str::to_string);

    let mut models = Vec::new();
    let Some(model_option) = model_option else {
        return models;
    };
    for group in model_option
        .get("options")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        // 形状一：`{group, name, options:[{value,name}]}`（按 provider 分组）
        // 形状二：`{value, name, description}`（扁平）
        let grouped = group.get("options").and_then(Value::as_array);
        let group_name = group
            .get("group")
            .or_else(|| group.get("name"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let entries: Vec<&Value> = match grouped {
            Some(items) => items.iter().collect(),
            None => vec![group],
        };
        for entry in entries {
            let value = entry
                .get("value")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let (provider, id) = split_model_value(&value);
            let label = entry
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .unwrap_or(&id)
                .to_string();
            models.push(EngineModel {
                id,
                label,
                provider: provider.or_else(|| group_name.clone()),
                variants: variants.clone(),
                default_variant: default_variant.clone(),
                is_default: Some(&value) == current_model.as_ref(),
            });
        }
    }
    models
}

/// `["deepseek-official","deepseek-v4-flash"]` → (`deepseek-official`, `deepseek-v4-flash`)。
fn split_model_value(value: &str) -> (Option<String>, String) {
    if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(value) {
        let parts: Vec<String> = items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect();
        if parts.len() == 2 {
            return (Some(parts[0].clone()), parts[1].clone());
        }
        if let Some(last) = parts.last() {
            return (None, last.clone());
        }
    }
    (None, value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        serde_json::json!([
            {
                "id": "model",
                "name": "Model",
                "category": "model",
                "type": "select",
                "currentValue": "[\"deepseek-official\",\"deepseek-v4-flash\"]",
                "options": [
                    {
                        "group": "deepseek-official",
                        "name": "DeepSeek",
                        "options": [
                            {
                                "value": "[\"deepseek-official\",\"deepseek-v4-flash\"]",
                                "name": "deepseek-v4-flash"
                            }
                        ]
                    }
                ]
            },
            {
                "id": "reasoning_effort",
                "name": "Reasoning effort",
                "category": "thought_level",
                "type": "select",
                "currentValue": "high",
                "options": [
                    { "value": "off", "name": "Off" },
                    { "value": "low", "name": "Low" },
                    { "value": "high", "name": "High" },
                    { "value": "max", "name": "Max" }
                ]
            }
        ])
    }

    #[test]
    fn grouped_models_are_flattened() {
        let models = parse_models(&sample());
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "deepseek-v4-flash");
        assert_eq!(models[0].provider.as_deref(), Some("deepseek-official"));
        assert_eq!(models[0].label, "deepseek-v4-flash");
        assert!(models[0].is_default);
    }

    #[test]
    fn effort_levels_become_variants() {
        let models = parse_models(&sample());
        assert_eq!(models[0].variants, vec!["off", "low", "high", "max"]);
        assert_eq!(models[0].default_variant.as_deref(), Some("high"));
    }

    #[test]
    fn flat_option_shape_also_works() {
        let models = parse_models(&serde_json::json!([
            {
                "id": "model",
                "currentValue": "deepseek-v4-flash",
                "options": [{ "value": "deepseek-v4-flash", "name": "Flash" }]
            }
        ]));
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "deepseek-v4-flash");
        assert_eq!(models[0].label, "Flash");
        assert!(models[0].variants.is_empty());
    }

    #[test]
    fn missing_model_option_yields_nothing() {
        assert!(parse_models(&serde_json::json!([])).is_empty());
    }

    #[test]
    fn model_value_splits_provider_and_id() {
        assert_eq!(
            split_model_value("[\"deepseek-official\",\"deepseek-v4-flash\"]"),
            (
                Some("deepseek-official".to_string()),
                "deepseek-v4-flash".to_string()
            )
        );
        assert_eq!(
            split_model_value("deepseek-v4-flash"),
            (None, "deepseek-v4-flash".to_string())
        );
    }
}
