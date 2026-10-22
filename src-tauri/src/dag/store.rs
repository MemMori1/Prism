//! 计划的持久化（`~/.prism/dags/plans/<id>.json`）。
//!
//! 和 `routines` / `annotations` 一致：**原子写**（临时文件 + rename），读盘容错，
//! 坏文件跳过并留一行 stderr。一个手改坏的 JSON 不该让整张计划列表消失。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use chrono::Utc;

use super::model::{DagPlan, DagSummary};

/// 计划 id 的长度上限（它同时是文件名）。
const ID_MAX: usize = 64;

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// 计划目录（需要时创建）。
pub fn plans_dir() -> Option<PathBuf> {
    let dir = crate::engines::opencode::search::prism_dir()?.join("dags/plans");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// 新计划 id：`dag-20261022-213045123-001`（毫秒 + 进程内序号）。
///
/// 只靠毫秒不够：连点两次「新建」完全可能落在同一毫秒里，撞名就意味着
/// 第二张计划把第一张覆盖掉。序号只在进程内递增，够用而且 id 仍然可读。
pub fn new_id() -> String {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed) % 1000;
    format!(
        "dag-{}-{seq:03}",
        Utc::now().format("%Y%m%d-%H%M%S%3f")
    )
}

/// id 会直接当文件名用：这条校验同时是路径穿越防线。
fn check_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.chars().count() > ID_MAX {
        return Err("计划 id 不合法".to_string());
    }
    if !id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(format!("计划 id「{id}」不合法：只能用字母、数字、连字符与下划线"));
    }
    Ok(())
}

fn parse(path: &Path) -> Result<DagPlan, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("读取 {} 失败：{error}", path.display()))?;
    serde_json::from_str::<DagPlan>(&text)
        .map_err(|error| format!("解析 {} 失败：{error}", path.display()))
}

/// 全部计划摘要，最近改过的在前。
pub fn list_at(dir: &Path) -> Vec<DagSummary> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<DagSummary> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .filter_map(|path| match parse(&path) {
            Ok(plan) => Some(DagSummary::from(&plan)),
            Err(error) => {
                eprintln!("[prism] 跳过计划：{error}");
                None
            }
        })
        .collect();
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    out
}

pub fn list() -> Vec<DagSummary> {
    match plans_dir() {
        Some(dir) => list_at(&dir),
        None => Vec::new(),
    }
}

pub fn get_at(dir: &Path, id: &str) -> Result<DagPlan, String> {
    check_id(id)?;
    parse(&dir.join(format!("{id}.json")))
}

pub fn get(id: &str) -> Result<DagPlan, String> {
    let dir = plans_dir().ok_or_else(|| "找不到 ~/.prism 目录".to_string())?;
    get_at(&dir, id)
}

/// 保存（新建或覆盖）。`updated_at` 由这里盖，不信前端传的值。
pub fn save_at(dir: &Path, mut plan: DagPlan) -> Result<DagPlan, String> {
    check_id(&plan.id)?;
    let now = now_ms();
    if plan.created_at == 0 {
        plan.created_at = now;
    }
    plan.updated_at = now;
    plan.version = super::model::VERSION;
    // 节点删了，它的坐标也该跟着走——否则重新加同名节点会"跳"到旧位置。
    let ids: Vec<String> = plan.nodes.iter().map(|node| node.id.clone()).collect();
    plan.layout.retain(|id, _| ids.iter().any(|known| known == id));

    let path = dir.join(format!("{}.json", plan.id));
    let body = serde_json::to_string_pretty(&plan)
        .map_err(|error| format!("序列化计划失败：{error}"))?;
    let temp = dir.join(format!(".{}.json.tmp", plan.id));
    std::fs::write(&temp, body)
        .map_err(|error| format!("写入 {} 失败：{error}", temp.display()))?;
    std::fs::rename(&temp, &path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        format!("保存 {} 失败：{error}", path.display())
    })?;
    Ok(plan)
}

pub fn save(plan: DagPlan) -> Result<DagPlan, String> {
    let dir = plans_dir().ok_or_else(|| "找不到 ~/.prism 目录".to_string())?;
    save_at(&dir, plan)
}

pub fn delete_at(dir: &Path, id: &str) -> Result<(), String> {
    check_id(id)?;
    let path = dir.join(format!("{id}.json"));
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(format!("没有找到计划「{id}」"))
        }
        Err(error) => Err(format!("删除 {} 失败：{error}", path.display())),
    }
}

pub fn delete(id: &str) -> Result<(), String> {
    let dir = plans_dir().ok_or_else(|| "找不到 ~/.prism 目录".to_string())?;
    delete_at(&dir, id)
}

// ---------------------------------------------------------------- 运行记录

/// 运行 id 的上限：它由 `run-<计划 id>-<毫秒>-<序号>` 拼成，比计划 id 长。
const RUN_ID_MAX: usize = 160;

/// 运行 id 的校验。字符集与计划 id 相同，但长度上限更宽。
fn check_run_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.chars().count() > RUN_ID_MAX {
        return Err("运行 id 不合法".to_string());
    }
    if !id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(format!("运行 id「{id}」不合法"));
    }
    Ok(())
}

/// 运行记录目录（`~/.prism/dags/runs/`）。
fn runs_dir() -> Option<PathBuf> {
    super::run::runs_dir()
}

/// 写一份运行快照。**每次状态变化都写**：崩在中间也能看出跑到哪儿。
pub fn save_run_at(dir: &Path, run: &super::run::DagRun) -> Result<(), String> {
    check_run_id(&run.id)?;
    let path = dir.join(format!("{}.json", run.id));
    let body =
        serde_json::to_string_pretty(run).map_err(|error| format!("序列化运行记录失败：{error}"))?;
    let temp = dir.join(format!(".{}.json.tmp", run.id));
    std::fs::write(&temp, body)
        .map_err(|error| format!("写入 {} 失败：{error}", temp.display()))?;
    std::fs::rename(&temp, &path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        format!("保存 {} 失败：{error}", path.display())
    })
}

pub fn save_run(run: &super::run::DagRun) -> Result<(), String> {
    let dir = runs_dir().ok_or_else(|| "找不到 ~/.prism 目录".to_string())?;
    save_run_at(&dir, run)
}

pub fn load_run_at(dir: &Path, run_id: &str) -> Result<super::run::DagRun, String> {
    check_run_id(run_id)?;
    let path = dir.join(format!("{run_id}.json"));
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("读取 {} 失败：{error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("解析 {} 失败：{error}", path.display()))
}

pub fn load_run(run_id: &str) -> Result<super::run::DagRun, String> {
    let dir = runs_dir().ok_or_else(|| "找不到 ~/.prism 目录".to_string())?;
    load_run_at(&dir, run_id)
}

/// 某张计划最近一次运行（按开始时间取最大）。磁盘上最多几十份，直接扫。
pub fn latest_run_of_at(
    dir: &Path,
    plan_id: &str,
) -> Result<Option<super::run::DagRun>, String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(None);
    };
    let mut best: Option<super::run::DagRun> = None;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let Ok(run) = load_run_at(dir, stem) else {
            continue;
        };
        if run.plan_id != plan_id {
            continue;
        }
        if best
            .as_ref()
            .map(|current| run.started_at > current.started_at)
            .unwrap_or(true)
        {
            best = Some(run);
        }
    }
    Ok(best)
}

pub fn latest_run_of(plan_id: &str) -> Result<Option<super::run::DagRun>, String> {
    match runs_dir() {
        Some(dir) => latest_run_of_at(&dir, plan_id),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::model::{DagDefaults, DagNode, Position, Retry};
    use std::collections::HashMap;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prism-dag-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample(id: &str) -> DagPlan {
        DagPlan {
            version: 1,
            id: id.to_string(),
            name: "加自动更新".to_string(),
            goal: "在设置里加自动更新".to_string(),
            project: "/tmp/project".to_string(),
            defaults: DagDefaults::default(),
            nodes: vec![DagNode {
                id: "research".to_string(),
                title: "调研".to_string(),
                task: "查一下".to_string(),
                depends_on: Vec::new(),
                agent: None,
                workspace: None,
                tools: None,
                permission: None,
                retry: Retry::default(),
                timeout_secs: None,
                gate: None,
                position: None,
            }],
            layout: HashMap::new(),
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn round_trips_a_plan() {
        let dir = temp_dir("roundtrip");
        let saved = save_at(&dir, sample("dag-1")).unwrap();
        assert!(saved.created_at > 0);
        assert_eq!(saved.updated_at, saved.created_at);

        let loaded = get_at(&dir, "dag-1").unwrap();
        assert_eq!(loaded.name, "加自动更新");
        assert_eq!(loaded.nodes.len(), 1);
        assert_eq!(list_at(&dir).len(), 1);
    }

    #[test]
    fn layout_entries_of_deleted_nodes_are_dropped() {
        let dir = temp_dir("layout");
        let mut plan = sample("dag-2");
        plan.layout
            .insert("research".to_string(), Position { x: 10.0, y: 20.0 });
        plan.layout
            .insert("ghost".to_string(), Position { x: 0.0, y: 0.0 });
        let saved = save_at(&dir, plan).unwrap();
        assert!(saved.layout.contains_key("research"));
        assert!(!saved.layout.contains_key("ghost"));
    }

    #[test]
    fn rejects_ids_that_could_escape_the_directory() {
        assert!(check_id("../etc/passwd").is_err());
        assert!(check_id("a/b").is_err());
        assert!(check_id("").is_err());
        assert!(check_id("dag-20261022-213045123").is_ok());
    }

    #[test]
    fn broken_file_is_skipped_without_losing_the_rest() {
        let dir = temp_dir("broken");
        save_at(&dir, sample("dag-ok")).unwrap();
        std::fs::write(dir.join("dag-broken.json"), "{ oops").unwrap();
        let ids: Vec<String> = list_at(&dir).into_iter().map(|item| item.id).collect();
        assert_eq!(ids, vec!["dag-ok".to_string()]);
    }

    #[test]
    fn delete_reports_missing_plan() {
        let dir = temp_dir("delete");
        assert!(delete_at(&dir, "dag-nope").unwrap_err().contains("没有找到"));
        save_at(&dir, sample("dag-3")).unwrap();
        delete_at(&dir, "dag-3").unwrap();
        assert!(list_at(&dir).is_empty());
    }

    #[test]
    fn new_ids_do_not_collide_within_a_second() {
        let first = new_id();
        let second = new_id();
        assert_ne!(first, second);
        assert!(first.starts_with("dag-"));
    }
}
