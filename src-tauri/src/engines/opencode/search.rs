//! Prism 自有全文搜索索引（SQLite FTS5）。
//!
//! 数据主权：引擎数据一律只读；索引与元信息全部落在 `~/.prism/search.db`。
//! 中文检索用 FTS5 trigram 分词器（子串匹配）；过短的查询（任一 token < 3 字符）
//! 回退到 LIKE 扫描，保证「搜索」这类两字词也能命中。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};

use crate::engines::core::{SearchHit, SearchStats, SessionSummary};
use crate::engines::opencode::db::{self, IndexDoc};

/// 高亮包裹字符（控制字符）：前端拆段渲染，天然避免 HTML 注入。
pub const HL_START: char = '\u{1}';
pub const HL_END: char = '\u{2}';

/// 单次查询默认返回条数。
const DEFAULT_LIMIT: i64 = 60;
/// LIKE 回退的扫描行数上限（先粗筛，再按 limit 截断）。
const LIKE_SCAN_LIMIT: i64 = 400;
/// 片段显示长度（字符）。
const SNIPPET_CHARS: usize = 140;

/// Prism 自有数据目录（`~/.prism`，需要时创建）。
pub fn prism_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let dir = home.join(".prism");
    if std::fs::create_dir_all(&dir).is_err() {
        return None;
    }
    Some(dir)
}

/// 搜索索引数据库路径。
pub fn prism_search_db_path() -> Option<PathBuf> {
    prism_dir().map(|dir| dir.join("search.db"))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

// ---- 索引库 ----

fn open_search(path: &Path) -> Result<Connection, String> {
    let connection =
        Connection::open(path).map_err(|cause| format!("打开搜索索引失败：{cause}"))?;
    let _ = connection.busy_timeout(Duration::from_millis(2000));
    ensure_schema(&connection)?;
    Ok(connection)
}

/// 建表（幂等）。测试用内存库也能跑，用来验证 FTS5 / trigram 可用。
pub fn ensure_schema(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS docs (
                 rowid INTEGER PRIMARY KEY,
                 message_id TEXT NOT NULL,
                 session_id TEXT NOT NULL,
                 project_id TEXT NOT NULL,
                 role TEXT NOT NULL,
                 seq INTEGER NOT NULL,
                 time_created INTEGER NOT NULL,
                 text TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS docs_session ON docs(session_id);
             CREATE VIRTUAL TABLE IF NOT EXISTS docs_fts USING fts5(
                 text,
                 content='docs',
                 content_rowid='rowid',
                 tokenize='trigram'
             );
             CREATE TRIGGER IF NOT EXISTS docs_ai AFTER INSERT ON docs BEGIN
                 INSERT INTO docs_fts(rowid, text) VALUES (new.rowid, new.text);
             END;
             CREATE TRIGGER IF NOT EXISTS docs_ad AFTER DELETE ON docs BEGIN
                 INSERT INTO docs_fts(docs_fts, rowid, text)
                 VALUES ('delete', old.rowid, old.text);
             END;",
        )
        .map_err(|cause| format!("初始化搜索索引失败：{cause}"))
}

fn read_meta(connection: &Connection, key: &str) -> Option<String> {
    connection
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            params![key],
            |row| row.get::<_, String>(0),
        )
        .ok()
}

fn write_meta(connection: &Connection, key: &str, value: &str) -> Result<(), String> {
    connection
        .execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .map(|_| ())
        .map_err(|cause| format!("写入索引元信息失败：{cause}"))
}

fn read_watermark(connection: &Connection) -> i64 {
    read_meta(connection, "watermark_session_updated")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

/// 索引状态（只读索引库，不触碰引擎数据）。
pub fn stats(search_db: &Path) -> Result<SearchStats, String> {
    if !search_db.exists() {
        return Ok(SearchStats {
            indexed_sessions: 0,
            indexed_messages: 0,
            updated_at: 0,
        });
    }
    let connection = open_search(search_db)?;
    let indexed_messages: i64 = connection
        .query_row("SELECT COUNT(*) FROM docs", [], |row| row.get(0))
        .map_err(|cause| format!("统计索引失败：{cause}"))?;
    let indexed_sessions: i64 = connection
        .query_row("SELECT COUNT(DISTINCT session_id) FROM docs", [], |row| {
            row.get(0)
        })
        .map_err(|cause| format!("统计索引失败：{cause}"))?;
    let updated_at = read_meta(&connection, "updated_at")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    Ok(SearchStats {
        indexed_sessions,
        indexed_messages,
        updated_at,
    })
}

/// 增量更新索引：只重建「自上次以来有更新」的会话，并清掉引擎侧已消失的会话。
pub fn update_index(engine_db: &Path, search_db: &Path) -> Result<SearchStats, String> {
    let mut connection = open_search(search_db)?;
    let watermark = read_watermark(&connection);
    let touched = db::list_sessions_updated_since(engine_db, watermark)?;
    let mut max_updated = watermark;

    {
        let transaction = connection
            .transaction()
            .map_err(|cause| format!("开启索引事务失败：{cause}"))?;

        for (session_id, project_id, time_updated) in &touched {
            let docs: Vec<IndexDoc> = db::list_index_docs(engine_db, session_id)?;
            transaction
                .execute(
                    "DELETE FROM docs WHERE session_id = ?1",
                    params![session_id],
                )
                .map_err(|cause| format!("清理旧索引失败：{cause}"))?;
            for doc in &docs {
                transaction
                    .execute(
                        "INSERT INTO docs
                             (message_id, session_id, project_id, role, seq, time_created, text)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![
                            doc.message_id,
                            session_id,
                            project_id,
                            doc.role,
                            doc.seq,
                            doc.time_created,
                            doc.text
                        ],
                    )
                    .map_err(|cause| format!("写入索引失败：{cause}"))?;
            }
            if *time_updated > max_updated {
                max_updated = *time_updated;
            }
        }

        // 清理引擎侧已不存在的会话（归档 / 删除）。
        let alive: std::collections::HashSet<String> = db::list_all_sessions(engine_db)?
            .into_iter()
            .map(|session| session.id)
            .collect();
        let existing: Vec<String> = {
            let mut statement = transaction
                .prepare("SELECT DISTINCT session_id FROM docs")
                .map_err(|cause| format!("查询索引会话失败：{cause}"))?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|cause| format!("查询索引会话失败：{cause}"))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|cause| format!("读取索引会话失败：{cause}"))?
        };
        for session_id in existing {
            if !alive.contains(&session_id) {
                transaction
                    .execute("DELETE FROM docs WHERE session_id = ?1", params![session_id])
                    .map_err(|cause| format!("清理过期索引失败：{cause}"))?;
            }
        }

        transaction
            .commit()
            .map_err(|cause| format!("提交索引失败：{cause}"))?;
    }

    write_meta(
        &connection,
        "watermark_session_updated",
        &max_updated.to_string(),
    )?;
    write_meta(&connection, "updated_at", &now_ms().to_string())?;
    stats(search_db)
}

// ---- 查询 ----

/// 查询入口：消息全文（默认）或仅会话标题。
pub fn search(
    engine_db: &Path,
    search_db: &Path,
    term: &str,
    project_id: Option<&str>,
    since: Option<i64>,
    only_title: bool,
    limit: Option<i64>,
) -> Result<Vec<SearchHit>, String> {
    let term = term.trim();
    if term.is_empty() {
        return Ok(Vec::new());
    }
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, 200);
    let sessions = db::list_all_sessions(engine_db)?;

    if only_title {
        return Ok(search_titles(&sessions, term, project_id, since, limit));
    }
    if !search_db.exists() {
        return Ok(Vec::new());
    }

    let connection = open_search(search_db)?;
    let raw = if needs_like_fallback(term) {
        search_like(&connection, term, project_id, since, limit)?
    } else {
        search_fts(&connection, term, project_id, since, limit)?
    };
    Ok(attach_sessions(raw, &sessions, limit))
}

/// 任一 token 少于 3 个字符时 trigram 无法命中：回退 LIKE。
pub fn needs_like_fallback(term: &str) -> bool {
    term.split_whitespace().any(|token| token.chars().count() < 3)
}

/// 把用户输入转成 FTS5 查询：每个 token 作为一个短语，隐式 AND。
pub fn build_fts_query(term: &str) -> String {
    term.split_whitespace()
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// LIKE 模式里的通配符转义（配合 `ESCAPE '\'`）。
pub fn escape_like(term: &str) -> String {
    let mut out = String::with_capacity(term.len() + 4);
    for ch in term.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// 在长文本里截一段包含命中词的片段（首个命中用控制字符标记）。
pub fn make_snippet(text: &str, term: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let needle: Vec<char> = term.trim().chars().collect();
    let match_at = find_ci(&chars, &needle);

    let (start, end) = match match_at {
        Some(idx) => {
            let lead = max_chars / 3;
            let start = idx.saturating_sub(lead);
            let end = (idx + needle.len() + (max_chars - max_chars / 3)).min(chars.len());
            (start, end)
        }
        None => (0, max_chars.min(chars.len())),
    };

    let mut out: Vec<char> = Vec::with_capacity(end - start + 2);
    let head_ellipsis = start > 0;
    if head_ellipsis {
        out.push('…');
    }
    let window_start = out.len();
    out.extend_from_slice(&chars[start..end]);
    if end < chars.len() {
        out.push('…');
    }
    if let Some(idx) = match_at {
        let a = window_start + (idx - start);
        let b = a + needle.len();
        out.insert(b, HL_END);
        out.insert(a, HL_START);
    }
    out.into_iter().collect()
}

/// 大小写不敏感（ASCII）的子串查找，返回字符下标。
fn find_ci(haystack: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&index| {
        haystack[index..index + needle.len()]
            .iter()
            .zip(needle)
            .all(|(a, b)| a == b || a.eq_ignore_ascii_case(b))
    })
}

struct RawHit {
    message_id: String,
    session_id: String,
    project_id: String,
    role: String,
    seq: i64,
    time_created: i64,
    /// 展示片段（已含高亮标记）。
    snippet: String,
}

fn search_fts(
    connection: &Connection,
    term: &str,
    project_id: Option<&str>,
    since: Option<i64>,
    limit: i64,
) -> Result<Vec<RawHit>, String> {
    let mut statement = connection
        .prepare(
            "SELECT d.message_id, d.session_id, d.project_id, d.role, d.seq, d.time_created,
                    snippet(docs_fts, 0, char(1), char(2), '…', 20)
             FROM docs_fts
             JOIN docs d ON d.rowid = docs_fts.rowid
             WHERE docs_fts MATCH ?1
               AND (?2 IS NULL OR d.project_id = ?2)
               AND (?3 IS NULL OR d.time_created >= ?3)
             ORDER BY bm25(docs_fts)
             LIMIT ?4",
        )
        .map_err(|cause| format!("查询搜索索引失败：{cause}"))?;

    let rows = statement
        .query_map(params![build_fts_query(term), project_id, since, limit], |row| {
            Ok(RawHit {
                message_id: row.get(0)?,
                session_id: row.get(1)?,
                project_id: row.get(2)?,
                role: row.get(3)?,
                seq: row.get(4)?,
                time_created: row.get(5)?,
                snippet: row.get(6)?,
            })
        })
        .map_err(|cause| format!("查询搜索索引失败：{cause}"))?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|cause| format!("读取搜索结果失败：{cause}"))
}

fn search_like(
    connection: &Connection,
    term: &str,
    project_id: Option<&str>,
    since: Option<i64>,
    limit: i64,
) -> Result<Vec<RawHit>, String> {
    let pattern = format!("%{}%", escape_like(term));
    let mut statement = connection
        .prepare(
            "SELECT message_id, session_id, project_id, role, seq, time_created, text
             FROM docs
             WHERE text LIKE ?1 ESCAPE '\\'
               AND (?2 IS NULL OR project_id = ?2)
               AND (?3 IS NULL OR time_created >= ?3)
             ORDER BY time_created DESC
             LIMIT ?4",
        )
        .map_err(|cause| format!("查询搜索索引失败：{cause}"))?;

    let rows = statement
        .query_map(params![pattern, project_id, since, LIKE_SCAN_LIMIT], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
            ))
        })
        .map_err(|cause| format!("查询搜索索引失败：{cause}"))?;

    let mut hits: Vec<RawHit> = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|cause| format!("读取搜索结果失败：{cause}"))?
        .into_iter()
        .map(|(message_id, session_id, project_id, role, seq, time_created, text)| RawHit {
            message_id,
            session_id,
            project_id,
            role,
            seq,
            time_created,
            snippet: make_snippet(&text, term, SNIPPET_CHARS),
        })
        .collect();
    hits.truncate(limit.max(1) as usize);
    Ok(hits)
}

/// 把索引行补上会话标题，生成最终命中。
fn attach_sessions(
    raw: Vec<RawHit>,
    sessions: &[SessionSummary],
    limit: i64,
) -> Vec<SearchHit> {
    let index: HashMap<&str, &SessionSummary> =
        sessions.iter().map(|session| (session.id.as_str(), session)).collect();
    raw.into_iter()
        .take(limit.max(1) as usize)
        .map(|hit| {
            let session = index.get(hit.session_id.as_str());
            SearchHit {
                message_id: Some(hit.message_id),
                session_id: hit.session_id,
                session_title: session.and_then(|session| session.title.clone()),
                project_id: hit.project_id,
                role: Some(hit.role),
                seq: Some(hit.seq),
                time_created: hit.time_created,
                snippet: hit.snippet,
            }
        })
        .collect()
}

/// 「仅标题」模式：直接扫引擎侧的会话标题（数据量小，无需索引）。
fn search_titles(
    sessions: &[SessionSummary],
    term: &str,
    project_id: Option<&str>,
    since: Option<i64>,
    limit: i64,
) -> Vec<SearchHit> {
    let needle = term.to_lowercase();
    sessions
        .iter()
        .filter(|session| project_id.is_none_or(|id| session.project_id == id))
        .filter(|session| since.is_none_or(|ts| session.time_updated >= ts))
        .filter(|session| {
            session
                .title
                .as_deref()
                .map(|title| title.to_lowercase().contains(&needle))
                .unwrap_or(false)
        })
        .take(limit.max(1) as usize)
        .map(|session| SearchHit {
            message_id: None,
            session_id: session.id.clone(),
            session_title: session.title.clone(),
            project_id: session.project_id.clone(),
            role: None,
            seq: None,
            time_created: session.time_updated,
            snippet: make_snippet(session.title.as_deref().unwrap_or_default(), term, 200),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_supports_fts5_trigram() {
        let connection = Connection::open_in_memory().expect("内存库");
        ensure_schema(&connection).expect("FTS5 trigram 应可用");
        connection
            .execute(
                "INSERT INTO docs (message_id, session_id, project_id, role, seq, time_created, text)
                 VALUES ('m1', 's1', 'p1', 'user', 1, 1, '你好世界，这是一个测试')",
                [],
            )
            .expect("插入文档");
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM docs_fts WHERE docs_fts MATCH '\"是一个\"'", [], |row| {
                row.get(0)
            })
            .expect("MATCH 查询");
        assert_eq!(count, 1);
    }

    #[test]
    fn short_queries_use_like_fallback() {
        assert!(needs_like_fallback("搜索"));
        assert!(needs_like_fallback("ab"));
        assert!(needs_like_fallback("hello 世界"));
        assert!(!needs_like_fallback("搜索词"));
        assert!(!needs_like_fallback("abc"));
        assert!(!needs_like_fallback("hello 中国人"));
    }

    #[test]
    fn fts_query_quotes_and_escapes_tokens() {
        assert_eq!(build_fts_query("foo bar"), "\"foo\" \"bar\"");
        assert_eq!(build_fts_query("  a  b  "), "\"a\" \"b\"");
        assert_eq!(
            build_fts_query("he said \"hi\""),
            "\"he\" \"said\" \"\"\"hi\"\"\""
        );
    }

    #[test]
    fn escape_like_escapes_wildcards() {
        assert_eq!(escape_like("a%b_c\\d"), "a\\%b\\_c\\\\d");
        assert_eq!(escape_like("普通"), "普通");
    }

    #[test]
    fn snippet_marks_match_and_windows_long_text() {
        let text = format!("{}关键命中词{}", "前言".repeat(80), "后记".repeat(80));
        let snippet = make_snippet(&text, "关键命中词", 60);
        assert!(snippet.contains(HL_START));
        assert!(snippet.contains(HL_END));
        assert!(snippet.contains("关键命中词"));
        assert!(snippet.starts_with('…'));
        assert!(snippet.chars().count() < 80);
    }

    #[test]
    fn snippet_without_match_is_plain_prefix() {
        let snippet = make_snippet("短文本没有词", "缺", 60);
        assert_eq!(snippet, "短文本没有词");
    }

    #[test]
    fn snippet_match_at_start_has_no_lead_ellipsis() {
        let snippet = make_snippet("开头就命中后面的内容", "开头", 10);
        assert!(snippet.starts_with(HL_START));
    }

    /// 依赖本机真实 OpenCode 数据的索引冒烟：`cargo test -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn indexes_and_searches_real_database() {
        let Some(engine_db) = crate::engines::opencode::locator::database_path() else {
            panic!("未找到本机 opencode.db");
        };
        let dir = std::env::temp_dir().join(format!("prism-search-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let search_db = dir.join("search.db");

        let stats = update_index(&engine_db, &search_db).expect("update_index");
        assert!(stats.indexed_messages > 0, "应索引到消息");
        println!(
            "索引：{} 会话 / {} 条消息",
            stats.indexed_sessions, stats.indexed_messages
        );

        // 从索引里取一段真实文本，截中间几字做查询，保证命中确定性。
        let sample: String = {
            let connection = open_search(&search_db).expect("打开索引");
            connection
                .query_row(
                    "SELECT text FROM docs WHERE length(text) > 30 ORDER BY rowid LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .expect("取样本文本")
        };
        let chars: Vec<char> = sample.chars().collect();
        let mid = chars.len() / 2;
        let query: String = chars[mid..mid + 4].iter().collect();
        let hits = search(
            &engine_db,
            &search_db,
            query.trim(),
            None,
            None,
            false,
            Some(20),
        )
        .expect("FTS 查询");
        assert!(!hits.is_empty(), "FTS 应命中：{query:?}");
        println!("FTS 命中 {} 条：{:?}", hits.len(), hits[0].snippet);
        assert!(hits[0].snippet.contains(HL_START), "片段应带高亮标记");

        // 两字符查询走 LIKE 回退。
        let short: String = chars[mid..mid + 2].iter().collect();
        let short_hits = search(
            &engine_db,
            &search_db,
            short.trim(),
            None,
            None,
            false,
            Some(20),
        )
        .expect("LIKE 查询");
        assert!(!short_hits.is_empty(), "LIKE 应命中：{short:?}");
        println!("LIKE 命中 {} 条：{:?}", short_hits.len(), short_hits[0].snippet);

        // 标题模式：用第一个命中会话的标题词（若有两个字以上）。
        if let Some(title) = hits[0].session_title.as_deref() {
            let title_chars: Vec<char> = title.chars().collect();
            if title_chars.len() >= 2 {
                let probe: String = title_chars[..2].iter().collect();
                let title_hits = search(
                    &engine_db,
                    &search_db,
                    &probe,
                    None,
                    None,
                    true,
                    Some(20),
                )
                .expect("标题查询");
                assert!(
                    !title_hits.is_empty(),
                    "标题应命中：{probe:?} in {title:?}"
                );
                println!("标题命中 {} 条（{probe:?}）", title_hits.len());
            }
        }

        // 二次更新应为增量（不重复插入）。
        let again = update_index(&engine_db, &search_db).expect("第二次 update_index");
        assert_eq!(again.indexed_messages, stats.indexed_messages);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
