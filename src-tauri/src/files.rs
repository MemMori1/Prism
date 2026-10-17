//! 项目文件索引（组合器 @ 补全）与 Git 上下文快照（「插入上下文」）。
//!
//! - 文件索引：优先 `git ls-files`（跟踪 + 未跟踪、尊重 .gitignore），
//!   非仓库目录回落为目录遍历（深度受限、跳过依赖/构建目录）。
//! - 排序：文件名精确 > 文件名前缀 > 文件名包含 > 路径包含；同分短路径优先。
//! - Git 快照：分支 + 变更列表 + 最近提交，供发送框「＋上下文」快捷插入。

use serde::Serialize;
use std::path::Path;
use std::process::Command;

/// 文件索引上限（防超大仓库撑爆内存）。
const MAX_FILES: usize = 4000;
/// 遍历回落时的最大深度。
const MAX_DEPTH: usize = 6;
/// 遍历时跳过的目录名。
const IGNORED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    ".next",
    ".nuxt",
    ".turbo",
    ".cache",
    "coverage",
    "__pycache__",
    ".venv",
    "venv",
    "Pods",
    "DerivedData",
];

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileHit {
    /// 相对项目根目录的路径（/ 分隔）。
    pub path: String,
    /// 文件名（末段）。
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitSnapshot {
    /// 当前分支（detached 时为短 sha）。
    pub branch: Option<String>,
    /// `git status --porcelain` 行（截断）。
    pub dirty: Vec<String>,
    /// `git log --oneline` 行（截断）。
    pub commits: Vec<String>,
}

fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// 查询词对路径的排名：越小越靠前；None 表示不匹配。
fn rank(path: &str, query: &str) -> Option<u8> {
    let path_lower = path.to_lowercase();
    if query.is_empty() {
        return Some(0);
    }
    let name_lower = basename(&path_lower);
    if name_lower == query {
        return Some(0);
    }
    if name_lower.starts_with(query) {
        return Some(1);
    }
    if name_lower.contains(query) {
        return Some(2);
    }
    if path_lower.contains(query) {
        return Some(3);
    }
    None
}

/// 过滤 + 排序 + 截断（纯函数，便于测试）。
pub fn rank_files(mut files: Vec<FileHit>, query: &str, limit: usize) -> Vec<FileHit> {
    let query = query.trim().to_lowercase();
    let mut scored: Vec<(u8, usize, FileHit)> = files
        .drain(..)
        .filter_map(|file| {
            rank(&file.path, &query).map(|score| (score, file.path.len(), file))
        })
        .collect();
    scored.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)).then_with(|| a.2.path.cmp(&b.2.path)));
    scored.into_iter().take(limit.clamp(1, 200)).map(|(_, _, file)| file).collect()
}

/// 用 git 列出文件（跟踪 + 未跟踪、尊重 .gitignore）。
fn git_ls_files(directory: &str) -> Option<Vec<FileHit>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let files: Vec<FileHit> = text
        .split('\0')
        .filter(|item| !item.trim().is_empty())
        .take(MAX_FILES)
        .map(|item| FileHit {
            path: item.replace('\\', "/"),
            name: basename(item),
        })
        .collect();
    (!files.is_empty()).then_some(files)
}

/// 目录遍历回落（跳过隐藏目录与常见依赖/构建目录）。
fn walk_files(directory: &str) -> Vec<FileHit> {
    let mut files = Vec::new();
    let root = Path::new(directory);
    let mut stack: Vec<(std::path::PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_DEPTH || files.len() >= MAX_FILES {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                if name.starts_with('.') || IGNORED_DIRS.contains(&name.as_str()) {
                    continue;
                }
                stack.push((entry.path(), depth + 1));
            } else if kind.is_file() {
                let path = entry.path();
                let Ok(relative) = path.strip_prefix(root) else { continue };
                let path_text = relative.to_string_lossy().replace('\\', "/");
                files.push(FileHit {
                    name: name.clone(),
                    path: path_text,
                });
                if files.len() >= MAX_FILES {
                    break;
                }
            }
        }
    }
    files
}

/// 列出项目文件（按查询词排名）。
pub fn list_project_files(directory: &str, query: &str, limit: usize) -> Result<Vec<FileHit>, String> {
    let directory = directory.trim();
    if directory.is_empty() {
        return Err("目录为空".to_string());
    }
    if !Path::new(directory).is_dir() {
        return Err(format!("目录不存在：{directory}"));
    }
    let files = git_ls_files(directory).unwrap_or_else(|| walk_files(directory));
    Ok(rank_files(files, query, limit))
}

fn git_lines(directory: &str, args: &[&str], max: usize) -> Vec<String> {
    let output = Command::new("git").arg("-C").arg(directory).args(args).output();
    let Ok(output) = output else { return Vec::new() };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .take(max)
        .map(|line| line.trim_end().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

/// Git 上下文快照（分支 / 变更 / 最近提交）。
pub fn git_snapshot(directory: &str) -> Result<GitSnapshot, String> {
    let directory = directory.trim();
    if directory.is_empty() {
        return Err("目录为空".to_string());
    }
    let probe = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map_err(|cause| format!("无法运行 git：{cause}"))?;
    let inside = probe.status.success()
        && String::from_utf8_lossy(&probe.stdout).trim() == "true";
    if !inside {
        return Err("该目录不是 Git 仓库".to_string());
    }

    let branch = {
        let current = git_lines(directory, &["branch", "--show-current"], 1);
        if let Some(name) = current.first().filter(|name| !name.is_empty()) {
            Some(name.clone())
        } else {
            git_lines(directory, &["rev-parse", "--short", "HEAD"], 1).into_iter().next()
        }
    };
    Ok(GitSnapshot {
        branch,
        dirty: git_lines(directory, &["status", "--porcelain=v1", "-uall"], 25),
        commits: git_lines(directory, &["log", "--oneline", "-8"], 8),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "prism-files-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("创建临时目录");
        dir
    }

    fn touch(root: &Path, relative: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().expect("父目录")).expect("建目录");
        fs::write(path, "x").expect("写文件");
    }

    #[test]
    fn ranks_name_first_then_path() {
        let files = vec![
            FileHit { path: "src/components/button".into(), name: "button".into() },
            FileHit { path: "src/button.ts".into(), name: "button.ts".into() },
            FileHit { path: "docs/button-guide.md".into(), name: "button-guide.md".into() },
            FileHit { path: "src/my-button.ts".into(), name: "my-button.ts".into() },
            FileHit { path: "src/other.ts".into(), name: "other.ts".into() },
        ];
        let hits = rank_files(files, "button", 10);
        assert_eq!(hits[0].path, "src/components/button", "文件名精确最靠前");
        assert_eq!(hits[1].path, "src/button.ts", "文件名前缀次之（同前缀短路径优先）");
        assert_eq!(hits[2].path, "docs/button-guide.md", "文件名前缀再次");
        assert_eq!(hits[3].path, "src/my-button.ts", "文件名包含排最后");
        assert_eq!(hits.len(), 4, "不匹配的应被过滤");
    }

    #[test]
    fn empty_query_lists_shortest_paths() {
        let files = vec![
            FileHit { path: "b/c/d/deep.ts".into(), name: "deep.ts".into() },
            FileHit { path: "a.ts".into(), name: "a.ts".into() },
        ];
        let hits = rank_files(files, "", 10);
        assert_eq!(hits[0].path, "a.ts");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn walk_skips_ignored_dirs() {
        let root = temp_dir("walk");
        touch(&root, "src/main.ts");
        touch(&root, "node_modules/pkg/index.js");
        touch(&root, "target/debug/build.o");
        touch(&root, ".git/HEAD");
        touch(&root, "docs/readme.md");
        let files = walk_files(root.to_str().expect("路径"));
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"src/main.ts"));
        assert!(paths.contains(&"docs/readme.md"));
        assert!(!paths.iter().any(|p| p.contains("node_modules")), "应跳过 node_modules：{paths:?}");
        assert!(!paths.iter().any(|p| p.starts_with("target/")), "应跳过 target：{paths:?}");
        assert!(!paths.iter().any(|p| p.starts_with(".git/")), "应跳过 .git：{paths:?}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn list_uses_git_and_fallback() {
        let root = temp_dir("list");
        touch(&root, "src/lib.rs");
        // 非 git 目录 → 遍历回落
        let hits = list_project_files(root.to_str().expect("路径"), "lib", 10).expect("列出");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "src/lib.rs");

        // git 仓库：未跟踪文件也应出现（-o --exclude-standard）
        let status = Command::new("git").arg("-C").arg(&root).args(["init", "-q"]).status();
        if status.map(|s| s.success()).unwrap_or(false) {
            touch(&root, "notes/todo.md");
            let hits = list_project_files(root.to_str().expect("路径"), "todo", 10).expect("列出");
            assert_eq!(hits[0].path, "notes/todo.md");
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn git_snapshot_reads_branch_and_dirty() {
        let root = temp_dir("snap");
        let init = Command::new("git").arg("-C").arg(&root).args(["init", "-q"]).status();
        if !init.map(|s| s.success()).unwrap_or(false) {
            return; // 无 git 环境则跳过
        }
        fs::write(root.join("a.txt"), "a").expect("写文件");
        let snapshot = git_snapshot(root.to_str().expect("路径")).expect("快照");
        assert!(snapshot.branch.is_some());
        assert!(snapshot.dirty.iter().any(|line| line.contains("a.txt")), "应含未跟踪文件：{:?}", snapshot.dirty);
        // 非仓库目录报错
        let plain = temp_dir("plain");
        assert!(git_snapshot(plain.to_str().expect("路径")).is_err());
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&plain);
    }
}
