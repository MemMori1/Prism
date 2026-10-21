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

/// dsh 的 home 有没有问题（不可写 / 被别的用户占着）。
///
/// 这是"连接已断开"最常见的真凶：某次 `sudo dsh` 留下的 `~/.dsh` 属主是 root、
/// 权限 700，普通用户连读都不行。dsh 启动时会 `mkdir <home>/profiles/<profile>`，
/// 那里一失败进程立刻退，ACP 侧只能感知到 stdout 关了。
///
/// 探测方式就是建它本来就要建的目录，幂等且没有额外副作用——顺带把"父目录不存在"
/// 这种首次运行情况也变成可用。
pub fn home_problem() -> Option<String> {
    probe_writable(&home()?)
}

fn probe_writable(home: &Path) -> Option<String> {
    match std::fs::create_dir_all(home.join("profiles")) {
        Ok(()) => None,
        Err(error) => Some(format!(
            "dsh 主目录 {} 不可写（{error}）。若曾被 sudo 跑过，属主可能是 root：\
             sudo chown -R \"$(whoami)\" {}",
            home.display(),
            home.display()
        )),
    }
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
    fallbacks.iter().find_map(|dir| first_executable(dir))
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

    #[test]
    fn writable_home_has_no_problem() {
        let dir = std::env::temp_dir().join(format!("prism-dsh-ok-{}", std::process::id()));
        assert!(probe_writable(&dir).is_none());
        assert!(dir.join("profiles").is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_in_place_of_home_is_reported() {
        // 拿普通文件当 home：在它下面建 `profiles` 必然失败。这覆盖的是
        // "~/.dsh 被 root 占着" 那类现象——探测必须能报出来，否则用户只会
        // 看到 ACP 侧一句"连接已断开"。
        let file = std::env::temp_dir().join(format!("prism-dsh-file-{}", std::process::id()));
        std::fs::write(&file, b"not a directory").expect("写文件");
        assert!(probe_writable(&file).is_some());
        let _ = std::fs::remove_file(&file);
    }
}
