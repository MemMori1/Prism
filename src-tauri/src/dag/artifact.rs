//! 制品仓库：DAG 节点产出物的托管与引用。
//!
//! 和 `crate::artifact` 不是一回事：那个做的是"时间线里点开本地 HTML/图片就地预览"，
//! 只读用户机器上的普通文件。这里是**自己管一份存储**——有 id、有归属、有血缘。
//!
//! 为什么必须有它
//! --------------
//! 节点之间原来靠 `{{nodes.x.output}}` 把上游正文整段拼进下游提示词。一张图两三层，
//! 每层几万字的报告叠起来，上下文必然炸掉（而且每拼一次都要付一次 token）。
//! 制品把"内容"和"引用"分开：正文只落一次盘，下游拿 id 或摘要，要看细节再取。
//!
//! 几条硬规则
//! ----------
//! - **先内容、后索引**：内容写失败就什么都不留；索引写失败留一个孤儿文件，
//!   读不到也删不掉——比"索引里有、文件没有"的坏引用温和。
//! - **同 DAG 可读，跨 DAG 拒绝**：`get(dag, id)` 先查 `ref.dag_id`；
//!   工具层的 dag 作用域是执行器注入的，模型没有"指定别的 DAG"的入参。
//! - **只写不改**：同名 key 再写一次是新 id（带 `parent_id` 的就是派生件），
//!   原始样本永远只读，hash 永存。
//! - **超限报错，不静默截断**：截断一个 300 MB 的样本只会让人以为分析过了。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 单件上限。样本动辄几十 MB，但再大就不是"产物"而是数据集了。
pub const MAX_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;
/// 单个 DAG 的总量上限：防的是一张图反复重试把磁盘写满。
pub const MAX_DAG_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// 摘要（进提示词的那份）的字符上限。
pub const SUMMARY_LIMIT: usize = 2048;
/// id 长度上限：它同时是文件名。
const ID_MAX: usize = 96;

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(TABLE[(byte >> 4) as usize] as char);
        out.push(TABLE[(byte & 0x0f) as usize] as char);
    }
    out
}

pub fn sha256_of(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex(&hasher.finalize())
}

/// 一个制品的元数据。内容在 `<root>/<dag_id>/<id>`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRef {
    pub id: String,
    pub dag_id: String,
    /// 谁产的。工具写入时是当前节点。
    pub node_id: String,
    /// 语义键：`report` / `strings` / `hashes` / `meta`。
    pub key: String,
    /// `text` / `json` / `binary` / `image`。
    pub kind: String,
    /// 原始文件名（有的话）。
    pub name: String,
    pub size: u64,
    pub sha256: String,
    /// 血缘：脱壳 / 派生产物指向原始样本。
    pub parent_id: Option<String>,
    pub created_at: i64,
}

/// 节点在工具层的 DAG 作用域。
///
/// 由原生执行器为每个节点注入；模型看不到也改不了 `dag_id`，
/// 所以"跨 DAG 读制品"在参数层面就不成立。
#[derive(Debug, Clone)]
pub struct DagScope {
    pub dag_id: String,
    pub node_id: String,
}

/// 一次写入。
pub struct PutRequest<'a> {
    pub dag_id: &'a str,
    pub node_id: &'a str,
    /// 空 → `report`。
    pub key: &'a str,
    /// 空 → 由 key 推（`<key>.json`）。
    pub name: &'a str,
    /// 空 → 按内容探测。
    pub kind: &'a str,
    pub parent_id: Option<&'a str>,
    pub bytes: &'a [u8],
}

/// 拼提示词时用的视图：只要摘要和元信息，不要正文。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtifactView {
    /// 空串 = 没有可用制品（写入失败 / 老计划）。
    pub id: String,
    pub key: String,
    pub kind: String,
    pub size: u64,
    pub sha256: String,
    pub summary: String,
    /// 摘要是被截过的（提示模型"还有全文"）。
    pub truncated: bool,
}

impl ArtifactView {
    pub fn meta_json(&self) -> String {
        serde_json::json!({
            "id": self.id,
            "key": self.key,
            "kind": self.kind,
            "size": self.size,
            "sha256": self.sha256,
        })
        .to_string()
    }
}

/// 取前 `limit` 个字符，返回（文本, 是否截断）。
pub fn head(text: &str, limit: usize) -> (String, bool) {
    if text.chars().count() <= limit {
        return (text.to_string(), false);
    }
    (text.chars().take(limit).collect(), true)
}

/// id 会直接当目录 / 文件名用：这条校验同时是路径穿越防线。
fn check_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.chars().count() > ID_MAX {
        return Err("制品 id 不合法".to_string());
    }
    if !id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(format!("id「{id}」不合法：只能用字母、数字、连字符与下划线"));
    }
    Ok(())
}

/// 新制品 id：`art-20261022-224501123-001`（毫秒 + 进程内序号）。
fn new_id() -> String {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed) % 1000;
    format!("art-{}-{seq:03}", Utc::now().format("%Y%m%d-%H%M%S%3f"))
}

/// 语义键的字符集：宽一点（允许 `.`），因为它不进文件名。
pub fn normalize_key(key: &str) -> String {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return "report".to_string();
    }
    trimmed
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
        .take(40)
        .collect()
}

/// 配额判定。抽成纯函数是为了能测——真造 256 MiB 的样本会把测试跑成磁盘炸弹。
fn check_quota(used: u64, incoming: u64) -> Result<(), String> {
    if incoming > MAX_ARTIFACT_BYTES {
        return Err(format!(
            "制品超过单件上限（{} MiB）",
            MAX_ARTIFACT_BYTES / (1024 * 1024)
        ));
    }
    if used.saturating_add(incoming) > MAX_DAG_BYTES {
        return Err(format!(
            "这个 DAG 的制品已满（{} GiB），先清理旧制品",
            MAX_DAG_BYTES / (1024 * 1024 * 1024)
        ));
    }
    Ok(())
}

/// 按内容探测类型。显式传入的 kind 优先，这个只在没传时兜底。
pub fn detect_kind(name: &str, bytes: &[u8]) -> &'static str {
    let ext = Path::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "bmp") {
        return "image";
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => {
            let first = text.trim_start().chars().next();
            if matches!(first, Some('{') | Some('[')) {
                "json"
            } else {
                "text"
            }
        }
        Err(_) => "binary",
    }
}

/// 索引读改写要串行：节点是并行跑的，`index.json` 是共享文件。
fn index_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 一个仓库根目录。测试用 [`ArtifactStore::at`] 指到临时目录。
#[derive(Debug, Clone)]
pub struct ArtifactStore {
    root: PathBuf,
}

impl ArtifactStore {
    pub fn at(root: impl Into<PathBuf>) -> ArtifactStore {
        ArtifactStore { root: root.into() }
    }

    /// `~/.prism/artifacts`。
    pub fn global() -> Result<ArtifactStore, String> {
        let root = crate::engines::opencode::search::prism_dir()
            .ok_or_else(|| "找不到 ~/.prism 目录".to_string())?
            .join("artifacts");
        Ok(ArtifactStore::at(root))
    }

    #[cfg(test)]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn dag_dir(&self, dag_id: &str) -> Result<PathBuf, String> {
        check_id(dag_id)?;
        Ok(self.root.join(dag_id))
    }

    fn index_path(dir: &Path) -> PathBuf {
        dir.join("index.json")
    }

    /// 宽松读取：坏索引当成空（`list` 用，不该因为一个坏文件让整页空掉）。
    fn load(&self, dag_id: &str) -> Vec<ArtifactRef> {
        let Ok(dir) = self.dag_dir(dag_id) else {
            return Vec::new();
        };
        let Ok(text) = std::fs::read_to_string(Self::index_path(&dir)) else {
            return Vec::new();
        };
        match serde_json::from_str::<Vec<ArtifactRef>>(&text) {
            Ok(list) => list,
            Err(error) => {
                eprintln!("[prism] 跳过制品索引 {dag_id}：{error}");
                Vec::new()
            }
        }
    }

    /// 严格读取：索引存在但解析不了就报错（`get` 用，不能返回半截数据）。
    fn load_strict(&self, dag_id: &str) -> Result<Vec<ArtifactRef>, String> {
        let dir = self.dag_dir(dag_id)?;
        let path = Self::index_path(&dir);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("读取制品索引失败：{error}")),
        };
        serde_json::from_str(&text).map_err(|error| format!("制品索引损坏：{error}"))
    }

    fn save_index(dir: &Path, list: &[ArtifactRef]) -> Result<(), String> {
        std::fs::create_dir_all(dir)
            .map_err(|error| format!("建制品目录失败：{error}"))?;
        let body = serde_json::to_string_pretty(list)
            .map_err(|error| format!("序列化制品索引失败：{error}"))?;
        let path = Self::index_path(dir);
        let temp = dir.join(".index.json.tmp");
        std::fs::write(&temp, body).map_err(|error| format!("写制品索引失败：{error}"))?;
        std::fs::rename(&temp, &path).map_err(|error| {
            let _ = std::fs::remove_file(&temp);
            format!("更新制品索引失败：{error}")
        })
    }

    fn path_of(&self, dag_id: &str, id: &str) -> Result<PathBuf, String> {
        check_id(id)?;
        Ok(self.dag_dir(dag_id)?.join(id))
    }

    /// 写一个制品。超配额直接报错，不写半截。
    pub fn put(&self, req: PutRequest<'_>) -> Result<ArtifactRef, String> {
        if req.bytes.is_empty() {
            return Err("制品内容为空，没有写入的必要".to_string());
        }
        let size = req.bytes.len() as u64;
        let key = normalize_key(req.key);
        let name = if req.name.trim().is_empty() {
            format!("{key}.json")
        } else {
            req.name.trim().to_string()
        };
        let kind = if req.kind.trim().is_empty() {
            detect_kind(&name, req.bytes).to_string()
        } else {
            req.kind.trim().to_string()
        };

        let guard = index_lock();
        let dir = self.dag_dir(req.dag_id)?;
        let mut list = self.load_strict(req.dag_id)?;
        let used: u64 = list.iter().map(|item| item.size).sum();
        check_quota(used, size)?;

        let id = new_id();
        std::fs::create_dir_all(&dir).map_err(|error| format!("建制品目录失败：{error}"))?;
        let target = dir.join(&id);
        let temp = dir.join(format!(".{id}.tmp"));
        std::fs::write(&temp, req.bytes).map_err(|error| {
            let _ = std::fs::remove_file(&temp);
            format!("写制品失败：{error}")
        })?;
        std::fs::rename(&temp, &target).map_err(|error| {
            let _ = std::fs::remove_file(&temp);
            format!("落盘制品失败：{error}")
        })?;

        let reference = ArtifactRef {
            id,
            dag_id: req.dag_id.to_string(),
            node_id: req.node_id.to_string(),
            key,
            kind,
            name,
            size,
            sha256: sha256_of(req.bytes),
            parent_id: req.parent_id.map(str::to_string),
            created_at: now_ms(),
        };
        list.push(reference.clone());
        // 索引写不进去就把内容删掉：留一个索引里没有的文件，谁也管不着它。
        if let Err(error) = Self::save_index(&dir, &list) {
            let _ = std::fs::remove_file(&target);
            return Err(error);
        }
        drop(guard);
        Ok(reference)
    }

    /// 便捷写法：把一段文本当制品存。
    pub fn put_text(
        &self,
        dag_id: &str,
        node_id: &str,
        key: &str,
        text: &str,
        parent_id: Option<&str>,
    ) -> Result<ArtifactRef, String> {
        self.put(PutRequest {
            dag_id,
            node_id,
            key,
            name: "",
            kind: "",
            parent_id,
            bytes: text.as_bytes(),
        })
    }

    /// 某个 DAG 的全部制品，新的在前。
    pub fn list(&self, dag_id: &str) -> Vec<ArtifactRef> {
        let mut list = self.load(dag_id);
        list.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        list
    }

    pub fn stat(&self, dag_id: &str, id: &str) -> Result<ArtifactRef, String> {
        self.load_strict(dag_id)?
            .into_iter()
            .find(|item| item.id == id)
            .ok_or_else(|| format!("没有这个制品：{id}"))
    }

    pub fn read(&self, dag_id: &str, id: &str) -> Result<(ArtifactRef, Vec<u8>), String> {
        let reference = self.stat(dag_id, id)?;
        let path = self.path_of(dag_id, id)?;
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("读取制品内容失败（{}）：{error}", reference.name))?;
        Ok((reference, bytes))
    }

    /// 读文本，超过 `max_chars` 截断并说明。
    pub fn read_text(
        &self,
        dag_id: &str,
        id: &str,
        max_chars: usize,
    ) -> Result<(ArtifactRef, String, bool), String> {
        let (reference, bytes) = self.read(dag_id, id)?;
        match String::from_utf8(bytes) {
            Ok(text) => {
                let (head, truncated) = head(&text, max_chars);
                Ok((reference, head, truncated))
            }
            Err(_) => Ok((
                reference,
                "（二进制制品，不能在对话里直接展开；用系统里的工具打开）".to_string(),
                false,
            )),
        }
    }

    /// 找某个节点产的某个 key 的制品（新的优先）。工具写入的 key 也走这条。
    pub fn find(&self, dag_id: &str, node_id: &str, key: &str) -> Option<ArtifactRef> {
        let key = normalize_key(key);
        let mut list = self.load(dag_id);
        list.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        list.into_iter()
            .find(|item| item.node_id == node_id && item.key == key)
    }

    /// 这个 DAG 已用多少字节。
    pub fn dag_bytes(&self, dag_id: &str) -> u64 {
        self.load(dag_id).iter().map(|item| item.size).sum()
    }

    /// 删一个制品。删计划**不会**调到这里：制品是证据，得单独确认。
    #[allow(dead_code)] // 清理入口留给 P1 的磁盘管理页，当前只有单测在用。
    pub fn delete(&self, dag_id: &str, id: &str) -> Result<(), String> {
        let _guard = index_lock();
        let dir = self.dag_dir(dag_id)?;
        let mut list = self.load_strict(dag_id)?;
        let before = list.len();
        list.retain(|item| item.id != id);
        if list.len() == before {
            return Err(format!("没有这个制品：{id}"));
        }
        let path = dir.join(id);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("删除制品内容失败：{error}")),
        }
        Self::save_index(&dir, &list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(tag: &str) -> ArtifactStore {
        let dir = std::env::temp_dir().join(format!("prism-art-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        ArtifactStore::at(dir)
    }

    fn put(store: &ArtifactStore, dag: &str, node: &str, key: &str, text: &str) -> ArtifactRef {
        store.put_text(dag, node, key, text, None).unwrap()
    }

    #[test]
    fn round_trips_content_and_metadata() {
        let store = store("roundtrip");
        let body = "第一行\n第二行";
        let reference = put(&store, "dag-1", "research", "report", body);

        assert_eq!(reference.key, "report");
        assert_eq!(reference.kind, "text");
        assert_eq!(reference.size, body.len() as u64);
        assert_eq!(reference.sha256, sha256_of(body.as_bytes()));
        assert!(reference.id.starts_with("art-"));

        let (loaded, bytes) = store.read("dag-1", &reference.id).unwrap();
        assert_eq!(loaded, reference);
        assert_eq!(String::from_utf8(bytes).unwrap(), body);
        assert_eq!(store.list("dag-1").len(), 1);
        assert_eq!(store.dag_bytes("dag-1"), body.len() as u64);
    }

    #[test]
    fn json_content_is_detected() {
        let store = store("json");
        let reference = put(&store, "dag-1", "n", "meta", r#"{"arch":"x86_64"}"#);
        assert_eq!(reference.kind, "json");
        assert_eq!(reference.name, "meta.json");
    }

    #[test]
    fn indexes_are_per_dag() {
        let store = store("scope");
        let a = put(&store, "dag-a", "n", "report", "甲的产物");
        put(&store, "dag-b", "n", "report", "乙的产物");

        assert_eq!(store.list("dag-a").len(), 1);
        assert_eq!(store.list("dag-b").len(), 1);
        // 拿甲的 id 去乙里取：不存在，而不是"读到了别人的内容"。
        assert!(store.stat("dag-b", &a.id).is_err());
        assert!(store.read("dag-b", &a.id).is_err());
    }

    #[test]
    fn blood_line_is_recorded() {
        let store = store("lineage");
        let sample = put(&store, "dag-1", "load", "report", "原始样本");
        let unpacked = store
            .put_text("dag-1", "unpack", "report", "脱壳后", Some(&sample.id))
            .unwrap();
        assert_eq!(unpacked.parent_id.as_deref(), Some(sample.id.as_str()));
        // 原始样本没被改：大小与哈希还是它自己的。
        assert_eq!(store.stat("dag-1", &sample.id).unwrap().size, sample.size);
    }

    #[test]
    fn find_picks_the_newest_of_a_key() {
        let store = store("find");
        put(&store, "dag-1", "research", "report", "旧的");
        let newest = put(&store, "dag-1", "research", "strings", "字符串清单");
        let found = store.find("dag-1", "research", "strings").unwrap();
        assert_eq!(found.id, newest.id);
        assert!(store.find("dag-1", "research", "nope").is_none());
    }

    #[test]
    fn quota_guards_both_dimensions() {
        assert!(check_quota(0, 1024).is_ok());
        let too_big = MAX_ARTIFACT_BYTES + 1;
        assert!(check_quota(0, too_big).unwrap_err().contains("单件上限"));
        assert!(check_quota(MAX_DAG_BYTES - 10, 1024)
            .unwrap_err()
            .contains("已满"));
    }

    #[test]
    fn rejects_ids_that_could_escape_the_directory() {
        let store = store("escape");
        assert!(store.put_text("../etc", "n", "report", "x", None).is_err());
        assert!(store.put_text("dag-1", "n", "report", "x", None).is_ok());
        assert!(store.stat("dag-1", "../../etc/passwd").is_err());
    }

    #[test]
    fn broken_index_is_reported_not_swallowed_by_get() {
        let store = store("broken");
        let reference = put(&store, "dag-1", "n", "report", "内容");
        let dir = store.root().join("dag-1");
        std::fs::write(dir.join("index.json"), "{ oops").unwrap();
        assert!(store.stat("dag-1", &reference.id).is_err());
        // list 走宽松路径：坏索引当空，不 panic。
        assert!(store.list("dag-1").is_empty());
    }

    #[test]
    fn empty_content_is_rejected() {
        let store = store("empty");
        assert!(store.put_text("dag-1", "n", "report", "", None).is_err());
    }

    #[test]
    fn deletes_content_and_entry() {
        let store = store("delete");
        let reference = put(&store, "dag-1", "n", "report", "内容");
        store.delete("dag-1", &reference.id).unwrap();
        assert!(store.list("dag-1").is_empty());
        assert!(!store.root().join("dag-1").join(&reference.id).exists());
        assert!(store.delete("dag-1", &reference.id).is_err());
    }

    #[test]
    fn text_reads_are_truncated_with_a_flag() {
        let store = store("truncate");
        let reference = put(&store, "dag-1", "n", "report", &"字".repeat(50));
        let (_, text, truncated) = store.read_text("dag-1", &reference.id, 10).unwrap();
        assert_eq!(text.chars().count(), 10);
        assert!(truncated);
    }

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256_of(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
