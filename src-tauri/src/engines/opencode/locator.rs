//! OpenCode 的安装定位：数据目录与 CLI 可执行文件。

use std::path::{Path, PathBuf};

pub const ENGINE_ID: &str = "opencode";
pub const DISPLAY_NAME: &str = "OpenCode";

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// 数据目录：$XDG_DATA_HOME/opencode 或 ~/.local/share/opencode。
pub fn data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(dir).join("opencode"));
    }
    home_dir().map(|home| home.join(".local").join("share").join("opencode"))
}

/// opencode.db 位置（存在时才返回）。
pub fn database_path() -> Option<PathBuf> {
    let path = data_dir()?.join("opencode.db");
    path.is_file().then_some(path)
}

/// CLI 探测顺序：
/// 1. $PRISM_OPENCODE_CLI 显式指定；
/// 2. OpenCode 桌面应用内置副本；
/// 3. 桌面应用下载的托管副本（多版本取最新）；
/// 4. 常见本地安装位置与 PATH。
pub fn cli_path() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("PRISM_OPENCODE_CLI").map(PathBuf::from) {
        if explicit.is_file() {
            return Some(explicit);
        }
    }

    const APP_BUNDLE_CANDIDATES: [&str; 2] = [
        "/Applications/OpenCode.app/Contents/Resources/opencode-cli",
        "/Applications/opencode.app/Contents/Resources/opencode-cli",
    ];
    for candidate in APP_BUNDLE_CANDIDATES {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return Some(path);
        }
    }

    if let Some(home) = home_dir() {
        let managed_root = home
            .join("Library")
            .join("Application Support")
            .join("ai.opencode.desktop")
            .join("cli");
        if let Some(found) = newest_versioned_cli(&managed_root) {
            return Some(found);
        }
        for relative in [
            ".opencode/bin/opencode",
            ".local/bin/opencode",
            ".bun/bin/opencode",
        ] {
            let path = home.join(relative);
            if path.is_file() {
                return Some(path);
            }
        }
    }

    which("opencode-cli").or_else(|| which("opencode"))
}

fn newest_versioned_cli(root: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path().join("opencode-cli"))
        .filter(|path| path.is_file())
        .collect();
    candidates.sort();
    candidates.pop()
}

fn which(binary: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(binary))
        .find(|path| path.is_file())
}

/// 读取 CLI 版本（`--version` 输出首个非空行）；失败返回 None。
pub fn cli_version(cli: &Path) -> Option<String> {
    let output = std::process::Command::new(cli).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_version_reads_output() {
        // /bin/echo --version 原样打印参数
        assert_eq!(
            cli_version(Path::new("/bin/echo")).as_deref(),
            Some("--version")
        );
        // 无输出视为不可用；不存在的文件不报错
        assert!(cli_version(Path::new("/usr/bin/true")).is_none());
        assert!(cli_version(Path::new("/nonexistent/prism-test")).is_none());
    }
}
