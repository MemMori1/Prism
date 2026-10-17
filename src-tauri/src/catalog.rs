//! 引擎能力目录（工作台组合器）：模型 / 斜杠命令的统一形状与解析。
//!
//! - OpenCode：GET /api/model（`data[]` 带 variants = 推理档位）、
//!   GET /api/model/default（默认模型）、GET /api/command（斜杠命令）。
//! - Codex：`model/list` RPC（supportedReasoningEfforts = 推理档位）。
//! - Claude：本机无 CLI 时由 ipc 层直接给 available=false 的空目录。

use serde::Serialize;
use serde_json::Value;

/// 一个可选模型（引擎中立形状）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineModel {
    /// 模型 id（OpenCode 为 modelID，Codex 为 model id）。
    pub id: String,
    /// 展示名（引擎给不出名字时回落为 id）。
    pub label: String,
    /// 提供方（OpenCode 有 providerID；Codex 为 None）。
    pub provider: Option<String>,
    /// 推理档位（变体）id 列表，按引擎顺序。
    pub variants: Vec<String>,
    /// 引擎标注的默认档位。
    pub default_variant: Option<String>,
    /// 是否引擎默认模型。
    pub is_default: bool,
}

/// 一条斜杠命令。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineCommand {
    pub name: String,
    pub description: String,
}

/// 某引擎在指定目录的能力目录。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineCatalog {
    pub engine: String,
    /// 该引擎运行轴是否可用（Claude 无 CLI 时 false）。
    pub available: bool,
    /// 不可用原因（给前端展示）。
    pub reason: Option<String>,
    pub models: Vec<EngineModel>,
    pub commands: Vec<EngineCommand>,
}

/// 空目录（引擎不可用时）。
pub fn unavailable(engine: &str, reason: impl Into<String>) -> EngineCatalog {
    EngineCatalog {
        engine: engine.to_string(),
        available: false,
        reason: Some(reason.into()),
        models: Vec::new(),
        commands: Vec::new(),
    }
}

// ---------------------------------------------------------------- OpenCode

/// 解析 `/api/model` 响应（已 unwrap `data`）。
/// `default_ref` 为 `/api/model/default` 的 `data`，用于标记默认模型。
pub fn parse_opencode_models(value: &Value, default_ref: Option<&Value>) -> Vec<EngineModel> {
    let default_pair = default_ref.map(|item| {
        (
            item.get("providerID").and_then(Value::as_str).unwrap_or_default().to_string(),
            item.get("modelID")
                .or_else(|| item.get("id"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        )
    });
    let items = value.as_array().cloned().unwrap_or_default();
    let mut models: Vec<EngineModel> = items
        .iter()
        .filter_map(|item| {
            let id = item
                .get("modelID")
                .or_else(|| item.get("id"))
                .and_then(Value::as_str)?
                .to_string();
            let provider = item
                .get("providerID")
                .and_then(Value::as_str)
                .map(str::to_string);
            let label = item
                .get("name")
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty())
                .unwrap_or(&id)
                .to_string();
            let variants = item
                .get("variants")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(|variant| {
                            variant.get("id").and_then(Value::as_str).map(str::to_string)
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(EngineModel {
                id,
                label,
                provider,
                variants,
                default_variant: None,
                is_default: false,
            })
        })
        .collect();
    if let Some((default_provider, default_model)) = default_pair {
        for model in models.iter_mut() {
            let provider_matches = model
                .provider
                .as_deref()
                .map(|provider| provider == default_provider.as_str())
                .unwrap_or(true);
            if model.id == default_model && provider_matches {
                model.is_default = true;
            }
        }
    }
    models
}

/// 解析 `/api/command` 响应（已 unwrap `data`）。
pub fn parse_opencode_commands(value: &Value) -> Vec<EngineCommand> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let name = item.get("name").and_then(Value::as_str)?.to_string();
                    let description = item
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    Some(EngineCommand { name, description })
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------- Codex

/// 解析 `model/list` 响应（整个 result：`{ data: [...] }`）。
pub fn parse_codex_models(result: &Value) -> Vec<EngineModel> {
    let items = result
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    items
        .iter()
        .filter_map(|item| {
            if item.get("hidden").and_then(Value::as_bool).unwrap_or(false) {
                return None;
            }
            let id = item
                .get("model")
                .or_else(|| item.get("id"))
                .and_then(Value::as_str)?
                .to_string();
            let label = item
                .get("displayName")
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty())
                .unwrap_or(&id)
                .to_string();
            let variants = item
                .get("supportedReasoningEfforts")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(|effort| {
                            effort
                                .get("reasoningEffort")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        })
                        .collect()
                })
                .unwrap_or_default();
            let default_variant = item
                .get("defaultReasoningEffort")
                .and_then(Value::as_str)
                .map(str::to_string);
            let is_default = item
                .get("isDefault")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Some(EngineModel {
                id,
                label,
                provider: None,
                variants,
                default_variant,
                is_default,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_opencode_models_with_variants_and_default() {
        // 真机形状（截取自 opencode serve /api/model）
        let data = json!([
            {
                "modelID": "nemotron-3.5-super-vl-free",
                "providerID": "opencode",
                "name": "Nemotron 3.5 Super VL Free",
                "variants": [
                    { "id": "none" }, { "id": "minimal" }, { "id": "low" },
                    { "id": "medium" }, { "id": "high" }, { "id": "xhigh" }
                ]
            },
            {
                "modelID": "deepseek-flash",
                "providerID": "deepseek",
                "variants": []
            }
        ]);
        let default_ref = json!({
            "id": "nemotron-3.5-super-vl-free",
            "modelID": "nemotron-3.5-super-vl-free",
            "providerID": "opencode"
        });
        let models = parse_opencode_models(&data, Some(&default_ref));
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "nemotron-3.5-super-vl-free");
        assert_eq!(models[0].provider.as_deref(), Some("opencode"));
        assert_eq!(models[0].label, "Nemotron 3.5 Super VL Free");
        assert_eq!(models[0].variants.len(), 6);
        assert!(models[0].is_default);
        // 无 name 时回落为 id；非默认模型不标记
        assert_eq!(models[1].label, "deepseek-flash");
        assert!(!models[1].is_default);
    }

    #[test]
    fn parses_opencode_commands() {
        let data = json!([
            { "name": "init", "description": "guided AGENTS.md setup" },
            { "name": "review", "description": "review changes" },
            { "name": "no-description" }
        ]);
        let commands = parse_opencode_commands(&data);
        assert_eq!(commands.len(), 3);
        assert_eq!(commands[0].name, "init");
        assert_eq!(commands[2].description, "");
    }

    #[test]
    fn parses_codex_models_skipping_hidden() {
        // 真机形状（截取自 codex app-server model/list）
        let result = json!({
            "data": [
                {
                    "id": "gpt-5.5",
                    "model": "gpt-5.5",
                    "displayName": "GPT-5.5",
                    "hidden": false,
                    "isDefault": true,
                    "defaultReasoningEffort": "medium",
                    "supportedReasoningEfforts": [
                        { "reasoningEffort": "low", "description": "fast" },
                        { "reasoningEffort": "medium", "description": "balanced" },
                        { "reasoningEffort": "high", "description": "deep" }
                    ]
                },
                { "id": "internal", "model": "internal", "hidden": true }
            ]
        });
        let models = parse_codex_models(&result);
        assert_eq!(models.len(), 1);
        let model = &models[0];
        assert_eq!(model.id, "gpt-5.5");
        assert_eq!(model.label, "GPT-5.5");
        assert_eq!(model.provider, None);
        assert_eq!(model.variants, vec!["low", "medium", "high"]);
        assert_eq!(model.default_variant.as_deref(), Some("medium"));
        assert!(model.is_default);
    }

    #[test]
    fn opencode_missing_fields_tolerated() {
        let models = parse_opencode_models(&json!([{ "modelID": "solo" }]), None);
        assert_eq!(models.len(), 1);
        assert!(models[0].variants.is_empty());
        assert!(!models[0].is_default);
        assert_eq!(parse_opencode_commands(&json!(null)).len(), 0);
    }
}
