//! Claude Code 数据定位：`$CLAUDE_CONFIG_DIR`（默认 `~/.claude`）。
//!
//! 数据主权：项目档案目录一律只读；Prism 不写引擎目录。

use std::path::{Path, PathBuf};

pub const ENGINE_ID: &str = "claude";
pub const DISPLAY_NAME: &str = "Claude Code";

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// 配置根目录：`$CLAUDE_CONFIG_DIR`（Claude CLI 官方变量）或 `~/.claude`。
pub fn config_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    home_dir().map(|home| home.join(".claude"))
}

/// 会话档案根目录：`<config>/projects/<编码 cwd>/<会话 uuid>.jsonl`。
pub fn projects_dir() -> Option<PathBuf> {
    config_root().map(|root| root.join("projects"))
}

/// claude CLI 可执行文件位置（Claude Code 安装位；未安装返回 None，运行轴后置）。
pub fn cli_path() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(root) = home_dir() {
        // `claude` 官方安装脚本落到 ~/.claude/local；npm -g 常落 ~/.local/bin。
        candidates.push(root.join(".claude").join("local").join("claude"));
        candidates.push(root.join(".local").join("bin").join("claude"));
    }
    candidates.push(PathBuf::from("/opt/homebrew/bin/claude"));
    candidates.push(PathBuf::from("/usr/local/bin/claude"));
    candidates.into_iter().find(|path| path.is_file())
}

/// 项目目录名编码（对齐 Claude CLI 口径）：ASCII 字母数字保留，其余一律 '-'。
pub fn encode_project_dir(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

/// 采信守卫：会话里的 cwd 编码回去与目录名一致才认（防 worktree 迁移等漂移）。
pub fn cwd_matches_dir(cwd: &str, dir_name: &str) -> bool {
    encode_project_dir(Path::new(cwd)) == dir_name
}

/// 兜底解码（目录名 → 展示路径）：'-' 既作分隔符又是被编码字符，天然有损，
/// 仅在目录内所有会话都没有可采信 cwd 时使用（极少见）。
pub fn decode_dir_name(dir_name: &str) -> String {
    let stripped = dir_name.trim_start_matches('-');
    format!("/{}", stripped.replace('-', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_project_dirs_like_cli() {
        assert_eq!(
            encode_project_dir(Path::new("/Users/example/Work/demo")),
            "-Users-example-Work-demo"
        );
        // '.' 与 '_' 同样编码为 '-'（有损、不可逆，故 cwd 优先从档案内取）。
        assert_eq!(encode_project_dir(Path::new("/tmp/my.app_v2")), "-tmp-my-app-v2");
        assert!(cwd_matches_dir("/tmp/demo", "-tmp-demo"));
        assert!(!cwd_matches_dir("/tmp/other", "-tmp-demo"));
    }

    #[test]
    fn decodes_dir_name_lossy() {
        assert_eq!(decode_dir_name("-tmp-demo"), "/tmp/demo");
    }

    #[test]
    fn honors_config_dir_env() {
        let sample = std::env::temp_dir().join(format!("prism-claude-cfg-{}", std::process::id()));
        std::env::set_var("CLAUDE_CONFIG_DIR", &sample);
        assert_eq!(projects_dir(), Some(sample.join("projects")));
        std::env::remove_var("CLAUDE_CONFIG_DIR");
        // 移除后回落到 ~/.claude/projects（HOME 在测试环境必然存在）。
        assert_eq!(
            projects_dir(),
            home_dir().map(|home| home.join(".claude").join("projects"))
        );
    }
}
