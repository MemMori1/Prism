//! 外部后端（upx / radare2 / Ghidra / .NET 工具链）的注册表。
//!
//! 三件事，都只写一份：
//!
//! 1. **状态缓存**：探测很贵（起进程读版本），但结果只在进程内有效。
//!    第一次问才探，之后读缓存；`toolchain_probe` 显式重探。
//! 2. **可见性**：工具声明里写 `needs: &[Need::Backend("upx")]`，
//!    后端没接上就**不进 schema**——模型看不到，就不会编一个调用出来。
//! 3. **作业目录**：外部程序的输入输出一律落在 `~/.prism/jobs/<id>/`，
//!    样本目录一个字节都不写。
//!
//! 为什么状态不落盘：机器上换了 Ghidra 版本，Prism 不能还按老版本解析。
//! 只有"用户手动指的那个路径"才值得记（`probe::overrides`）。

pub mod cfg;
pub mod exec;
pub mod ghidra;
pub mod hash_identify;
pub mod index;
pub mod ir;
pub mod jadx;
pub mod probe;
pub mod radare2;
pub mod text;
pub mod upx;
pub mod vm;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use sha2::{Digest, Sha256};

use super::tools::{Need, ToolContext, ToolDef};
use crate::dag::artifact::ArtifactStore;
use crate::engines::native::forensics::target_of;
use probe::{BackendState, BackendStatus};

/// 同时保留多少个作业目录（多出来的按创建时间淘汰）。
///
/// 正常路径上 [`JobDir`] 会在 drop 时自己删干净，这个上限只兜底
/// "进程被杀 / 早退忘了收"留下的残骸。
const KEEP_JOBS: usize = 20;

static CACHE: OnceLock<RwLock<HashMap<&'static str, BackendStatus>>> = OnceLock::new();
static JOB_SEQ: AtomicU64 = AtomicU64::new(0);

fn cache() -> &'static RwLock<HashMap<&'static str, BackendStatus>> {
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// 锁中毒不该让工具链整块瘫掉：状态缓存丢一次最多是重探一遍。
fn read_cache() -> std::sync::RwLockReadGuard<'static, HashMap<&'static str, BackendStatus>> {
    cache().read().unwrap_or_else(|error| error.into_inner())
}

fn write_cache() -> std::sync::RwLockWriteGuard<'static, HashMap<&'static str, BackendStatus>> {
    cache().write().unwrap_or_else(|error| error.into_inner())
}

/// 查一个后端的当前状态；没探过就**当场探一次**再返回。
///
/// 探测不持锁：起进程读版本可能几百毫秒，持着写锁会让并发的
/// `tools.list` 一起卡住。
pub fn status(id: &str) -> Option<BackendStatus> {
    probe::find(id)?;
    let cached = read_cache()
        .get(id)
        .filter(|status| status.state != BackendState::Unknown)
        .cloned();
    match cached {
        Some(status) => Some(status),
        None => Some(refresh(id)),
    }
}

/// 重探一个后端并写回缓存。
pub fn refresh(id: &str) -> BackendStatus {
    let fresh = probe::probe(id);
    write_cache().insert(fresh.id, fresh.clone());
    fresh
}

/// 重探全部已登记的后端（设置页「重新检测」）。
pub fn refresh_all() -> Vec<BackendStatus> {
    probe::BACKENDS.iter().map(|def| refresh(def.id)).collect()
}

/// 全部后端的状态（没探过的会被探一次）。
pub fn all() -> Vec<BackendStatus> {
    probe::BACKENDS
        .iter()
        .filter_map(|def| status(def.id))
        .collect()
}

/// 这条需求此刻满足了吗？不满足就给人话原因。
///
/// 文案刻意和 §11 的错误表一致：模型读到的原因与设置页看到的是同一句。
pub fn unmet(need: &Need) -> Option<String> {
    match need {
        Need::Backend(id) => {
            // 未登记的 id 按"不满足"处理：拼错一个后端名不该让工具永远可见。
            let Some(status) = status(id) else {
                return Some(format!("需要未登记的后端：{id}"));
            };
            match &status.state {
                BackendState::Ready => None,
                BackendState::Unknown => None,
                BackendState::Missing => Some(format!("需要 {}（未检测到）", status.name)),
                BackendState::TooOld { found, need } => Some(format!(
                    "需要 {} {need} 以上（本机检测到 {found}）",
                    status.name
                )),
                BackendState::Broken(why) => {
                    Some(format!("需要 {}（找到了但跑不起来：{why}）", status.name))
                }
            }
        }
    }
}

/// 一组需求是否全部满足；`Err` 是第一条不满足的原因。
pub fn needs_met(needs: &[Need]) -> Result<(), String> {
    for need in needs {
        if let Some(reason) = unmet(need) {
            return Err(reason);
        }
    }
    Ok(())
}

/// 取一个**可用**的后端；不可用就按 §11 的格式报错。
///
/// 工具执行前调用：即便模型从历史里翻出旧调用硬发过来，也要在这里挡住。
pub fn require(id: &str) -> Result<BackendStatus, String> {
    let status = status(id).ok_or_else(|| format!("没有登记过的后端：{id}"))?;
    match &status.state {
        BackendState::Ready => Ok(status),
        BackendState::Missing => Err(format!(
            "{} 没接上：没找到可执行文件。{}，装好后在 设置 → 工具链 里指一下路径。",
            status.name, status.install
        )),
        BackendState::TooOld { found, need } => Err(format!(
            "{} 版本太低：检测到 {found}，至少要 {need}。升级：{}",
            status.name, status.install
        )),
        BackendState::Broken(why) => Err(format!(
            "找到 {}，但跑不起来（{why}）。装好依赖后点「重新检测」。",
            status.name
        )),
        BackendState::Unknown => {
            Err(format!("{} 的状态还没探到，点「重新检测」再试。", status.name))
        }
    }
}

/// 后端层登记的工具（由 `tools::builtin_tools` 追加）。
pub fn tools() -> Vec<Arc<ToolDef>> {
    let mut list = Vec::new();
    list.extend(upx::tools());
    list.extend(radare2::tools());
    list.extend(ghidra::tools());
    list.extend(cfg::tools());
    list.extend(hash_identify::tools());
    list.extend(vm::tools());
    list.extend(text::tools());
    list.extend(jadx::tools());
    list
}

// ---------------------------------------------------------------- 样本准备

/// 一个已经准备成"外部程序能直接读"的样本。
#[derive(Debug, Clone)]
pub struct StagedSample {
    /// 交给外部程序的真实路径（制品输入时是作业目录里的副本）。
    pub path: PathBuf,
    /// 用户给的那个路径（制品输入时为空）。
    pub origin_path: Option<PathBuf>,
    /// 原始文件名。
    pub name: String,
    pub size: u64,
    pub sha256: String,
    /// 输入来自哪个制品（`artifact:<id>`）。
    pub artifact: Option<String>,
}

/// 把输入准备成外部程序能读的一个文件，并算出整份 sha256。
///
/// 两条硬规矩，和 `bin.unpack.upx` 一致：
///
/// - **`artifact:<id>` 必须先落到作业目录**：制品库按 id 寻址，不是按路径；
/// - **符号链接按真实路径校验**：词法归一挡不住 `ln -s /etc/passwd x`。
///
/// `max_bytes` 只限制"制品复制"这一路：路径输入不复制，外部程序自己流式读。
pub fn stage_sample(
    ctx: &ToolContext<'_>,
    raw: &str,
    job: &JobDir,
    max_bytes: u64,
) -> Result<StagedSample, String> {
    if let Some(id) = raw.strip_prefix("artifact:") {
        let scope = ctx
            .dag
            .ok_or_else(|| "读制品需要 DAG 作用域；在聊天里请改用文件路径".to_string())?;
        let store = ArtifactStore::global()?;
        let reference = store.stat(&scope.dag_id, id.trim())?;
        if reference.size > max_bytes {
            return Err(format!(
                "制品 {} 有 {:.1} MiB，超过 {} MiB 上限",
                reference.id,
                reference.size as f64 / (1024.0 * 1024.0),
                max_bytes / (1024 * 1024)
            ));
        }
        let (_, bytes) = store.read(&scope.dag_id, &reference.id)?;
        let path = job.input().join("sample.bin");
        std::fs::write(&path, &bytes).map_err(|error| format!("写临时样本失败：{error}"))?;
        return Ok(StagedSample {
            path,
            origin_path: None,
            name: reference.name.clone(),
            size: bytes.len() as u64,
            sha256: reference.sha256.clone(),
            artifact: Some(reference.id),
        });
    }

    let (path, size) = target_of(ctx, &serde_json::json!({ "path": raw }))
        .map_err(|failure| failure.outcome.text)?;
    let real = std::fs::canonicalize(&path).map_err(|error| format!("读不到 {raw}：{error}"))?;
    let root = std::fs::canonicalize(ctx.directory)
        .unwrap_or_else(|_| PathBuf::from(ctx.directory));
    if !real.starts_with(&root) {
        return Err(format!(
            "路径越出工作目录：{raw}（真实路径 {}）",
            real.display()
        ));
    }
    let sha256 = sha256_file(&real)?;
    let name = real
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "sample.bin".to_string());
    Ok(StagedSample {
        // 交给外部程序的是真实路径：校验完到真正打开之间，符号链接可能被换掉。
        path: real,
        origin_path: Some(path),
        name,
        size,
        sha256,
        artifact: None,
    })
}

/// IR 里的样本描述（radare2 / Ghidra 共用）。
pub fn sample_ref(sample: &StagedSample, meta: &serde_json::Value) -> index::SampleRef {
    index::SampleRef {
        sha256: sample.sha256.clone(),
        size: sample.size,
        format: meta["format"].as_str().unwrap_or("unknown").to_string(),
        arch: meta["arch"].as_str().map(str::to_string),
        bits: meta["bits"].as_u64().map(|value| value as u32),
        image_base: None,
    }
}

/// 流式算整份文件的 sha256：缓存键要按整份比，不能只读文件头。
fn sha256_file(path: &Path) -> Result<String, String> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)
        .map_err(|error| format!("读不到 {}：{error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("读 {} 失败：{error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    Ok(out)
}

// ---------------------------------------------------------------- 作业目录

/// 一次外部调用的临时目录，drop 时整棵删掉。
///
/// 不直接写 `std::env::temp_dir()`：macOS 的 `/var/folders/...` 会被
/// 安全软件当成可疑落点，而且用户想复现时找不到。`~/.prism/jobs` 是
/// Prism 自己的地盘，权限与制品库一致。
pub struct JobDir {
    path: PathBuf,
}

impl JobDir {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 外部程序的输出目录（`out/`）。
    pub fn out(&self) -> PathBuf {
        self.path.join("out")
    }

    /// 放进去给外部程序读的东西（避免程序往样本目录写临时文件）。
    pub fn input(&self) -> PathBuf {
        self.path.join("in")
    }
}

impl Drop for JobDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// 建一个作业目录（`~/.prism/jobs/<时间>-<标签>-<序号>/`）。
pub fn job_dir(label: &str) -> Result<JobDir, String> {
    let root = crate::engines::opencode::search::prism_dir()
        .ok_or_else(|| "找不到 ~/.prism 目录".to_string())?
        .join("jobs");
    job_dir_at(&root, label)
}

/// 同 [`job_dir`]，但根目录可指定（单测用）。
pub fn job_dir_at(root: &Path, label: &str) -> Result<JobDir, String> {
    let safe: String = label
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .take(24)
        .collect();
    let seq = JOB_SEQ.fetch_add(1, Ordering::Relaxed);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_millis())
        .unwrap_or(0);
    let dir = root.join(format!("{stamp}-{safe}-{seq}"));
    std::fs::create_dir_all(dir.join("out"))
        .map_err(|error| format!("建作业目录失败：{error}"))?;
    std::fs::create_dir_all(dir.join("in"))
        .map_err(|error| format!("建作业目录失败：{error}"))?;
    prune_jobs(root);
    Ok(JobDir { path: dir })
}

/// 只留最新的 [`KEEP_JOBS`] 个作业目录。
///
/// 失败一律忽略：清理是维护动作，不能因为它失败就把这次调用搞挂。
fn prune_jobs(root: &Path) {
    prune_dir(root, KEEP_JOBS);
}

/// 只留最新的 `keep` 个条目（目录或文件），按创建时间。
pub fn prune_dir(root: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut items: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            let created = meta.created().or_else(|_| meta.modified()).ok()?;
            Some((created, entry.path()))
        })
        .collect();
    if items.len() <= keep {
        return;
    }
    items.sort_by_key(|(created, _)| *created);
    let cut = items.len() - keep;
    for (_, path) in items.into_iter().take(cut) {
        let _ = if path.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "prism-jobs-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建测试目录");
        dir
    }

    #[test]
    fn job_dir_has_in_and_out() {
        let root = scratch("layout");
        let job = job_dir_at(&root, "upx sample.exe").expect("建作业目录");
        assert!(job.out().is_dir());
        assert!(job.input().is_dir());
        // 标签里的空格不能进目录名。
        let name = job.path().file_name().unwrap().to_string_lossy().to_string();
        assert!(!name.contains(' '), "{name}");
        assert!(name.contains("upx-sample-exe"), "{name}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn job_dir_is_removed_on_drop() {
        let root = scratch("drop");
        let path = {
            let job = job_dir_at(&root, "x").expect("建作业目录");
            job.path().to_path_buf()
        };
        assert!(!path.exists(), "drop 之后不该还留着");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn old_jobs_are_pruned() {
        let root = scratch("prune");
        // 造 KEEP_JOBS + 3 个"旧"目录：名字无所谓，按 mtime 淘汰。
        for index in 0..(KEEP_JOBS + 3) {
            let dir = root.join(format!("old-{index:03}"));
            std::fs::create_dir_all(&dir).expect("建旧目录");
        }
        let _ = job_dir_at(&root, "new").expect("建作业目录");
        let count = std::fs::read_dir(&root)
            .expect("列目录")
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .count();
        assert!(count <= KEEP_JOBS + 1, "没淘汰干净：还剩 {count} 个");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unknown_backend_has_no_status() {
        assert!(status("definitely-not-registered").is_none());
        assert!(require("definitely-not-registered").is_err());
        // 拼错后端名不该让工具永远可见：fail closed。
        assert!(needs_met(&[Need::Backend("definitely-not-registered")]).is_err());
    }

    #[test]
    fn unmet_reports_missing_backend_in_plain_words() {
        // 用未登记的 id 走不到分支，这里直接构造需求侧的行为：
        // upx 在 CI 上也可能没装，所以只断言"要么满足、要么有原因"。
        let reason = unmet(&Need::Backend("upx"));
        match reason {
            Some(text) => assert!(text.contains("UPX"), "{text}"),
            None => assert!(needs_met(&[Need::Backend("upx")]).is_ok()),
        }
    }

    #[test]
    fn staged_sample_hashes_the_whole_file() {
        use crate::dag::artifact::sha256_of;
        use crate::engines::native::tools::EnabledTools;

        let root = scratch("stage");
        let jobs = scratch("stage-jobs");
        let job = job_dir_at(&jobs, "stage").expect("建作业目录");
        std::fs::write(root.join("sample.bin"), b"hello").expect("写样本");
        let enabled = EnabledTools::all();
        let ctx = ToolContext {
            app: None,
            session_id: "test",
            directory: root.to_str().expect("utf8"),
            call_id: "call",
            provider: None,
            model: None,
            permission: None,
            enabled: &enabled,
            dag: None,
            should_stop: &|| false,
        };
        let staged = stage_sample(&ctx, "sample.bin", &job, 1024).expect("应能准备样本");
        assert_eq!(staged.sha256, sha256_of(b"hello"));
        assert_eq!(staged.size, 5);
        assert_eq!(staged.name, "sample.bin");
        assert!(staged.artifact.is_none());
        assert!(staged.origin_path.is_some());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&jobs);
    }

    #[cfg(unix)]
    #[test]
    fn staged_sample_rejects_symlink_escape() {
        use crate::engines::native::tools::EnabledTools;

        let root = scratch("stage-link");
        let outside = std::env::temp_dir().join(format!(
            "prism-outside-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&outside).expect("建外部目录");
        std::fs::write(outside.join("secret"), b"secret").expect("写外部文件");
        std::os::unix::fs::symlink(outside.join("secret"), root.join("link"))
            .expect("建符号链接");

        let jobs = scratch("stage-link-jobs");
        let job = job_dir_at(&jobs, "stage").expect("建作业目录");
        let enabled = EnabledTools::all();
        let ctx = ToolContext {
            app: None,
            session_id: "test",
            directory: root.to_str().expect("utf8"),
            call_id: "call",
            provider: None,
            model: None,
            permission: None,
            enabled: &enabled,
            dag: None,
            should_stop: &|| false,
        };
        let error = stage_sample(&ctx, "link", &job, 1024).expect_err("应拒绝越界");
        assert!(error.contains("越出工作目录"), "{error}");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&jobs);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
