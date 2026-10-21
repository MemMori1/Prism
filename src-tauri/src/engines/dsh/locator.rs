//! dsh 数据定位：`$DSH_HOME`（官方变量，默认 `~/.dsh`）。
//!
//! 数据主权与另外三家一致：会话日志一律只读，Prism 不写引擎目录。
//! 唯一例外是 ACP 进程本身——它由 Prism 起、由 Prism 结束。

use std::path::{Path, PathBuf};

pub const ENGINE_ID: &str = "dsh";
pub const DISPLAY_NAME: &str = "DeepSeek Harness";
/// 实时对话用的 profile。官方把 ACP 定位成"外部控制器"入口，
/// 比 headless 更适合 Prism：一个长驻进程能连续跑多轮，还能列/恢复会话。
pub const PROFILE: &str = "acp";

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// dsh 主目录：`$DSH_HOME` 优先（官方解析顺序），否则 `~/.dsh`。
pub fn home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("DSH_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    home_dir().map(|home| home.join(".dsh"))
}

/// 会话日志根目录：`<home>/sessions/<编码 cwd>/<会话 id>/session.vN.jsonl.zstd`。
///
/// 版本号（实测 v4）会随 dsh 升级变，所以文件名按"前缀 + 后缀"匹配，
/// 见 `history::SESSION_FILE_PREFIX`。
pub fn sessions_dir() -> Option<PathBuf> {
    home().map(|home| home.join("sessions"))
}

/// `dsh` 可执行文件。
///
/// 查找顺序刻意从"用户显式指定"到"常见安装位"：
/// 1. `DSH_BIN`（路径写死，供打包版或非标准安装）
/// 2. `PATH`（`npm i -g` / `pnpm add -g` 之后的标准位置）
/// 3. 几个 npm 前缀与本地 bin 的兜底
///
/// **不认 npx 缓存**：那目录名是哈希、会被清理，指过去等于埋一个时好时坏的
/// 依赖。CLI 找不到时健康状态会说清楚，而不是假装能用。
pub fn cli_path() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("DSH_BIN").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }

    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if let Some(found) = first_executable(&dir) {
                return Some(found);
            }
        }
    }

    let mut fallbacks: Vec<PathBuf> = Vec::new();
    if let Some(home) = home_dir() {
        fallbacks.push(home.join(".local").join("bin"));
        fallbacks.push(home.join(".npm-global").join("bin"));
        fallbacks.push(home.join(".dsh").join("bin"));
    }
    fallbacks.push(PathBuf::from("/opt/homebrew/bin"));
    fallbacks.push(PathBuf::from("/usr/local/bin"));
    fallbacks
        .iter()
        .find_map(|dir| first_executable(dir))
}

/// 某个目录下的 dsh 可执行文件（Windows 上 npm 装出来的是 `.cmd`）。
fn first_executable(dir: &Path) -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["dsh.cmd", "dsh.exe", "dsh"]
    } else {
        &["dsh"]
    };
    names
        .iter()
        .map(|name| dir.join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn honors_dsh_home_env() {
        let sample = std::env::temp_dir().join(format!("prism-dsh-home-{}", std::process::id()));
        std::env::set_var("DSH_HOME", &sample);
        assert_eq!(home(), Some(sample.clone()));
        assert_eq!(sessions_dir(), Some(sample.join("sessions")));
        std::env::remove_var("DSH_HOME");
        // 回落到 ~/.dsh（测试环境的 HOME 必然存在）。
        assert_eq!(home(), home_dir().map(|home| home.join(".dsh")));
    }

    #[test]
    fn prefers_dsh_bin_override() {
        let dir = std::env::temp_dir().join(format!("prism-dsh-bin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录");
        let fake = dir.join("dsh");
        std::fs::write(&fake, b"#!/bin/sh\n").expect("写假可执行文件");
        std::env::set_var("DSH_BIN", &fake);
        assert_eq!(cli_path(), Some(fake));
        std::env::remove_var("DSH_BIN");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
