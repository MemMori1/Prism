//! 档案增补数据：星标 / 标签 / 自定义标题（`~/.prism/annotations.json`）。
//!
//! 原则：引擎数据只读，增补信息一律放 Prism 自己的存储；
//! 原子写（临时文件 + rename）；坏文件按空处理（不阻塞档案浏览）；
//! 会话增补清空后不保留空记录。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::engines::opencode::search::prism_dir;

/// 标签名的字符上限。
const TAG_NAME_LIMIT: usize = 24;
/// 自定义标题的字符上限。
const TITLE_LIMIT: usize = 120;

/// 单个会话的增补信息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionAnnotation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub starred: bool,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub updated_at: i64,
}

impl SessionAnnotation {
    /// 没有任何增补内容时视为空（不落盘）。
    fn is_empty(&self) -> bool {
        self.title.is_none() && !self.starred && self.tags.is_empty()
    }
}

/// 标签定义。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagDef {
    pub id: String,
    pub name: String,
    /// `#rrggbb`。
    pub color: String,
}

/// 全量存储（前端拿到后整体替换）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationStore {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub tags: Vec<TagDef>,
    #[serde(default)]
    pub sessions: BTreeMap<String, SessionAnnotation>,
}

fn default_version() -> u32 {
    1
}

impl Default for AnnotationStore {
    fn default() -> Self {
        AnnotationStore {
            version: 1,
            tags: Vec::new(),
            sessions: BTreeMap::new(),
        }
    }
}

/// 存储文件路径（`~/.prism/annotations.json`）。
pub fn store_path() -> Result<PathBuf, String> {
    let dir = prism_dir().ok_or_else(|| "无法定位 ~/.prism 目录".to_string())?;
    Ok(dir.join("annotations.json"))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
}

/// 读盘容错：文件不存在 / 解析失败一律返回空表。
pub fn load(path: &Path) -> AnnotationStore {
    let Ok(text) = std::fs::read_to_string(path) else {
        return AnnotationStore::default();
    };
    let mut store: AnnotationStore = serde_json::from_str(&text).unwrap_or_default();
    if store.version == 0 {
        store.version = 1;
    }
    store
}

/// 原子写（临时文件 + rename）。
pub fn save(path: &Path, store: &AnnotationStore) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建 {} 失败：{error}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(store).map_err(|error| error.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|error| format!("写入 {} 失败：{error}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|error| format!("替换 {} 失败：{error}", path.display()))
}

static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 读 → 改 → 原子写；进程内串行化，避免并发修改互相覆盖。
pub fn mutate(
    f: impl FnOnce(&mut AnnotationStore) -> Result<(), String>,
) -> Result<AnnotationStore, String> {
    let _guard = LOCK.lock().map_err(|_| "标注存储锁异常".to_string())?;
    let path = store_path()?;
    let mut store = load(&path);
    f(&mut store)?;
    store.version = 1;
    save(&path, &store)?;
    Ok(store)
}

/// 更新一个会话的增补信息（None = 不动；标题传空串 = 恢复引擎标题）。
pub fn update_session(
    store: &mut AnnotationStore,
    session_id: &str,
    title: Option<String>,
    starred: Option<bool>,
    tags: Option<Vec<String>>,
) -> Result<(), String> {
    if session_id.trim().is_empty() {
        return Err("会话 id 不能为空".to_string());
    }
    let entry = store.sessions.entry(session_id.to_string()).or_default();
    if let Some(value) = title {
        let trimmed = value.trim();
        if trimmed.chars().count() > TITLE_LIMIT {
            return Err(format!("标题最多 {TITLE_LIMIT} 个字符"));
        }
        entry.title = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    }
    if let Some(value) = starred {
        entry.starred = value;
    }
    if let Some(value) = tags {
        let known: std::collections::HashSet<String> =
            store.tags.iter().map(|tag| tag.id.clone()).collect();
        let mut deduped: Vec<String> = Vec::new();
        for id in value {
            if known.contains(&id) && !deduped.contains(&id) {
                deduped.push(id);
            }
        }
        entry.tags = deduped;
    }
    entry.updated_at = now_ms();
    if entry.is_empty() {
        store.sessions.remove(session_id);
    }
    Ok(())
}

fn normalize_color(color: &str) -> Result<String, String> {
    let value = color.trim().to_ascii_lowercase();
    let valid = value.len() == 7
        && value.starts_with('#')
        && value[1..].chars().all(|ch| ch.is_ascii_hexdigit());
    if valid {
        Ok(value)
    } else {
        Err("颜色格式应为 #rrggbb".to_string())
    }
}

fn validate_tag_name(store: &AnnotationStore, name: &str, except_id: Option<&str>) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("标签名不能为空".to_string());
    }
    if trimmed.chars().count() > TAG_NAME_LIMIT {
        return Err(format!("标签名最多 {TAG_NAME_LIMIT} 个字符"));
    }
    if store
        .tags
        .iter()
        .any(|tag| tag.name == trimmed && except_id.map_or(true, |id| tag.id != id))
    {
        return Err(format!("已有同名标签「{trimmed}」"));
    }
    Ok(trimmed.to_string())
}

fn new_tag_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("tg-{:x}-{:x}", now_ms(), seq)
}

/// 新建标签，返回新标签 id。
pub fn create_tag(store: &mut AnnotationStore, name: &str, color: &str) -> Result<String, String> {
    let name = validate_tag_name(store, name, None)?;
    let color = normalize_color(color)?;
    let id = new_tag_id();
    store.tags.push(TagDef {
        id: id.clone(),
        name,
        color,
    });
    Ok(id)
}

/// 重命名 / 换色（至少给一项）。
pub fn update_tag(
    store: &mut AnnotationStore,
    id: &str,
    name: Option<String>,
    color: Option<String>,
) -> Result<(), String> {
    let index = store
        .tags
        .iter()
        .position(|tag| tag.id == id)
        .ok_or_else(|| "标签不存在".to_string())?;
    let next_name = match name {
        Some(value) => Some(validate_tag_name(store, &value, Some(id))?),
        None => None,
    };
    let next_color = match color {
        Some(value) => Some(normalize_color(&value)?),
        None => None,
    };
    let tag = &mut store.tags[index];
    if let Some(value) = next_name {
        tag.name = value;
    }
    if let Some(value) = next_color {
        tag.color = value;
    }
    Ok(())
}

/// 删除标签，并从所有会话上摘掉引用（仅剩该标签的会话记录一并清掉）。
pub fn delete_tag(store: &mut AnnotationStore, id: &str) -> Result<(), String> {
    let before = store.tags.len();
    store.tags.retain(|tag| tag.id != id);
    if store.tags.len() == before {
        return Err("标签不存在".to_string());
    }
    for entry in store.sessions.values_mut() {
        entry.tags.retain(|tag_id| tag_id != id);
    }
    store.sessions.retain(|_, entry| !entry.is_empty());
    Ok(())
}

/// 合并标签：把来源标签的引用移到目标标签（去重）并删除来源。
pub fn merge_tag(store: &mut AnnotationStore, source_id: &str, target_id: &str) -> Result<(), String> {
    if source_id == target_id {
        return Ok(());
    }
    if !store.tags.iter().any(|tag| tag.id == source_id) {
        return Err("要合并的标签不存在".to_string());
    }
    if !store.tags.iter().any(|tag| tag.id == target_id) {
        return Err("目标标签不存在".to_string());
    }
    store.tags.retain(|tag| tag.id != source_id);
    for entry in store.sessions.values_mut() {
        if entry.tags.iter().any(|id| id == source_id) {
            entry.tags.retain(|id| id != source_id);
            if !entry.tags.iter().any(|id| id == target_id) {
                entry.tags.push(target_id.to_string());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("prism-annotations-{tag}-{}.json", std::process::id()))
    }

    #[test]
    fn round_trip_save_and_load() {
        let path = temp_path("round");
        let mut store = AnnotationStore::default();
        update_session(
            &mut store,
            "opencode:ses_1",
            Some("自定义标题".to_string()),
            Some(true),
            None,
        )
        .expect("更新");
        let id = create_tag(&mut store, "工作", "#6D5AE6").expect("建标签");
        update_session(&mut store, "opencode:ses_1", None, None, Some(vec![id.clone()]))
            .expect("挂标签");
        save(&path, &store).expect("保存");

        let loaded = load(&path);
        let entry = &loaded.sessions["opencode:ses_1"];
        assert_eq!(entry.title.as_deref(), Some("自定义标题"));
        assert!(entry.starred);
        assert_eq!(entry.tags, vec![id]);
        assert_eq!(loaded.tags.len(), 1);
        assert_eq!(loaded.tags[0].name, "工作");
        assert_eq!(loaded.tags[0].color, "#6d5ae6", "颜色归一为小写");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn clearing_title_removes_empty_entry() {
        let mut store = AnnotationStore::default();
        update_session(&mut store, "codex:t1", Some("示例".to_string()), None, None).expect("更新");
        assert!(store.sessions.contains_key("codex:t1"));
        update_session(&mut store, "codex:t1", Some("   ".to_string()), None, None).expect("清标题");
        assert!(!store.sessions.contains_key("codex:t1"), "空记录不保留");
    }

    #[test]
    fn tag_name_and_color_validation() {
        let mut store = AnnotationStore::default();
        assert!(create_tag(&mut store, "  ", "#6d5ae6").is_err(), "空名拒绝");
        assert!(create_tag(&mut store, "工作", "red").is_err(), "颜色格式拒绝");
        create_tag(&mut store, "工作", "#6d5ae6").expect("建标签");
        assert!(create_tag(&mut store, "工作 ", "#111111").is_err(), "同名（trim 后）拒绝");
    }

    #[test]
    fn delete_tag_strips_sessions_and_keeps_other_fields() {
        let mut store = AnnotationStore::default();
        let id = create_tag(&mut store, "临时", "#111111").expect("建标签");
        update_session(&mut store, "u1", Some("标题".to_string()), None, Some(vec![id.clone()]))
            .expect("挂");
        update_session(&mut store, "u2", None, None, Some(vec![id.clone()])).expect("挂");
        delete_tag(&mut store, &id).expect("删");
        assert!(store.tags.is_empty());
        assert_eq!(store.sessions["u1"].title.as_deref(), Some("标题"));
        assert!(!store.sessions.contains_key("u2"), "仅剩标签的会话记录被清掉");
    }

    #[test]
    fn merge_moves_sessions_and_dedupes() {
        let mut store = AnnotationStore::default();
        let a = create_tag(&mut store, "A", "#111111").expect("A");
        let b = create_tag(&mut store, "B", "#222222").expect("B");
        update_session(&mut store, "s1", None, None, Some(vec![a.clone(), b.clone()])).expect("s1");
        update_session(&mut store, "s2", None, None, Some(vec![a.clone()])).expect("s2");
        merge_tag(&mut store, &a, &b).expect("合并");
        assert_eq!(store.tags.len(), 1);
        assert_eq!(store.sessions["s1"].tags, vec![b.clone()]);
        assert_eq!(store.sessions["s2"].tags, vec![b]);
    }

    #[test]
    fn unknown_tag_ids_are_dropped() {
        let mut store = AnnotationStore::default();
        update_session(
            &mut store,
            "s1",
            None,
            Some(true),
            Some(vec!["tg-not-exist".to_string()]),
        )
        .expect("更新");
        assert!(store.sessions["s1"].tags.is_empty());
        assert!(store.sessions["s1"].starred);
    }
}
