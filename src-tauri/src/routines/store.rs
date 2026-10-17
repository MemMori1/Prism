//! 定时任务定义与执行日志的持久化（`~/.prism/routines.json`、`~/.prism/routine-logs/`）。
//!
//! 约定：定义文件是单一事实源（原子写：临时文件 + rename）；读盘容错，
//! 文件不存在视为空表；执行日志每次运行落一个 JSON（文件名 = 开始时刻）。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::cron;
use crate::engines::opencode::search::prism_dir;

/// 已接入运行轴的引擎（与前端 CREATABLE_ENGINES 对齐）。
pub const RUNTIME_ENGINES: [&str; 2] = ["opencode", "codex"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineDefinition {
    pub id: String,
    pub name: String,
    /// 五字段 cron。
    pub schedule: String,
    pub prompt: String,
    pub engine: String,
    pub enabled: bool,
    pub created_at: i64,
    /// 最近一次触发时刻（无论成败）。
    pub last_trigger: Option<i64>,
    /// succeeded / failed / interrupted / timeout / error
    pub last_status: Option<String>,
    pub last_session_id: Option<String>,
}

/// 列表视图：定义 + 派生字段（不落盘）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineView {
    #[serde(flatten)]
    pub definition: RoutineDefinition,
    pub next_run: Option<i64>,
    pub running: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineLogEntry {
    pub started_at: i64,
    pub finished_at: Option<i64>,
    /// succeeded / failed / interrupted / timeout / error
    pub status: String,
    pub session_id: Option<String>,
    pub engine: String,
    pub directory: String,
    pub error: Option<String>,
}

pub fn store_path() -> Option<PathBuf> {
    prism_dir().map(|dir| dir.join("routines.json"))
}

pub fn logs_dir() -> Option<PathBuf> {
    prism_dir().map(|dir| dir.join("routine-logs"))
}

/// 读取全部定义；文件不存在返回空表；解析失败带上下文报错。
pub fn load_all(path: &Path) -> Result<Vec<RoutineDefinition>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("读取 {} 失败：{error}", path.display())),
    };
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&text).map_err(|error| format!("解析 {} 失败：{error}", path.display()))
}

/// 原子写全部定义。
pub fn save_all(path: &Path, routines: &[RoutineDefinition]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建 {} 失败：{error}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(routines).map_err(|error| error.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|error| format!("写入 {} 失败：{error}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|error| format!("替换 {} 失败：{error}", path.display()))
}

/// 校验（新建 / 编辑共用）：名称与提示词非空、引擎在运行轴名单、cron 可解析。
pub fn validate(name: &str, schedule: &str, prompt: &str, engine: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("任务名称不能为空".to_string());
    }
    if prompt.trim().is_empty() {
        return Err("任务提示词不能为空".to_string());
    }
    if !RUNTIME_ENGINES.contains(&engine) {
        return Err(format!("该引擎（{engine}）的运行轴尚在开发，暂不能跑定时任务"));
    }
    cron::parse(schedule).map(|_| ())
}

/// 生成任务 id（时间戳 + 进程内序号，可读且不冲突）。
pub fn new_id(now_ms: i64) -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("rt-{now_ms:x}-{seq:x}")
}

/// 读取某任务的执行日志（新到旧，最多 limit 条；单条坏记录跳过）。
pub fn read_logs(dir: &Path, routine_id: &str, limit: usize) -> Vec<RoutineLogEntry> {
    let folder = dir.join(routine_id);
    let Ok(entries) = std::fs::read_dir(&folder) else {
        return Vec::new();
    };
    let mut logs: Vec<RoutineLogEntry> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().map(|ext| ext == "json").unwrap_or(false))
        .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .filter_map(|text| serde_json::from_str::<RoutineLogEntry>(&text).ok())
        .collect();
    logs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    logs.truncate(limit);
    logs
}

/// 写入一条执行日志（文件名 = 开始时刻）。
pub fn write_log(dir: &Path, routine_id: &str, entry: &RoutineLogEntry) -> Result<(), String> {
    let folder = dir.join(routine_id);
    std::fs::create_dir_all(&folder).map_err(|error| format!("创建 {} 失败：{error}", folder.display()))?;
    let path = folder.join(format!("{}.json", entry.started_at));
    let text = serde_json::to_string_pretty(entry).map_err(|error| error.to_string())?;
    std::fs::write(&path, text).map_err(|error| format!("写日志 {} 失败：{error}", path.display()))
}

/// 派生列表视图（下一步触发时刻 + 是否运行中）。
pub fn to_view(definition: &RoutineDefinition, now_ms: i64, running: bool) -> RoutineView {
    let next_run = cron::parse(&definition.schedule)
        .ok()
        .and_then(|schedule| schedule.next_after(now_ms));
    RoutineView {
        definition: definition.clone(),
        next_run,
        running,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prism-routines-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    fn sample() -> RoutineDefinition {
        RoutineDefinition {
            id: "rt-1".to_string(),
            name: "每日站会摘要".to_string(),
            schedule: "30 9 * * 1-5".to_string(),
            prompt: "汇总昨天的工作".to_string(),
            engine: "opencode".to_string(),
            enabled: true,
            created_at: 1_700_000_000_000,
            last_trigger: None,
            last_status: None,
            last_session_id: None,
        }
    }

    #[test]
    fn serde_round_trip_uses_camel_case() {
        let definition = sample();
        let text = serde_json::to_string(&definition).unwrap();
        assert!(text.contains("createdAt"), "应输出 camelCase：{text}");
        let parsed: RoutineDefinition = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed, definition);
    }

    #[test]
    fn missing_file_reads_as_empty() {
        let dir = temp_dir("missing");
        assert!(load_all(&dir.join("routines.json")).unwrap().is_empty());
    }

    #[test]
    fn save_then_load_round_trip() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("routines.json");
        let items = vec![sample()];
        save_all(&path, &items).expect("保存");
        assert_eq!(load_all(&path).unwrap(), items);
    }

    #[test]
    fn validate_rejects_bad_input() {
        assert!(validate("", "0 9 * * *", "干活", "opencode").is_err());
        assert!(validate("任务", "0 9 * * *", " ", "opencode").is_err());
        assert!(validate("任务", "0 9 * * *", "干活", "claude").is_err());
        assert!(validate("任务", "0 9 * *", "干活", "opencode").is_err());
        assert!(validate("任务", "0 9 * * *", "干活", "opencode").is_ok());
    }

    #[test]
    fn logs_round_trip_sorted_newest_first() {
        let dir = temp_dir("logs");
        let entry = |started_at: i64, status: &str| RoutineLogEntry {
            started_at,
            finished_at: Some(started_at + 1_000),
            status: status.to_string(),
            session_id: Some("ses_1".to_string()),
            engine: "opencode".to_string(),
            directory: "/tmp/ws".to_string(),
            error: None,
        };
        write_log(&dir, "rt-1", &entry(100, "failed")).unwrap();
        write_log(&dir, "rt-1", &entry(300, "succeeded")).unwrap();
        write_log(&dir, "rt-1", &entry(200, "timeout")).unwrap();
        let logs = read_logs(&dir, "rt-1", 10);
        assert_eq!(logs.len(), 3);
        assert_eq!(logs[0].status, "succeeded");
        assert_eq!(logs[2].status, "failed");
        assert_eq!(read_logs(&dir, "rt-1", 2).len(), 2);
        assert!(read_logs(&dir, "rt-none", 10).is_empty());
    }

    #[test]
    fn view_derives_next_run() {
        let definition = sample();
        let now = 1_700_000_000_000; // 2023-11-14 22:13 UTC（本地 11-15 早）
        let view = to_view(&definition, now, true);
        assert!(view.running);
        assert!(view.next_run.unwrap_or_default() > now);
    }
}
