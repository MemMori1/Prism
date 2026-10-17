//! 会话文件账本：这个会话动过哪些文件、逐次操作与增删行数。
//!
//! 各引擎适配器把原始记录归一成 `RawEvent`（引擎差异封在各自模块里），
//! 这里负责聚合、排序与上限控制。账本只读引擎档案，不产生任何新数据。

use std::collections::HashMap;

use serde::Serialize;

/// 聚合前的单条文件操作（引擎适配器产出）。
#[derive(Debug, Clone, PartialEq)]
pub struct RawEvent {
    /// 引擎给的原始路径（绝对，或相对会话工作目录）。
    pub path: String,
    /// 时间线记录 id（点击跳回该轮）。
    pub record_id: String,
    /// 工具调用 id（可选；用于直接定位工具卡片）。
    pub call_id: Option<String>,
    /// 时间戳（毫秒；未知为 0）。
    pub time: i64,
    /// 归一类型：read / edit / write / delete。
    pub kind: String,
    /// 引擎原始工具名（展示用）。
    pub tool: String,
    pub added: i64,
    pub removed: i64,
}

/// 账本事件上限（超出只保留最近的，并标记 truncated）。
const MAX_EVENTS: usize = 5000;

/// 文件账本里的一次操作。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LedgerEvent {
    pub record_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    pub time: i64,
    pub kind: String,
    pub tool: String,
    pub added: i64,
    pub removed: i64,
}

/// 账本里的一个文件（聚合）。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LedgerFile {
    pub path: String,
    pub last_time: i64,
    /// 全部事件合计增 / 删行数。
    pub added: i64,
    pub removed: i64,
    /// 事件（时间正序）。
    pub events: Vec<LedgerEvent>,
}

/// 一个会话的文件账本（整体返回，前端展示用）。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionLedger {
    /// 按最近操作时间倒序。
    pub files: Vec<LedgerFile>,
    pub total_events: i64,
    /// 有改动（edit / write / delete）的文件数。
    pub changed_files: i64,
    /// 是否因超上限被截断。
    pub truncated: bool,
}

/// 聚合：按路径分组 → 组内时间正序 → 组间按最近操作倒序。
pub fn aggregate(mut events: Vec<RawEvent>) -> SessionLedger {
    let truncated = events.len() > MAX_EVENTS;
    if truncated {
        // 保留最近的事件（只为截断排序，落盘顺序无关）。
        events.sort_by(|a, b| b.time.cmp(&a.time));
        events.truncate(MAX_EVENTS);
    }

    let mut groups: HashMap<String, Vec<RawEvent>> = HashMap::new();
    for event in events {
        groups.entry(event.path.clone()).or_default().push(event);
    }

    let mut files: Vec<LedgerFile> = groups
        .into_iter()
        .map(|(path, mut events)| {
            events.sort_by(|a, b| a.time.cmp(&b.time));
            let last_time = events.last().map(|event| event.time).unwrap_or(0);
            let added = events.iter().map(|event| event.added).sum();
            let removed = events.iter().map(|event| event.removed).sum();
            LedgerFile {
                path,
                last_time,
                added,
                removed,
                events: events
                    .into_iter()
                    .map(|event| LedgerEvent {
                        record_id: event.record_id,
                        call_id: event.call_id,
                        time: event.time,
                        kind: event.kind,
                        tool: event.tool,
                        added: event.added,
                        removed: event.removed,
                    })
                    .collect(),
            }
        })
        .collect();

    files.sort_by(|a, b| b.last_time.cmp(&a.last_time).then_with(|| a.path.cmp(&b.path)));

    let total_events = files.iter().map(|file| file.events.len() as i64).sum();
    let changed_files = files
        .iter()
        .filter(|file| file.events.iter().any(|event| event.kind != "read"))
        .count() as i64;

    SessionLedger {
        files,
        total_events,
        changed_files,
        truncated,
    }
}

/// 文本行数（空串为 0）。
pub fn count_lines(text: &str) -> i64 {
    if text.is_empty() {
        0
    } else {
        text.lines().count() as i64
    }
}

/// 工具名归一：edit / write / delete / read，其余返回 None。
pub fn normalize_kind(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "edit" | "multiedit" | "notebookedit" | "apply_patch" | "patch" | "update" => Some("edit"),
        "write" | "create" | "add" => Some("write"),
        "delete" | "remove" => Some("delete"),
        "read" | "view" | "open" => Some("read"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(path: &str, time: i64, kind: &str, added: i64, removed: i64) -> RawEvent {
        RawEvent {
            path: path.to_string(),
            record_id: format!("rec-{time}"),
            call_id: None,
            time,
            kind: kind.to_string(),
            tool: kind.to_string(),
            added,
            removed,
        }
    }

    #[test]
    fn groups_by_path_with_counts_and_order() {
        let ledger = aggregate(vec![
            event("a.ts", 300, "edit", 5, 2),
            event("b.ts", 100, "read", 0, 0),
            event("a.ts", 200, "read", 0, 0),
            event("a.ts", 300, "edit", 1, 1),
        ]);

        assert_eq!(ledger.files.len(), 2);
        // 组间：最近操作倒序（a 的最后一次 = 300 > b = 100）。
        assert_eq!(ledger.files[0].path, "a.ts");
        assert_eq!(ledger.files[0].last_time, 300);
        assert_eq!(ledger.files[0].added, 6);
        assert_eq!(ledger.files[0].removed, 3);
        assert_eq!(ledger.files[0].events.len(), 3);
        // 组内时间正序。
        assert_eq!(ledger.files[0].events[0].time, 200);
        assert_eq!(ledger.files[1].path, "b.ts");
        assert_eq!(ledger.total_events, 4);
        assert_eq!(ledger.changed_files, 1, "只有 a.ts 有改动");
        assert!(!ledger.truncated);
    }

    #[test]
    fn caps_events_and_keeps_recent() {
        let mut events = Vec::new();
        for index in 0..(MAX_EVENTS + 10) {
            events.push(event("f.ts", index as i64, "edit", 1, 0));
        }
        let ledger = aggregate(events);
        assert!(ledger.truncated);
        assert_eq!(ledger.total_events, MAX_EVENTS as i64);
        assert_eq!(
            ledger.files[0].events.last().map(|event| event.time),
            Some((MAX_EVENTS + 9) as i64),
            "保留最近的事件"
        );
    }

    #[test]
    fn line_count_and_kind_normalization() {
        assert_eq!(count_lines(""), 0);
        assert_eq!(count_lines("a\nb\n"), 2);
        assert_eq!(count_lines("a"), 1);
        assert_eq!(normalize_kind("Edit"), Some("edit"));
        assert_eq!(normalize_kind("MultiEdit"), Some("edit"));
        assert_eq!(normalize_kind("Write"), Some("write"));
        assert_eq!(normalize_kind("Read"), Some("read"));
        assert_eq!(normalize_kind("Bash"), None);
    }
}
