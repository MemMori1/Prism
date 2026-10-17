//! 本地交付物读取（时间线里引用的本地 HTML / SVG / 图片 → 就地预览）。
//!
//! 安全策略：只读普通文件；类型白名单；超限文件直接拒绝（不做截断展示，
//! 避免半截页面误导）；文本类回传 UTF-8 文本（lossy），图片类回传 base64。

use std::fs;
use std::path::Path;

use base64::Engine as _;
use serde::Serialize;

/// 文本类（HTML / SVG）大小上限。
const TEXT_LIMIT: u64 = 2 * 1024 * 1024;
/// 图片大小上限。
const IMAGE_LIMIT: u64 = 8 * 1024 * 1024;

/// 交付物内容（就地预览用）。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactData {
    /// html / svg / image
    pub kind: String,
    pub name: String,
    pub size: u64,
    pub mime: String,
    /// 文本类内容（html / svg）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// 图片 base64（不含 data: 前缀，前端自行拼装）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base64: Option<String>,
}

/// 按扩展名判定可预览类型：(kind, mime, 大小上限)。
fn classify(path: &Path) -> Option<(&'static str, &'static str, u64)> {
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())?;
    match ext.as_str() {
        "html" | "htm" => Some(("html", "text/html", TEXT_LIMIT)),
        "svg" => Some(("svg", "image/svg+xml", TEXT_LIMIT)),
        "png" => Some(("image", "image/png", IMAGE_LIMIT)),
        "jpg" | "jpeg" => Some(("image", "image/jpeg", IMAGE_LIMIT)),
        "gif" => Some(("image", "image/gif", IMAGE_LIMIT)),
        "webp" => Some(("image", "image/webp", IMAGE_LIMIT)),
        "bmp" => Some(("image", "image/bmp", IMAGE_LIMIT)),
        "avif" => Some(("image", "image/avif", IMAGE_LIMIT)),
        _ => None,
    }
}

/// 读取交付物内容。目录、缺失文件、不支持的类型、超限文件都时报错。
pub fn read_artifact(path: &Path) -> Result<ArtifactData, String> {
    let path = fs::canonicalize(path).map_err(|cause| format!("文件不存在或不可读：{cause}"))?;
    let metadata = fs::metadata(&path).map_err(|cause| format!("读取文件失败：{cause}"))?;
    if !metadata.is_file() {
        return Err("只能预览普通文件".to_string());
    }
    let (kind, mime, limit) = classify(&path)
        .ok_or_else(|| "暂不支持预览该类型（支持 HTML / SVG / 常见图片）".to_string())?;
    let size = metadata.len();
    if size > limit {
        return Err(format!(
            "文件过大（上限 {} MiB），请到系统里打开",
            limit / (1024 * 1024)
        ));
    }
    let name = path
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_default();
    let bytes = fs::read(&path).map_err(|cause| format!("读取文件失败：{cause}"))?;

    let (text, base64) = if kind == "image" {
        (
            None,
            Some(base64::engine::general_purpose::STANDARD.encode(&bytes)),
        )
    } else {
        (Some(String::from_utf8_lossy(&bytes).to_string()), None)
    };
    Ok(ArtifactData {
        kind: kind.to_string(),
        name,
        size,
        mime: mime.to_string(),
        text,
        base64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prism-artifact-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("临时目录");
        dir
    }

    #[test]
    fn reads_html_as_text() {
        let dir = temp_dir("html");
        let file = dir.join("card.html");
        fs::write(&file, "<h1>你好</h1>").expect("写文件");
        let data = read_artifact(&file).expect("应可读");
        assert_eq!(data.kind, "html");
        assert_eq!(data.mime, "text/html");
        assert_eq!(data.text.as_deref(), Some("<h1>你好</h1>"));
        assert!(data.base64.is_none());
        assert_eq!(data.name, "card.html");
    }

    #[test]
    fn reads_png_as_base64() {
        let dir = temp_dir("png");
        let file = dir.join("shot.png");
        fs::write(&file, [0x89u8, 0x50, 0x4E, 0x47]).expect("写文件");
        let data = read_artifact(&file).expect("应可读");
        assert_eq!(data.kind, "image");
        assert_eq!(data.mime, "image/png");
        assert_eq!(data.base64.as_deref(), Some("iVBORw=="));
        assert!(data.text.is_none());
    }

    #[test]
    fn rejects_unsupported_oversize_and_missing() {
        let dir = temp_dir("reject");
        let txt = dir.join("notes.txt");
        fs::write(&txt, "hi").expect("写文件");
        assert!(read_artifact(&txt).is_err(), "txt 不在白名单");

        let big = dir.join("big.html");
        fs::write(&big, vec![b'a'; (TEXT_LIMIT + 1) as usize]).expect("写文件");
        assert!(read_artifact(&big).is_err(), "超限拒绝");

        assert!(read_artifact(&dir).is_err(), "目录拒绝");
        assert!(read_artifact(&dir.join("missing.html")).is_err(), "缺失拒绝");
    }
}
