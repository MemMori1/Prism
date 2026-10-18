//! 文件变更追踪与预览：会话目录「这次改了什么」。
//!
//! 口径：**以工作树为准**，不去逐条解析各家 Agent 的工具调用。
//! 一次 `sed`、一次 `npm i`、一次重命名脚本能改到的文件，比工具卡里看得到的多；
//! 只有工作树本身知道最终发生了什么。所以这里全部是 `git status` / `git diff`
//! 的只读调用——不写 index、不动分支、不建提交，看的就是「相对 HEAD 的未提交改动」。
//!
//! 非仓库目录不是错误：返回 `isRepo = false`，面板给一句提示就好。

use serde::Serialize;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

/// 一次最多列多少个文件（超大改动只给前 N 个，并标 truncated）。
const MAX_FILES: usize = 300;
/// 单个 diff 的字节上限。
const MAX_DIFF: usize = 256 * 1024;
/// 预览文本的字节上限。
const MAX_TEXT: u64 = 512 * 1024;
/// 未跟踪文件数行的上限（防超大文件把数行拖死）。
const MAX_COUNT_LINES: usize = 40_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeFile {
    /// 绝对路径（预览 / 打开用）。
    pub path: String,
    /// 相对仓库根的路径（/ 分隔；diff 与显示用）。
    pub rel: String,
    /// added / modified / deleted / renamed
    pub status: String,
    /// 新文件 / 未跟踪（diff 得自己拼）。
    pub untracked: bool,
    pub additions: i64,
    pub deletions: i64,
    pub binary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeReport {
    /// 目录是不是在 Git 工作树里。
    pub is_repo: bool,
    /// 仓库根（相对路径都按它算）。
    pub root: Option<String>,
    /// 当前分支（detached 时为短 sha）。
    pub branch: Option<String>,
    pub files: Vec<ChangeFile>,
    pub additions: i64,
    pub deletions: i64,
    /// 文件太多被截断。
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    pub rel: String,
    /// unified diff（`git diff` 原样；未跟踪文件由内容拼）。
    pub patch: String,
    /// 超过上限被截断。
    pub truncated: bool,
    pub binary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileText {
    pub path: String,
    pub text: String,
    pub size: u64,
    /// 超过上限被截断。
    pub truncated: bool,
    /// 二进制（不解码，文本给空）。
    pub binary: bool,
}

// ---- git 只读调用 ----

fn git(directory: &str, args: &[&str]) -> Result<Output, String> {
    Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        .output()
        .map_err(|cause| format!("无法运行 git：{cause}"))
}

/// 仓库根；不在工作树里返回 None（不是错误）。
fn toplevel(directory: &str) -> Option<String> {
    let output = git(directory, &["rev-parse", "--show-toplevel"]).ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

fn branch_of(root: &str) -> Option<String> {
    let output = git(root, &["branch", "--show-current"]).ok()?;
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !name.is_empty() {
        return Some(name);
    }
    let short = git(root, &["rev-parse", "--short", "HEAD"]).ok()?;
    let sha = String::from_utf8_lossy(&short.stdout).trim().to_string();
    if sha.is_empty() {
        None
    } else {
        Some(sha)
    }
}

// ---- 解析（纯函数，便于单测）----

/// 状态字母：porcelain 的 XY 两列压成一个语义状态。
fn status_of(x: char, y: char) -> (&'static str, bool) {
    if x == '?' && y == '?' {
        return ("added", true);
    }
    if x == 'D' || y == 'D' {
        return ("deleted", false);
    }
    if x == 'A' {
        return ("added", false);
    }
    if x == 'R' || y == 'R' {
        return ("renamed", false);
    }
    ("modified", false)
}

/// 拆 `git status --porcelain=v1 -z`：`XY PATH\0`，重命名时 `XY NEW\0OLD\0`。
pub fn parse_status(raw: &[u8]) -> Vec<(String, &'static str, bool)> {
    let text = String::from_utf8_lossy(raw);
    let mut parts = text.split('\0');
    let mut out = Vec::new();
    while let Some(entry) = parts.next() {
        if entry.len() < 4 {
            continue;
        }
        let mut chars = entry.chars();
        let x = chars.next().unwrap_or(' ');
        let y = chars.next().unwrap_or(' ');
        let path = entry[3..].to_string();
        let (status, untracked) = status_of(x, y);
        // 重命名 / 复制：下一段是原路径（仅 -z 下如此），吃掉它免得当成一条记录。
        if x == 'R' || x == 'C' || y == 'R' || y == 'C' {
            parts.next();
        }
        if !path.is_empty() {
            out.push((path, status, untracked));
        }
    }
    out
}

/// 拆 `git diff --numstat`：`增加\t删除\t路径`（二进制是 `-`）。
pub fn parse_numstat(text: &str) -> Vec<(String, i64, i64, bool)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut fields = line.split('\t');
        let (Some(add), Some(del), Some(path)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let binary = add == "-" || del == "-";
        let additions = add.parse::<i64>().unwrap_or(0);
        let deletions = del.parse::<i64>().unwrap_or(0);
        let path = rename_target(path);
        if !path.is_empty() {
            out.push((path, additions, deletions, binary));
        }
    }
    out
}

/// 重命名的路径写法：`old => new` 或 `dir/{a => b}/c` → 取新路径。
pub fn rename_target(path: &str) -> String {
    if let Some(start) = path.find('{') {
        if let Some(offset) = path[start..].find('}') {
            let end = start + offset;
            if let Some((_, new)) = path[start + 1..end].split_once(" => ") {
                return format!("{}{}{}", &path[..start], new, &path[end + 1..]);
            }
        }
    }
    match path.rsplit_once(" => ") {
        Some((_, new)) => new.trim().to_string(),
        None => path.to_string(),
    }
}

/// 未跟踪文件的 diff：`git diff` 看不到它，按内容拼一份「全部新增」。
pub fn new_file_patch(rel: &str, text: &str) -> (String, i64) {
    let lines: Vec<&str> = text.lines().collect();
    let count = lines.len();
    let mut patch = format!(
        "diff --git a/{rel} b/{rel}\nnew file mode 100644\n--- /dev/null\n+++ b/{rel}\n@@ -0,0 +1,{count} @@\n"
    );
    for line in &lines {
        patch.push('+');
        patch.push_str(line);
        patch.push('\n');
    }
    if !text.is_empty() && !text.ends_with('\n') {
        patch.push_str("\\ No newline at end of file\n");
    }
    (patch, count as i64)
}

// ---- 变更报告 ----

/// 会话目录 → 未提交改动清单。
pub fn report(directory: &str) -> Result<ChangeReport, String> {
    let directory = directory.trim();
    if directory.is_empty() {
        return Err("会话没有工作目录".to_string());
    }
    let Some(root) = toplevel(directory) else {
        return Ok(ChangeReport {
            is_repo: false,
            root: None,
            branch: None,
            files: Vec::new(),
            additions: 0,
            deletions: 0,
            truncated: false,
        });
    };

    let status = git(&root, &["status", "--porcelain=v1", "-z", "-uall"])
        .map_err(|cause| format!("读取改动失败：{cause}"))?;
    if !status.status.success() {
        return Err(format!(
            "读取改动失败：{}",
            String::from_utf8_lossy(&status.stderr).trim()
        ));
    }

    // 计数：相对 HEAD（暂存 + 未暂存一次算完）。仓库还没有提交时退回工作树 + 暂存两份。
    let mut counts = Vec::new();
    if let Ok(output) = git(&root, &["diff", "--numstat", "HEAD", "--"]) {
        if output.status.success() {
            counts = parse_numstat(&String::from_utf8_lossy(&output.stdout));
        }
    }
    if counts.is_empty() {
        for args in [
            vec!["diff", "--numstat", "--"],
            vec!["diff", "--cached", "--numstat", "--"],
        ] {
            if let Ok(output) = git(&root, &args) {
                if output.status.success() {
                    counts.extend(parse_numstat(&String::from_utf8_lossy(&output.stdout)));
                }
            }
        }
    }

    let mut files = Vec::new();
    let mut truncated = false;
    for (rel, status_name, untracked) in parse_status(&status.stdout) {
        if files.len() >= MAX_FILES {
            truncated = true;
            break;
        }
        let known = counts.iter().find(|(path, ..)| path == &rel);
        let mut additions = known.map(|(_, add, ..)| *add).unwrap_or(0);
        let deletions = known.map(|(.., del, _)| *del).unwrap_or(0);
        let mut binary = known.map(|(.., bin)| *bin).unwrap_or(false);

        // 未跟踪文件不在 numstat 里：数行（二进制就不数）。
        if untracked {
            let absolute = Path::new(&root).join(&rel);
            let (lines, is_binary) = count_lines(&absolute);
            additions = lines as i64;
            binary = is_binary;
        }

        files.push(ChangeFile {
            path: Path::new(&root).join(&rel).to_string_lossy().to_string(),
            rel,
            status: status_name.to_string(),
            untracked,
            additions,
            deletions,
            binary,
        });
    }

    // 改动多的排前面：文件多时先看动得最狠的。
    files.sort_by(|a, b| {
        (b.additions + b.deletions)
            .cmp(&(a.additions + a.deletions))
            .then_with(|| a.rel.cmp(&b.rel))
    });

    let additions = files.iter().map(|file| file.additions).sum();
    let deletions = files.iter().map(|file| file.deletions).sum();
    Ok(ChangeReport {
        is_repo: true,
        root: Some(root.clone()),
        branch: branch_of(&root),
        files,
        additions,
        deletions,
        truncated,
    })
}

/// 数一个文件的行数 + 判二进制（读前 8 KiB 找 NUL）。
fn count_lines(path: &Path) -> (usize, bool) {
    let Ok(bytes) = fs::read(path) else {
        return (0, false);
    };
    let probe = &bytes[..bytes.len().min(8192)];
    if probe.contains(&0) {
        return (0, true);
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines = text.lines().take(MAX_COUNT_LINES).count();
    (lines, false)
}

// ---- 单文件 diff ----

pub fn diff(directory: &str, rel: &str) -> Result<FileDiff, String> {
    let directory = directory.trim();
    if rel.trim().is_empty() || rel.starts_with('/') || rel.split('/').any(|part| part == "..") {
        return Err("路径不合法".to_string());
    }
    let Some(root) = toplevel(directory) else {
        return Err("该目录不是 Git 仓库".to_string());
    };

    // 先看它是不是「新文件」：未跟踪 / 不在 HEAD 里。
    let tracked = git(&root, &["ls-files", "--error-unmatch", "--", rel])
        .map(|output| output.status.success())
        .unwrap_or(false);

    let absolute = Path::new(&root).join(rel);
    let mut binary = false;
    let (patch, truncated) = if !tracked {
        let Ok(bytes) = fs::read(&absolute) else {
            return Err(format!("读不到 {rel}"));
        };
        if bytes[..bytes.len().min(8192)].contains(&0) {
            binary = true;
            (String::new(), false)
        } else {
            let text = String::from_utf8_lossy(&bytes);
            let (patch, _) = new_file_patch(rel, &text);
            let (patch, truncated) = clamp_bytes(&patch, MAX_DIFF);
            (patch, truncated)
        }
    } else {
        let output = git(&root, &["diff", "HEAD", "--", rel])
            .map_err(|cause| format!("读取 diff 失败：{cause}"))?;
        let text = String::from_utf8_lossy(&output.stdout).to_string();
        let text = if text.trim().is_empty() {
            // 仓库还没有提交：退回工作树对暂存区。
            let fallback = git(&root, &["diff", "--", rel])
                .map_err(|cause| format!("读取 diff 失败：{cause}"))?;
            String::from_utf8_lossy(&fallback.stdout).to_string()
        } else {
            text
        };
        binary = text.contains("Binary files") || text.contains("GIT binary patch");
        let (patch, truncated) = clamp_bytes(&text, MAX_DIFF);
        (patch, truncated)
    };

    Ok(FileDiff {
        rel: rel.to_string(),
        patch,
        truncated,
        binary,
    })
}

/// 按字节上限截断，且不切坏 UTF-8。
fn clamp_bytes(text: &str, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text.to_string(), false);
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

// ---- 文件预览 ----

pub fn read_text(path: &str) -> Result<FileText, String> {
    let target = Path::new(path);
    let meta = fs::metadata(target).map_err(|cause| format!("读不到这个文件：{cause}"))?;
    if meta.is_dir() {
        return Err("这是一个目录".to_string());
    }
    let size = meta.len();
    let bytes = fs::read(target).map_err(|cause| format!("读不到这个文件：{cause}"))?;
    let probe = &bytes[..bytes.len().min(8192)];
    if probe.contains(&0) {
        return Ok(FileText {
            path: path.to_string(),
            text: String::new(),
            size,
            truncated: false,
            binary: true,
        });
    }
    let truncated = size > MAX_TEXT;
    let slice = &bytes[..bytes.len().min(MAX_TEXT as usize)];
    Ok(FileText {
        path: path.to_string(),
        text: String::from_utf8_lossy(slice).to_string(),
        size,
        truncated,
        binary: false,
    })
}

/// 保存一个**已存在**的文本文件（只能落在会话目录 / 仓库根之内，不新建文件）。
pub fn write_text(path: &str, text: &str, directory: &str) -> Result<u64, String> {
    if text.len() > 2 * 1024 * 1024 {
        return Err("文件超过 2 MB，不在应用内编辑".to_string());
    }
    let target = fs::canonicalize(path).map_err(|cause| format!("找不到这个文件：{cause}"))?;
    if !target.is_file() {
        return Err("这不是一个普通文件".to_string());
    }
    // 边界：只允许改会话目录（有仓库就按仓库根）里的文件，防止被拿去写别的路径。
    let base = match toplevel(directory) {
        Some(root) => fs::canonicalize(root).unwrap_or_else(|_| Path::new(directory).to_path_buf()),
        None => fs::canonicalize(directory).map_err(|cause| format!("目录不可用：{cause}"))?,
    };
    if !target.starts_with(&base) {
        return Err("只能编辑会话目录里的文件".to_string());
    }
    fs::write(&target, text).map_err(|cause| format!("写入失败：{cause}"))?;
    Ok(text.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_maps_untracked_and_modified() {
        let raw = b"?? src/new.rs\0 M src/a.rs\0M  src/b.rs\0";
        let parsed = parse_status(raw);
        assert_eq!(
            parsed,
            vec![
                ("src/new.rs".to_string(), "added", true),
                ("src/a.rs".to_string(), "modified", false),
                ("src/b.rs".to_string(), "modified", false),
            ]
        );
    }

    #[test]
    fn status_maps_deleted_and_renamed() {
        let raw = b"D  gone.rs\0R  new.rs\0old.rs\0";
        let parsed = parse_status(raw);
        assert_eq!(parsed[0], ("gone.rs".to_string(), "deleted", false));
        // 重命名的原路径不能当成第二条记录。
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].0, "new.rs");
        assert_eq!(parsed[1].1, "renamed");
    }

    #[test]
    fn numstat_parses_counts_and_binary() {
        let text = "12\t3\tsrc/a.rs\n-\t-\tlogo.png\n5\t0\tdir/{a => b}/c.ts\n";
        let parsed = parse_numstat(text);
        assert_eq!(parsed[0], ("src/a.rs".to_string(), 12, 3, false));
        assert_eq!(parsed[1], ("logo.png".to_string(), 0, 0, true));
        assert_eq!(parsed[2].0, "dir/b/c.ts".to_string());
    }

    #[test]
    fn new_file_patch_counts_lines() {
        let (patch, count) = new_file_patch("docs/a.md", "一\n二\n三");
        assert_eq!(count, 3);
        assert!(patch.contains("--- /dev/null"));
        assert!(patch.contains("@@ -0,0 +1,3 @@"));
        assert!(patch.contains("+三\n"));
        assert!(patch.contains("\\ No newline at end of file"));
    }

    #[test]
    fn clamp_never_breaks_utf8() {
        let text = "中文中文中文";
        let (clamped, truncated) = clamp_bytes(text, 7);
        assert!(truncated);
        assert_eq!(clamped, "中文");
    }

    /// 真起一个临时仓库跑一遍：改一个文件 + 新增一个文件，看报告与 diff 对不对。
    #[test]
    fn report_and_diff_on_real_temp_repo() {
        let dir = std::env::temp_dir().join(format!("prism-changes-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        let run = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .expect("git 跑不起来")
        };
        if !run(&["init", "-q"]).status.success() {
            return; // 机器上没有 git：跳过
        }
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "t"]);

        fs::write(dir.join("a.txt"), "one\ntwo\n").expect("write");
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);

        // 改一行 + 加一个未跟踪文件
        fs::write(dir.join("a.txt"), "one\nTWO\nthree\n").expect("write");
        fs::write(dir.join("b.txt"), "新文件\n第二行\n").expect("write");

        let report = report(dir.to_str().unwrap()).expect("report");
        assert!(report.is_repo);
        assert_eq!(report.files.len(), 2);
        let modified = report.files.iter().find(|f| f.rel == "a.txt").expect("a.txt");
        assert_eq!(modified.status, "modified");
        assert_eq!(modified.additions, 2);
        assert_eq!(modified.deletions, 1);
        let added = report.files.iter().find(|f| f.rel == "b.txt").expect("b.txt");
        assert_eq!(added.status, "added");
        assert!(added.untracked);
        assert_eq!(added.additions, 2);

        let patch = diff(dir.to_str().unwrap(), "b.txt").expect("diff");
        assert!(patch.patch.contains("new file mode"));
        assert!(patch.patch.contains("+新文件"));

        // 越界路径要被挡掉
        assert!(diff(dir.to_str().unwrap(), "../etc/passwd").is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_text_only_inside_directory() {
        let dir = std::env::temp_dir().join(format!("prism-write-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        let file = dir.join("a.txt");
        fs::write(&file, "old").expect("write");

        let written = write_text(file.to_str().unwrap(), "new\n", dir.to_str().unwrap()).expect("ok");
        assert_eq!(written, 4);
        assert_eq!(fs::read_to_string(&file).unwrap(), "new\n");

        // 目录之外的文件不许写
        let outside = std::env::temp_dir().join(format!("prism-outside-{}.txt", std::process::id()));
        fs::write(&outside, "keep").expect("write");
        let rejected = write_text(outside.to_str().unwrap(), "hack", dir.to_str().unwrap());
        assert!(rejected.is_err());
        assert_eq!(fs::read_to_string(&outside).unwrap(), "keep");

        // 不存在 / 是目录
        assert!(write_text(dir.join("none.txt").to_str().unwrap(), "x", dir.to_str().unwrap()).is_err());
        assert!(write_text(dir.to_str().unwrap(), "x", dir.to_str().unwrap()).is_err());

        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_file(&outside);
    }

    #[test]
    fn read_text_refuses_directory_and_flags_binary() {
        assert!(read_text("/tmp").is_err());

        let dir = std::env::temp_dir().join("prism-changes-read");
        let _ = fs::create_dir_all(&dir);
        let binary = dir.join("b.bin");
        fs::write(&binary, [0u8, 1, 2, 3]).expect("write");
        let data = read_text(binary.to_str().unwrap()).expect("read");
        assert!(data.binary);
        assert!(data.text.is_empty());

        let text = dir.join("a.txt");
        fs::write(&text, "hello\n").expect("write");
        let data = read_text(text.to_str().unwrap()).expect("read");
        assert_eq!(data.text, "hello\n");
        assert!(!data.binary);
    }
}
