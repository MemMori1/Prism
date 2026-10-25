//! Codex 会话状态库（`state_*.sqlite`）的只读读取。
//!
//! 只读原则：一律 READ_ONLY 打开；Prism 不改引擎任何字段。
//! 表的版本号会随 Codex 升级变化（state_4 → state_5 …），由 locator 探测。

use std::path::Path;

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row};

use super::locator::ENGINE_ID;
use super::rollout;
use crate::engines::core::{ProjectItem, SessionSummary};

type DbResult<T> = Result<T, String>;

/// Codex 会话 id 在 Prism 内的命名空间前缀（避免与 OpenCode ses_* 混淆）。
pub const SESSION_PREFIX: &str = "codex:";
/// Codex 项目 id 前缀（项目即 cwd）。
pub const PROJECT_PREFIX: &str = "codex:";

pub fn session_id_for(thread_id: &str) -> String {
    format!("{SESSION_PREFIX}{thread_id}")
}

pub fn is_codex_session(session_id: &str) -> bool {
    session_id.starts_with(SESSION_PREFIX)
}

pub fn thread_id_of(session_id: &str) -> Option<&str> {
    session_id.strip_prefix(SESSION_PREFIX)
}

pub fn is_codex_project(project_id: &str) -> bool {
    project_id.starts_with(PROJECT_PREFIX)
}

pub fn cwd_of(project_id: &str) -> Option<&str> {
    project_id.strip_prefix(PROJECT_PREFIX)
}

pub fn project_id_for(cwd: &str) -> String {
    format!("{PROJECT_PREFIX}{cwd}")
}

fn open_readonly(path: &Path) -> DbResult<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|cause| format!("打开 Codex 状态库失败：{cause}"))?;
    let _ = connection.busy_timeout(std::time::Duration::from_millis(2000));
    Ok(connection)
}

/// 标题规整：threads.title 可能是一整段多行文本（首个用户消息），
/// 只取首行并截断到 80 字符。
fn normalize_title(raw: &str) -> Option<String> {
    let first_line = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    if first_line.is_empty() {
        return None;
    }
    let chars: Vec<char> = first_line.chars().collect();
    if chars.len() <= 80 {
        return Some(first_line.to_string());
    }
    Some(format!("{}…", chars[..80].iter().collect::<String>()))
}

fn project_name(cwd: &str) -> String {
    Path::new(cwd)
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| cwd.to_string())
}

/// 项目列表：按 cwd 聚合未归档会话，按最近活动排序。
pub fn list_projects(db_path: &Path) -> DbResult<Vec<ProjectItem>> {
    let connection = open_readonly(db_path)?;
    let mut statement = connection
        .prepare(
            "SELECT cwd, COUNT(*), MAX(updated_at_ms)
             FROM threads
             WHERE archived = 0
             GROUP BY cwd
             ORDER BY MAX(updated_at_ms) DESC",
        )
        .map_err(|cause| format!("查询 Codex 项目失败：{cause}"))?;

    let rows = statement
        .query_map([], |row| {
            let cwd: String = row.get(0)?;
            Ok(ProjectItem {
                engine: ENGINE_ID.to_string(),
                id: project_id_for(&cwd),
                name: project_name(&cwd),
                worktree: cwd,
                session_count: row.get(1)?,
                time_updated: row.get(2)?,
            })
        })
        .map_err(|cause| format!("查询 Codex 项目失败：{cause}"))?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|cause| format!("读取 Codex 项目失败：{cause}"))
}

/// 某目录下的会话（未归档，按最近活动排序）。
pub fn list_sessions(db_path: &Path, cwd: &str, limit: i64) -> DbResult<Vec<SessionSummary>> {
    let connection = open_readonly(db_path)?;
    let mut statement = connection
        .prepare(
            "SELECT id, cwd, title, model, tokens_used, created_at_ms, updated_at_ms, rollout_path
             FROM threads
             WHERE cwd = ?1 AND archived = 0
             ORDER BY updated_at_ms DESC
             LIMIT ?2",
        )
        .map_err(|cause| format!("查询 Codex 会话失败：{cause}"))?;

    let rows = statement
        .query_map(params![cwd, limit], |row| map_thread_row(row))
        .map_err(|cause| format!("查询 Codex 会话失败：{cause}"))?;

    let mut sessions = Vec::new();
    for row in rows {
        let (mut summary, rollout_path) =
            row.map_err(|cause| format!("读取 Codex 会话失败：{cause}"))?;
        apply_token_split(&mut summary, &rollout_path);
        sessions.push(summary);
    }
    Ok(sessions)
}

/// 按线程 id 读取单个会话（工作台恢复列时用）。
pub fn get_session(db_path: &Path, thread_id: &str) -> DbResult<Option<SessionSummary>> {
    let connection = open_readonly(db_path)?;
    let row = connection
        .query_row(
            "SELECT id, cwd, title, model, tokens_used, created_at_ms, updated_at_ms, rollout_path
             FROM threads
             WHERE id = ?1",
            params![thread_id],
            |row| map_thread_row(row),
        )
        .optional()
        .map_err(|cause| format!("查询 Codex 会话失败：{cause}"))?;
    Ok(row.map(|(mut summary, rollout_path)| {
        apply_token_split(&mut summary, &rollout_path);
        summary
    }))
}

/// 会话的 rollout 文件路径（时间线 / 工具详情读取用）。
pub fn rollout_path_of(db_path: &Path, thread_id: &str) -> DbResult<Option<String>> {
    let connection = open_readonly(db_path)?;
    connection
        .query_row(
            "SELECT rollout_path FROM threads WHERE id = ?1",
            params![thread_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|cause| format!("查询 Codex 会话失败：{cause}"))
}

fn map_thread_row(row: &Row<'_>) -> rusqlite::Result<(SessionSummary, String)> {
    let thread_id: String = row.get(0)?;
    let cwd: String = row.get(1)?;
    let raw_title: String = row.get(2)?;
    let model: Option<String> = row.get(3)?;
    let tokens_used: i64 = row.get(4)?;
    let created_at_ms: Option<i64> = row.get(5)?;
    let updated_at_ms: Option<i64> = row.get(6)?;
    let rollout_path: String = row.get(7)?;
    let summary = SessionSummary {
        engine: ENGINE_ID.to_string(),
        id: session_id_for(&thread_id),
        project_id: project_id_for(&cwd),
        title: normalize_title(&raw_title),
        directory: cwd,
        model,
        cost: 0.0,
        // 兜底：threads 只有总量；真实四类拆分在 apply_token_split 里补。
        tokens_input: tokens_used,
        tokens_output: 0,
        tokens_cached: 0,
        tokens_reasoning: 0,
        time_created: created_at_ms.unwrap_or(0),
        time_updated: updated_at_ms.unwrap_or(0),
    };
    Ok((summary, rollout_path))
}

/// 从 rollout 尾部读最近一次 token_count，补齐四类 token。
fn apply_token_split(summary: &mut SessionSummary, rollout_path: &str) {
    if let Some((input, output, reasoning)) = rollout::tail_token_usage(Path::new(rollout_path)) {
        summary.tokens_input = input;
        summary.tokens_output = output;
        summary.tokens_reasoning = reasoning;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_multiline_title() {
        assert_eq!(
            normalize_title("import requests\nfrom bs4 import BeautifulSoup"),
            Some("import requests".to_string())
        );
        assert_eq!(normalize_title("\n\n  "), None);
        let long = "长".repeat(120);
        let normalized = normalize_title(&long).expect("应有标题");
        assert_eq!(normalized.chars().count(), 81); // 80 + …
    }

    #[test]
    fn codex_id_prefixes() {
        assert!(is_codex_session("codex:abc"));
        assert!(!is_codex_session("ses_123"));
        assert_eq!(thread_id_of("codex:abc"), Some("abc"));
        assert_eq!(cwd_of("codex:/Users/x"), Some("/Users/x"));
        assert!(is_codex_project("codex:/Users/x"));
    }

    /// 依赖本机真实 Codex 数据：`cargo test -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn reads_real_codex_data() {
        let Some(db_path) = crate::engines::codex::locator::state_db() else {
            panic!("未找到 Codex 状态库");
        };
        let projects = list_projects(&db_path).expect("list_projects");
        assert!(!projects.is_empty(), "应有 Codex 项目");
        println!("Codex 项目 {} 个", projects.len());

        let project = &projects[0];
        let cwd = cwd_of(&project.id).expect("项目 id 前缀");
        let sessions = list_sessions(&db_path, cwd, 50).expect("list_sessions");
        assert!(!sessions.is_empty(), "项目下应有会话");
        let newest = &sessions[0];
        println!(
            "最新会话：{}（{} 条？标题 {:?}，↓{} ↑{}）",
            newest.id, project.session_count, newest.title, newest.tokens_input, newest.tokens_output
        );

        let thread_id = thread_id_of(&newest.id).expect("会话 id 前缀");
        let rollout_path = rollout_path_of(&db_path, thread_id)
            .expect("rollout_path_of")
            .expect("应有 rollout 路径");
        assert!(Path::new(&rollout_path).is_file(), "rollout 文件应存在");

        let page = rollout::read_timeline(Path::new(&rollout_path), None, 150).expect("时间线");
        assert!(!page.records.is_empty(), "应有时间线记录");
        println!(
            "时间线记录 {} 条（has_more={}），首条角色 {}",
            page.records.len(),
            page.has_more,
            page.records[0].role
        );

        // 找第一个工具 part，验证工具详情能读出来。
        let tool = page
            .records
            .iter()
            .flat_map(|record| &record.parts)
            .find_map(|part| match part {
                crate::engines::core::TimelinePart::Tool { call_id, name, .. } => {
                    Some((call_id.clone(), name.clone()))
                }
                _ => None,
            });
        if let Some((call_id, name)) = tool {
            let detail = rollout::read_tool_detail(Path::new(&rollout_path), "", &call_id)
                .expect("read_tool_detail");
            println!(
                "工具详情：{} / {}（输出 {} 字符）",
                name,
                call_id,
                detail.output.as_deref().map(str::len).unwrap_or(0)
            );
        }
    }
}
