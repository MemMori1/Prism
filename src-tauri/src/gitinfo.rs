//! 目录的 git 工作区信息（分支 / 主仓库根 / 是否链接 worktree）。
//!
//! 只读文件系统（不调 git 命令）：向上找 `.git`；链接 worktree 时
//! `.git` 是文本指针「gitdir: <主仓>/.git/worktrees/<name>」。
//! 目录可能已被删除 —— 查不到就返回空项，绝不报错。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// 最多向上找 40 层（防异常深路径/软链环）。
const MAX_DEPTH: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitInfo {
    /// 该目录所属仓库的主工作区根（非 git 目录为 None）。
    pub repo_root: Option<String>,
    /// 当前分支（分离头指针时为短 sha）。
    pub branch: Option<String>,
    /// 是否链接 worktree（非主工作区）。
    pub is_worktree: bool,
}

impl GitInfo {
    fn none() -> Self {
        GitInfo {
            repo_root: None,
            branch: None,
            is_worktree: false,
        }
    }
}

/// 读 `.git` 指针文件里的 `gitdir: …` 路径。
fn read_gitdir(dot_git: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(dot_git).ok()?;
    let raw = text.lines().find_map(|line| line.strip_prefix("gitdir:"))?.trim();
    if raw.is_empty() {
        return None;
    }
    let candidate = PathBuf::from(raw);
    if candidate.is_absolute() {
        Some(candidate)
    } else {
        dot_git.parent().map(|parent| parent.join(candidate))
    }
}

/// `<主仓>/.git/worktrees/<name>` → `<主仓>`。
fn worktree_main_root(gitdir: &Path) -> Option<PathBuf> {
    let worktrees = gitdir.parent()?;
    let dot_git = worktrees.parent()?;
    if worktrees.file_name()? != "worktrees" || dot_git.file_name()? != ".git" {
        return None;
    }
    dot_git.parent().map(Path::to_path_buf)
}

/// 读 HEAD：`ref: refs/heads/x` → x；分离头指针 → 短 sha。
fn read_head(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("HEAD")).ok()?;
    let line = text.trim();
    if let Some(name) = line.strip_prefix("ref: refs/heads/") {
        return Some(name.to_string());
    }
    if let Some(name) = line.strip_prefix("ref: ") {
        return name.rsplit('/').next().map(str::to_string);
    }
    if line.len() >= 7 && line.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Some(line[..7].to_string());
    }
    None
}

/// 从某个目录向上找 `.git`。
pub fn git_info(path: &Path) -> GitInfo {
    let mut current = path.to_path_buf();
    for _ in 0..MAX_DEPTH {
        let dot_git = current.join(".git");
        if dot_git.is_dir() {
            return GitInfo {
                repo_root: Some(current.to_string_lossy().to_string()),
                branch: read_head(&dot_git),
                is_worktree: false,
            };
        }
        if dot_git.is_file() {
            if let Some(gitdir) = read_gitdir(&dot_git) {
                let repo_root = worktree_main_root(&gitdir).unwrap_or_else(|| current.clone());
                let branch = read_head(&gitdir);
                return GitInfo {
                    repo_root: Some(repo_root.to_string_lossy().to_string()),
                    branch,
                    is_worktree: true,
                };
            }
        }
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => break,
        }
    }
    GitInfo::none()
}

/// 批量查询（键 = 传入的原路径字符串）。
pub fn git_info_batch(paths: &[String]) -> HashMap<String, GitInfo> {
    let mut map = HashMap::new();
    for raw in paths {
        map.insert(raw.clone(), git_info(Path::new(raw)));
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prism-gitinfo-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("临时目录");
        dir
    }

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("父目录");
        }
        std::fs::write(path, text).expect("写文件");
    }

    #[test]
    fn finds_main_repo_and_branch_from_subdir() {
        let root = temp_root("main");
        let repo = root.join("repo");
        write(&repo.join(".git/HEAD"), "ref: refs/heads/main\n");
        std::fs::create_dir_all(repo.join("src/deep")).expect("子目录");

        let info = git_info(&repo.join("src/deep"));
        assert_eq!(info.repo_root.as_deref(), Some(repo.to_string_lossy().as_ref()));
        assert_eq!(info.branch.as_deref(), Some("main"));
        assert!(!info.is_worktree);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolves_linked_worktree_to_main_root() {
        let root = temp_root("wt");
        let main = root.join("repo");
        write(&main.join(".git/HEAD"), "ref: refs/heads/main\n");
        let gitdir = main.join(".git/worktrees/feature");
        write(&gitdir.join("HEAD"), "ref: refs/heads/feature/x\n");

        let worktree = root.join("repo-feature");
        std::fs::create_dir_all(&worktree).expect("worktree 目录");
        write(
            &worktree.join(".git"),
            &format!("gitdir: {}\n", gitdir.to_string_lossy()),
        );

        let info = git_info(&worktree);
        assert_eq!(info.repo_root.as_deref(), Some(main.to_string_lossy().as_ref()));
        assert_eq!(info.branch.as_deref(), Some("feature/x"));
        assert!(info.is_worktree);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn detached_head_shows_short_sha_and_non_git_is_empty() {
        let root = temp_root("detached");
        let repo = root.join("plain");
        write(&repo.join(".git/HEAD"), "a1b2c3d4e5f6a7b8\n");
        let info = git_info(&repo);
        assert_eq!(info.branch.as_deref(), Some("a1b2c3d"), "短 sha");

        let none = git_info(&root.join("not-a-repo"));
        assert_eq!(none, GitInfo::none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn batch_maps_every_input_path() {
        let root = temp_root("batch");
        let repo = root.join("r");
        write(&repo.join(".git/HEAD"), "ref: refs/heads/dev\n");
        let missing = root.join("gone").to_string_lossy().to_string();
        let paths = vec![
            repo.to_string_lossy().to_string(),
            missing.clone(),
        ];
        let map = git_info_batch(&paths);
        assert_eq!(map[&repo.to_string_lossy().to_string()].branch.as_deref(), Some("dev"));
        assert!(map[&missing].repo_root.is_none(), "已删除目录返回空项");
        let _ = std::fs::remove_dir_all(&root);
    }
}
