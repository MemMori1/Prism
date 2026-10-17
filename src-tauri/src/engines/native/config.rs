//! 原生引擎的模型服务配置（`~/.prism/native/config.json`）。
//!
//! 一个「服务」= 一个 OpenAI 兼容端点（baseUrl + 密钥）+ 它下面勾选的模型。
//! 结构参照 DeepSeek Harness 的 Settings → Models：可以挂多个服务，各自一份
//! 密钥与模型清单，全局只有一个「当前模型」。这样 DeepSeek 官方、公司网关、
//! 本地 Ollama 能并存，切模型不用重填 key。
//!
//! 密钥一律不返回给界面（只回 `hasKey` + 末四位），落盘 0600。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT_MAX_TURNS: u32 = 12;
/// 目前只实现了 OpenAI 兼容协议；字段留着，将来加 Anthropic 时不用改结构。
pub const PROTOCOL_OPENAI: &str = "openai";

fn default_protocol() -> String {
    PROTOCOL_OPENAI.to_string()
}

/// 服务下的一个模型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelEntry {
    pub id: String,
    /// 展示名（`/models` 里有名字时用它，没有就省掉）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl ModelEntry {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: None,
        }
    }

    pub fn with_label(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: Some(label.into()),
        }
    }

    pub fn display(&self) -> &str {
        self.label
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(self.id.as_str())
    }

    /// 去掉首尾空白，空 id 视为无效；与 id 相同的展示名不落盘。
    fn normalize(mut self) -> Option<Self> {
        self.id = self.id.trim().to_string();
        if self.id.is_empty() {
            return None;
        }
        if let Some(label) = self.label.take() {
            let label = label.trim().to_string();
            self.label = if label.is_empty() || label == self.id {
                None
            } else {
                Some(label)
            };
        }
        Some(self)
    }
}

/// 一个模型服务（端点）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    /// 稳定 id，创建后不可改（请求与会话记录都用它）。
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default = "default_protocol")]
    pub protocol: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub models: Vec<ModelEntry>,
}

impl ProviderConfig {
    /// 地址在 = 能发请求。本地 Ollama / vLLM 往往不校验密钥，所以不强制 key。
    pub fn usable(&self) -> bool {
        !self.base_url.trim().is_empty()
    }

    /// 能发请求且至少勾了一个模型 = 可被选中。
    pub fn ready(&self) -> bool {
        self.usable() && !self.models.is_empty()
    }

    /// `https://host/v1` → `https://host/v1/chat/completions`
    pub fn endpoint(&self) -> String {
        let base = self.base_url.trim().trim_end_matches('/');
        if base.ends_with("/chat/completions") {
            base.to_string()
        } else {
            format!("{base}/chat/completions")
        }
    }

    /// `https://host/v1` → `https://host/v1/models`
    pub fn models_endpoint(&self) -> String {
        let base = self.base_url.trim().trim_end_matches('/');
        if base.ends_with("/models") {
            base.to_string()
        } else {
            format!("{base}/models")
        }
    }

    pub fn key_hint(&self) -> String {
        let key = self.api_key.trim();
        if key.chars().count() < 4 {
            return String::new();
        }
        key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect()
    }

    fn view(&self, active: Option<&Selection>) -> ProviderView {
        let is_active = active.map(|item| item.provider == self.id).unwrap_or(false);
        ProviderView {
            id: self.id.clone(),
            label: if self.label.trim().is_empty() {
                self.id.clone()
            } else {
                self.label.clone()
            },
            protocol: if self.protocol.trim().is_empty() {
                default_protocol()
            } else {
                self.protocol.clone()
            },
            base_url: self.base_url.clone(),
            has_key: !self.api_key.trim().is_empty(),
            key_hint: self.key_hint(),
            models: self
                .models
                .iter()
                .map(|model| ModelView {
                    id: model.id.clone(),
                    label: model.display().to_string(),
                    active: is_active
                        && active
                            .map(|item| item.model == model.id)
                            .unwrap_or(false),
                })
                .collect(),
            ready: self.ready(),
            active: is_active,
        }
    }
}

/// 当前选中的模型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Selection {
    pub provider: String,
    pub model: String,
}

/// 服务里单个模型回给界面的样子。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelView {
    pub id: String,
    pub label: String,
    /// 是否是当前选中的模型。
    pub active: bool,
}

/// 服务回给界面的样子（不含密钥原文）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderView {
    pub id: String,
    pub label: String,
    pub protocol: String,
    pub base_url: String,
    pub has_key: bool,
    pub key_hint: String,
    pub models: Vec<ModelView>,
    pub ready: bool,
    /// 当前选中的模型是否落在这个服务上（用来高亮卡片）。
    pub active: bool,
}

/// 整份配置的只读视图。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeConfigView {
    pub providers: Vec<ProviderView>,
    pub active: Option<Selection>,
    pub max_turns: u32,
    pub ready: bool,
    pub model_count: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeConfig {
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub active: Option<Selection>,
    #[serde(default)]
    pub max_turns: Option<u32>,
}

/// 落盘结构：兼容旧版单端点字段（`baseUrl` / `model` / `apiKey`）。
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredConfig {
    #[serde(default)]
    providers: Vec<ProviderConfig>,
    #[serde(default)]
    active: Option<Selection>,
    #[serde(default)]
    max_turns: Option<u32>,
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    api_key: String,
}

impl NativeConfig {
    pub fn max_turns(&self) -> u32 {
        self.max_turns.unwrap_or(DEFAULT_MAX_TURNS).clamp(1, 40)
    }

    pub fn find(&self, id: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|provider| provider.id == id)
    }

    /// 当前要用的「服务 + 模型」。
    ///
    /// 选中的那一对失效时（服务被删 / 模型取消勾选）自动落到第一个可用服务的
    /// 第一个模型，避免删完服务后引擎突然不可用。
    pub fn selection(&self) -> Option<(&ProviderConfig, &ModelEntry)> {
        if let Some(active) = &self.active {
            if let Some(provider) = self.find(&active.provider) {
                if provider.usable() {
                    if let Some(model) = provider.models.iter().find(|item| item.id == active.model) {
                        return Some((provider, model));
                    }
                }
            }
        }
        self.providers
            .iter()
            .find(|provider| provider.ready())
            .and_then(|provider| provider.models.first().map(|model| (provider, model)))
    }

    pub fn ready(&self) -> bool {
        self.selection().is_some()
    }

    pub fn model_count(&self) -> usize {
        self.providers.iter().map(|provider| provider.models.len()).sum()
    }

    /// 选中项失效时把它校正到一个真实存在的模型。
    fn rebalance(&mut self) {
        let (provider_id, model_id) = match self.selection() {
            Some((provider, model)) => (provider.id.clone(), model.id.clone()),
            None => return,
        };
        if self
            .active
            .as_ref()
            .map(|item| item.provider == provider_id && item.model == model_id)
            .unwrap_or(false)
        {
            return;
        }
        self.active = Some(Selection {
            provider: provider_id,
            model: model_id,
        });
    }

    pub fn view(&self) -> NativeConfigView {
        let resolved = self.selection().map(|(provider, model)| Selection {
            provider: provider.id.clone(),
            model: model.id.clone(),
        });
        NativeConfigView {
            providers: self
                .providers
                .iter()
                .map(|provider| provider.view(resolved.as_ref()))
                .collect(),
            active: resolved,
            max_turns: self.max_turns(),
            ready: self.ready(),
            model_count: self.model_count(),
        }
    }
}

/// 界面提交的服务；`api_key` 留空表示沿用已存密钥。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInput {
    /// 空 = 新建；有值 = 编辑（id 本身不可改）。
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub clear_key: bool,
    #[serde(default)]
    pub models: Vec<ModelEntry>,
}

/// 界面提交的运行参数。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitsInput {
    #[serde(default)]
    pub max_turns: Option<u32>,
}

fn normalize_base_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return None;
    }
    Some(trimmed.to_string())
}

fn host_of(base_url: &str) -> String {
    let rest = base_url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    rest.split('/').next().unwrap_or("").to_string()
}

fn slugify(text: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            dash = false;
        } else if !out.is_empty() && !dash {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// 由名字（退化到域名）生成不与现有服务冲突的 id。
fn unique_id(label: &str, base_url: &str, providers: &[ProviderConfig]) -> String {
    let mut base = slugify(label);
    if base.is_empty() || base == "provider" {
        base = slugify(&host_of(base_url));
    }
    if base.is_empty() {
        base = "provider".to_string();
    }
    if providers.iter().all(|provider| provider.id != base) {
        return base;
    }
    for index in 2.. {
        let candidate = format!("{base}-{index}");
        if providers.iter().all(|provider| provider.id != candidate) {
            return candidate;
        }
    }
    base
}

fn dedup_models(models: Vec<ModelEntry>) -> Vec<ModelEntry> {
    let mut out: Vec<ModelEntry> = Vec::new();
    for model in models.into_iter().filter_map(ModelEntry::normalize) {
        if out.iter().any(|item| item.id == model.id) {
            continue;
        }
        out.push(model);
    }
    out
}

/// 新增或更新一个服务（纯内存操作，便于单测）。返回它的 id。
pub fn apply_provider(config: &mut NativeConfig, input: ProviderInput) -> Result<String, String> {
    let base_url = normalize_base_url(&input.base_url)
        .ok_or_else(|| "Base URL 要以 http:// 或 https:// 开头".to_string())?;
    let models = dedup_models(input.models);
    let label = input.label.trim().to_string();

    let key = input.api_key.unwrap_or_default().trim().to_string();

    match input
        .id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(id) => {
            let Some(provider) = config
                .providers
                .iter_mut()
                .find(|provider| provider.id == id)
            else {
                return Err(format!("服务不存在：{id}"));
            };
            provider.base_url = base_url;
            if !label.is_empty() {
                provider.label = label;
            }
            provider.models = models;
            if input.clear_key {
                provider.api_key.clear();
            } else if !key.is_empty() {
                provider.api_key = key;
            }
            let id = provider.id.clone();
            config.rebalance();
            Ok(id)
        }
        None => {
            if config.providers.len() >= 12 {
                return Err("最多 12 个模型服务".to_string());
            }
            let id = unique_id(
                if label.is_empty() { &base_url } else { &label },
                &base_url,
                &config.providers,
            );
            config.providers.push(ProviderConfig {
                id: id.clone(),
                label: if label.is_empty() {
                    id.clone()
                } else {
                    label
                },
                protocol: default_protocol(),
                base_url,
                api_key: key,
                models,
            });
            config.rebalance();
            Ok(id)
        }
    }
}

/// 删除服务；选中的服务被删时自动改选别家。
pub fn remove_provider(config: &mut NativeConfig, id: &str) -> Result<(), String> {
    let before = config.providers.len();
    config.providers.retain(|provider| provider.id != id);
    if config.providers.len() == before {
        return Err(format!("服务不存在：{id}"));
    }
    if config
        .active
        .as_ref()
        .map(|item| item.provider == id)
        .unwrap_or(false)
    {
        config.active = None;
    }
    config.rebalance();
    Ok(())
}

/// 选默认模型（会话里没单独指定时用它）。
pub fn set_active(config: &mut NativeConfig, provider: &str, model: &str) -> Result<(), String> {
    let Some(entry) = config.find(provider) else {
        return Err(format!("服务不存在：{provider}"));
    };
    if !entry.models.iter().any(|item| item.id == model) {
        return Err(format!("{} 下没有模型 {model}", entry.label));
    }
    config.active = Some(Selection {
        provider: provider.to_string(),
        model: model.to_string(),
    });
    Ok(())
}

// ---------- 落盘 ----------

const DEFAULT_MAX_PROVIDERS: usize = 12;

/// 配置目录：`~/.prism/native`。
pub fn dir() -> Option<PathBuf> {
    let dir = crate::engines::opencode::search::prism_dir()?.join("native");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

pub fn config_path() -> Option<PathBuf> {
    Some(dir()?.join("config.json"))
}

fn from_stored(stored: StoredConfig) -> NativeConfig {
    let mut config = NativeConfig {
        providers: stored
            .providers
            .into_iter()
            .map(|mut provider| {
                provider.models = dedup_models(std::mem::take(&mut provider.models));
                if provider.protocol.trim().is_empty() {
                    provider.protocol = default_protocol();
                }
                provider
            })
            .collect(),
        active: stored.active,
        max_turns: stored.max_turns,
    };

    // 旧版单端点配置：迁移成一个服务，id / 模型都沿用原值。
    if config.providers.is_empty() {
        if let Some(base_url) = normalize_base_url(&stored.base_url) {
            let model = stored.model.trim().to_string();
            config.providers.push(ProviderConfig {
                id: "default".to_string(),
                label: "默认服务".to_string(),
                protocol: default_protocol(),
                base_url,
                api_key: stored.api_key,
                models: if model.is_empty() {
                    Vec::new()
                } else {
                    vec![ModelEntry::new(model.clone())]
                },
            });
            if !model.is_empty() {
                config.active = Some(Selection {
                    provider: "default".to_string(),
                    model,
                });
            }
        }
    }

    if config.providers.len() > DEFAULT_MAX_PROVIDERS {
        config.providers.truncate(DEFAULT_MAX_PROVIDERS);
    }
    config
}

/// 读配置：文件缺失 / 损坏一律当空配置（不要让引擎因为坏 JSON 起不来）。
pub fn load() -> NativeConfig {
    let Some(path) = config_path() else {
        return NativeConfig::default();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return NativeConfig::default();
    };
    let stored: StoredConfig = serde_json::from_str(&text).unwrap_or_default();
    from_stored(stored)
}

fn save(config: &NativeConfig) -> Result<(), String> {
    let path = config_path().ok_or_else(|| "无法定位 ~/.prism 目录".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建 {} 失败：{error}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(config).map_err(|error| error.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|error| format!("写入 {} 失败：{error}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, &path).map_err(|error| format!("重命名失败：{error}"))
}

/// 新增 / 更新服务。
pub fn save_provider(input: ProviderInput) -> Result<NativeConfigView, String> {
    let mut config = load();
    apply_provider(&mut config, input)?;
    save(&config)?;
    Ok(config.view())
}

/// 删除服务。
pub fn delete_provider(id: &str) -> Result<NativeConfigView, String> {
    let mut config = load();
    remove_provider(&mut config, id)?;
    save(&config)?;
    Ok(config.view())
}

/// 选默认模型。
pub fn select_model(provider: &str, model: &str) -> Result<NativeConfigView, String> {
    let mut config = load();
    set_active(&mut config, provider, model)?;
    save(&config)?;
    Ok(config.view())
}

/// 只改运行参数（最大轮次）。
pub fn update_limits(input: LimitsInput) -> Result<NativeConfigView, String> {
    let mut config = load();
    if let Some(turns) = input.max_turns {
        config.max_turns = Some(turns.clamp(1, 40));
    }
    save(&config)?;
    Ok(config.view())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(id: &str, models: &[&str]) -> ProviderConfig {
        ProviderConfig {
            id: id.to_string(),
            label: id.to_string(),
            protocol: default_protocol(),
            base_url: format!("https://{id}.example.com/v1"),
            api_key: "sk-test-1234".to_string(),
            models: models.iter().map(|model| ModelEntry::new(*model)).collect(),
        }
    }

    #[test]
    fn endpoint_normalizes_trailing_slash() {
        let mut item = provider("a", &["m"]);
        item.base_url = "https://api.example.com/v1/".to_string();
        assert_eq!(
            item.endpoint(),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(item.models_endpoint(), "https://api.example.com/v1/models");
    }

    #[test]
    fn endpoint_keeps_full_path() {
        let mut item = provider("a", &["m"]);
        item.base_url = "https://api.example.com/v1/chat/completions".to_string();
        assert_eq!(
            item.endpoint(),
            "https://api.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn view_hides_key_but_keeps_hint() {
        let mut config = NativeConfig::default();
        config.providers.push(provider("deepseek", &["deepseek-chat"]));
        config.active = Some(Selection {
            provider: "deepseek".to_string(),
            model: "deepseek-chat".to_string(),
        });
        let view = config.view();
        assert!(view.ready);
        assert_eq!(view.active.as_ref().unwrap().model, "deepseek-chat");
        let card = &view.providers[0];
        assert!(card.has_key);
        assert_eq!(card.key_hint, "1234");
        assert!(card.models[0].active);
        assert_eq!(view.model_count, 1);
    }

    #[test]
    fn ready_requires_base_url_and_model() {
        let mut config = NativeConfig::default();
        assert!(!config.ready());
        let mut item = provider("a", &["m"]);
        item.base_url.clear();
        config.providers.push(item);
        assert!(!config.ready(), "没有地址不算就绪");
        config.providers[0].base_url = "http://127.0.0.1:11434/v1".to_string();
        assert!(config.ready(), "本地端点不强制密钥");
        config.providers[0].models.clear();
        assert!(!config.ready());
    }

    #[test]
    fn legacy_single_endpoint_migrates() {
        let stored = StoredConfig {
            base_url: "https://api.deepseek.com/v1".to_string(),
            model: "deepseek-chat".to_string(),
            api_key: "sk-old".to_string(),
            max_turns: Some(8),
            ..Default::default()
        };
        let config = from_stored(stored);
        assert_eq!(config.providers.len(), 1);
        assert_eq!(config.providers[0].id, "default");
        assert_eq!(config.providers[0].models[0].id, "deepseek-chat");
        assert_eq!(config.active.as_ref().unwrap().provider, "default");
        assert_eq!(config.max_turns(), 8);
        assert!(config.ready());
    }

    #[test]
    fn apply_provider_creates_with_unique_id() {
        let mut config = NativeConfig::default();
        for label in ["DeepSeek", "DeepSeek"] {
            apply_provider(
                &mut config,
                ProviderInput {
                    label: label.to_string(),
                    base_url: "https://api.deepseek.com/v1".to_string(),
                    api_key: Some("sk-x".to_string()),
                    models: vec![ModelEntry::new("deepseek-chat")],
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let ids: Vec<&str> = config.providers.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["deepseek", "deepseek-2"]);
        // 新建后自动选中第一个可用模型
        assert_eq!(config.active.as_ref().unwrap().provider, "deepseek");
    }

    #[test]
    fn apply_provider_keeps_key_when_blank() {
        let mut config = NativeConfig::default();
        config.providers.push(provider("a", &["m"]));
        apply_provider(
            &mut config,
            ProviderInput {
                id: Some("a".to_string()),
                label: "A".to_string(),
                base_url: "https://a.example.com/v1".to_string(),
                api_key: Some("   ".to_string()),
                models: vec![ModelEntry::new("m")],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(config.providers[0].api_key, "sk-test-1234");
        assert_eq!(config.providers[0].label, "A");
    }

    #[test]
    fn apply_provider_can_clear_key() {
        let mut config = NativeConfig::default();
        config.providers.push(provider("a", &["m"]));
        apply_provider(
            &mut config,
            ProviderInput {
                id: Some("a".to_string()),
                base_url: "https://a.example.com/v1".to_string(),
                clear_key: true,
                models: vec![ModelEntry::new("m")],
                ..Default::default()
            },
        )
        .unwrap();
        assert!(config.providers[0].api_key.is_empty());
    }

    #[test]
    fn apply_provider_rejects_bad_url() {
        let mut config = NativeConfig::default();
        let error = apply_provider(
            &mut config,
            ProviderInput {
                label: "x".to_string(),
                base_url: "api.example.com".to_string(),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(error.contains("http"));
    }

    #[test]
    fn remove_provider_reselects_other_service() {
        let mut config = NativeConfig::default();
        config.providers.push(provider("a", &["a1"]));
        config.providers.push(provider("b", &["b1"]));
        config.active = Some(Selection {
            provider: "a".to_string(),
            model: "a1".to_string(),
        });
        remove_provider(&mut config, "a").unwrap();
        assert_eq!(config.active.as_ref().unwrap().provider, "b");
        assert_eq!(config.active.as_ref().unwrap().model, "b1");
        assert!(remove_provider(&mut config, "a").is_err());
    }

    #[test]
    fn select_model_must_exist() {
        let mut config = NativeConfig::default();
        config.providers.push(provider("a", &["a1"]));
        assert!(set_active(&mut config, "a", "a2").is_err());
        assert!(set_active(&mut config, "b", "a1").is_err());
        set_active(&mut config, "a", "a1").unwrap();
    }

    #[test]
    fn dedup_models_drops_blank_and_repeated() {
        let models = dedup_models(vec![
            ModelEntry::new("a"),
            ModelEntry::new(" a "),
            ModelEntry::new("  "),
            ModelEntry::with_label("b", "B"),
            ModelEntry::with_label("c", "c"),
        ]);
        assert_eq!(
            models,
            vec![ModelEntry::new("a"), ModelEntry::with_label("b", "B"), ModelEntry::new("c")]
        );
    }
}
