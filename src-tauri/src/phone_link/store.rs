//! 手机链接的配置与配对设备持久化（`~/.prism/phone-link.json`）。
//!
//! 约定：文件是单一事实源（原子写：临时文件 + rename），读盘容错——文件不
//! 存在视为默认配置。Webhook 地址与设备令牌是敏感凭据：文件权限 0600，
//! 且绝不把原文回传 UI（只给「已配置 + 尾 4 位」提示，与 native 密钥同口径）。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::engines::opencode::search::prism_dir;

/// 允许手机驱动的引擎（与定时任务的运行轴同口径）。
pub const RUNTIME_ENGINES: [&str; 2] = ["opencode", "codex"];

/// 默认监听端口：vite dev 用 1520，这里顺延避开。
pub const DEFAULT_PORT: u16 = 15211;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PushEvents {
    /// 回合收口（成功 / 失败）时推送。
    pub on_done: bool,
    /// 引擎请求授权时推送。
    pub on_permission: bool,
    /// 回合报错时推送。
    pub on_error: bool,
}

impl Default for PushEvents {
    fn default() -> Self {
        Self {
            on_done: true,
            on_permission: true,
            on_error: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedDevice {
    pub name: String,
    pub token: String,
    pub paired_at: i64,
    pub last_seen_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PhoneLinkConfig {
    pub enabled: bool,
    pub engine: String,
    pub port: u16,
    /// 企业微信群机器人 webhook；明文落盘（与 native 密钥同口径），0600 保护。
    pub webhook_url: Option<String>,
    pub events: PushEvents,
    /// 当前配对的设备；至多一台，重新配对即顶掉旧设备。
    pub device: Option<PairedDevice>,
}

impl Default for PhoneLinkConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            engine: "opencode".to_string(),
            port: DEFAULT_PORT,
            webhook_url: None,
            events: PushEvents::default(),
            device: None,
        }
    }
}

pub fn store_path() -> Option<PathBuf> {
    prism_dir().map(|dir| dir.join("phone-link.json"))
}

/// 读取配置；文件不存在（或内容为空）返回默认配置，解析失败带上下文报错。
pub fn load(path: &Path) -> Result<PhoneLinkConfig, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PhoneLinkConfig::default())
        }
        Err(error) => return Err(format!("读取 {} 失败：{error}", path.display())),
    };
    if text.trim().is_empty() {
        return Ok(PhoneLinkConfig::default());
    }
    serde_json::from_str(&text).map_err(|error| format!("解析 {} 失败：{error}", path.display()))
}

/// 原子写配置（tmp + rename），unix 上收紧到 0600——里面存着凭据。
pub fn save(path: &Path, config: &PhoneLinkConfig) -> Result<(), String> {
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
    std::fs::rename(&tmp, path).map_err(|error| format!("替换 {} 失败：{error}", path.display()))
}

/// 校验保存进来的增量配置：引擎必须在运行轴名单、webhook 必须是 https。
pub fn validate(engine: &str, webhook_url: Option<&str>) -> Result<(), String> {
    if !RUNTIME_ENGINES.contains(&engine) {
        return Err(format!("手机会话暂不支持引擎 {engine}（可用：{}）", RUNTIME_ENGINES.join(" / ")));
    }
    if let Some(url) = webhook_url {
        let url = url.trim();
        if !url.is_empty() && !url.starts_with("https://") {
            return Err("Webhook 地址必须是 https:// 开头（企业微信群机器人地址）".to_string());
        }
    }
    Ok(())
}

/// Webhook 在 UI 上的提示：只暴露「已配置 + 尾 4 位」，不回传原文。
pub fn webhook_hint(url: Option<&str>) -> Option<String> {
    let url = url?.trim();
    if url.is_empty() {
        return None;
    }
    let tail: String = url.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    Some(format!("…{tail}"))
}

/// 128-bit 随机令牌（32 个 hex 字符）：配对令牌与设备令牌共用。
pub fn new_token() -> String {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).expect("系统随机源不可用");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prism-phone-link-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir.join("phone-link.json")
    }

    #[test]
    fn missing_file_reads_as_default() {
        let path = temp_path("missing");
        let config = load(&path).unwrap();
        assert!(!config.enabled);
        assert_eq!(config.engine, "opencode");
        assert_eq!(config.port, DEFAULT_PORT);
        assert!(config.device.is_none());
    }

    #[test]
    fn save_then_load_round_trip() {
        let path = temp_path("roundtrip");
        let mut config = PhoneLinkConfig::default();
        config.enabled = true;
        config.engine = "codex".to_string();
        config.webhook_url = Some("https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=abcd".into());
        config.device = Some(PairedDevice {
            name: "iPhone".into(),
            token: "t-1".into(),
            paired_at: 100,
            last_seen_at: 200,
        });
        save(&path, &config).unwrap();
        assert_eq!(load(&path).unwrap(), config);
        // 存的是凭据：文件权限应为 0600（unix）。
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "配置文件应收紧到 0600");
        }
    }

    #[test]
    fn corrupt_file_errors_with_context() {
        let path = temp_path("corrupt");
        std::fs::write(&path, "{not json").unwrap();
        let error = load(&path).unwrap_err();
        assert!(error.contains("解析"), "报错应带上下文：{error}");
    }

    #[test]
    fn validate_rejects_bad_engine_and_http_webhook() {
        assert!(validate("claude", None).is_err());
        assert!(validate("opencode", Some("http://qyapi.example.com")).is_err());
        assert!(validate("opencode", Some("https://qyapi.weixin.qq.com/x")).is_ok());
        assert!(validate("codex", None).is_ok());
    }

    #[test]
    fn webhook_hint_only_exposes_tail() {
        assert_eq!(webhook_hint(None), None);
        assert_eq!(webhook_hint(Some("  ")), None);
        assert_eq!(
            webhook_hint(Some("https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=9f8e7d6c")),
            Some("…7d6c".to_string())
        );
    }

    #[test]
    fn tokens_are_random_hex() {
        let a = new_token();
        let b = new_token();
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "两次生成不应相同");
    }
}
