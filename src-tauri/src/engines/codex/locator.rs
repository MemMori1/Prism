//! Codex 数据定位：`$CODEX_HOME`（默认 `~/.codex`）。
//!
//! 数据主权：状态库与 rollout 文件一律只读；Prism 不写引擎目录。

use std::path::{Path, PathBuf};

pub const ENGINE_ID: &str = "codex";
pub const DISPLAY_NAME: &str = "Codex";

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Codex 数据目录：`$CODEX_HOME` 或 `~/.codex`。
pub fn codex_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CODEX_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    home_dir().map(|home| home.join(".codex"))
}

/// 会话状态库：`state_*.sqlite`（文件名带版本号，按 schema 探测 `threads` 表）。
pub fn state_db() -> Option<PathBuf> {
    let home = codex_home()?;
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(&home)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            name.starts_with("state") && name.ends_with(".sqlite")
        })
        .collect();
    // 版本号大的优先（state_5 > state_4）；再确认确有 threads 表。
    candidates.sort();
    candidates.into_iter().rev().find(|path| has_threads_table(path))
}

fn has_threads_table(path: &Path) -> bool {
    rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .and_then(|connection| {
        connection.query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'threads'",
            [],
            |_| Ok(()),
        )
    })
    .is_ok()
}

/// codex CLI 可执行文件位置（Codex.app 内嵌 / 常见安装位）。
pub fn cli_path() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(root) = home_dir() {
        candidates.push(root.join(".codex").join("bin").join("codex"));
        candidates.push(root.join(".local").join("bin").join("codex"));
    }
    candidates.push(PathBuf::from(
        "/Applications/Codex.app/Contents/Resources/codex",
    ));
    candidates.push(PathBuf::from("/opt/homebrew/bin/codex"));
    candidates.push(PathBuf::from("/usr/local/bin/codex"));
    candidates.into_iter().find(|path| path.is_file())
}
