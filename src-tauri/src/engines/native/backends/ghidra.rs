//! `bin.ghidra.analyze` / `bin.ghidra.decompile_func`：Ghidra 的分析与反编译。
//!
//! 和 radare2 的分工：r2 轻、快、只给函数边界；Ghidra 重、慢、给基本块和伪 C。
//! 所以 Ghidra 的工具**只在装了 Ghidra 时出现**，而且一次只跑一个 JVM
//! （一个 JVM 起步就吃 1 GB，开两个用户机器会卡）。
//!
//! 四条硬规矩：
//!
//! 1. **项目目录保留**：`decompile_func` 要复用；只有缓存淘汰时才删；
//! 2. **脚本随应用走**：`-scriptPath` 指向 Prism 自己的 `resources/ghidra`，
//!    绝不往用户的 Ghidra 脚本目录写东西；
//! 3. **超时给部分结果**：`-analysisTimeoutPerFile 900` 让 Ghidra 自己收尾，
//!    只要 `functions.json` 写出来了就交付并标 `truncated`，不判失败；
//! 4. **反编译失败给原话**：Ghidra 的 "low-level error" 原样带出去，
//!    不翻译成"反编译失败"——翻译过的话没法查。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};

use super::exec::{self, ExecSpec};
use super::index::{self, ProgramIndex, SampleRef};
use super::probe::{BackendState, BackendStatus};
use super::{job_dir, sample_ref, stage_sample, JobDir, StagedSample};
use crate::dag::artifact::{ArtifactStore, PutRequest};
use crate::engines::native::forensics::{identify, read_head};
use crate::engines::native::tools::Need;
use crate::engines::native::tools::{
    builtin_full, Network, Permission, Risk, ToolContext, ToolDef, ToolMeta, ToolOutcome, ToolResult,
    ToolScope,
};

/// 整个 analyzeHeadless 的硬超时（§5.2 的上限）。Ghidra 自己的分析超时是 900s。
const ANALYZE_TIMEOUT: Duration = Duration::from_secs(1800);
/// Ghidra 内部单文件分析超时：到点它自己收尾并跑 postScript。
const ANALYSIS_TIMEOUT_PER_FILE: u64 = 900;
/// 单函数反编译的硬超时（脚本内部还有 60s 的 DecompInterface 超时）。
const DECOMPILE_TIMEOUT: Duration = Duration::from_secs(120);
const ANALYZE_STDOUT_CAP: usize = 2 * 1024 * 1024;
const ANALYZE_STDERR_CAP: usize = 2 * 1024 * 1024;
const DECOMPILE_STDOUT_CAP: usize = 1024 * 1024;
const DECOMPILE_STDERR_CAP: usize = 512 * 1024;
/// 制品输入复制到作业目录的上限；路径输入不复制，不设限。
const MAX_STAGED: u64 = 1024 * 1024 * 1024;
/// 伪 C 超过这个大小就不整份读进内存（几乎不可能，真遇到说明函数畸形）。
const MAX_CODE_BYTES: usize = 4 * 1024 * 1024;
/// 返回给模型看的函数预览条数：完整清单在缓存/制品里。
const PREVIEW: usize = 30;
/// 伪 C 预览字符数：完整文本在制品/缓存里。
const CODE_PREVIEW: usize = 60_000;
/// Ghidra 项目名固定，`decompile_func` 才能按名字找回同一个项目。
const PROJECT_NAME: &str = "prism";
/// 导入的程序名固定：`stage_sample` 可能给的是原文件名，这里统一成 `sample.bin`。
const PROGRAM_NAME: &str = "sample.bin";

/// Ghidra 一次只能跑一个：JVM 很吃内存，并发两个用户机器会卡（§5.3）。
static JVM: OnceLock<Mutex<()>> = OnceLock::new();

fn jvm_lock() -> std::sync::MutexGuard<'static, ()> {
    JVM.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

pub fn tools() -> Vec<Arc<ToolDef>> {
    vec![
        builtin_full(
            "bin.ghidra.analyze",
            "Ghidra 分析",
            schema_analyze,
            Permission::ReadOnly,
            None,
            None,
            "",
            true,
            ToolMeta {
                tags: &["bin:static", "bin:disasm"],
                risk: Risk::Read,
                network: Network::None,
                sandbox: None,
                scope: ToolScope::Any,
                needs: &[Need::Backend("ghidra")],
            },
            run_analyze,
        ),
        builtin_full(
            "bin.ghidra.decompile_func",
            "Ghidra 单函数反编译",
            schema_decompile,
            Permission::ReadOnly,
            None,
            None,
            "",
            true,
            ToolMeta {
                tags: &["bin:static", "bin:disasm", "bin:decompile"],
                risk: Risk::Read,
                network: Network::None,
                sandbox: None,
                scope: ToolScope::Any,
                needs: &[Need::Backend("ghidra")],
            },
            run_decompile,
        ),
    ]
}

fn schema_analyze() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bin.ghidra.analyze",
            "description": "用 Ghidra 自动分析样本，导出函数边界、基本块与外部符号，生成 ProgramIndex（缓存键 sha256+版本；项目目录保留给反编译复用）。只读样本、不执行样本；要伪 C 用 bin.ghidra.decompile_func。",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "样本路径（相对工作目录），或 DAG 制品引用 artifact:<id>"
                    }
                },
                "required": ["path"]
            }
        }
    })
}

fn schema_decompile() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bin.ghidra.decompile_func",
            "description": "反编译单个函数，产出伪 C（制品 key=pseudocode）。va 接受 0x401000 / 十进制地址 / 函数名；没有项目缓存时会先做一次全量分析。只读样本、不执行样本。",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "样本路径（相对工作目录），或 DAG 制品引用 artifact:<id>"
                    },
                    "va": {
                        "type": "string",
                        "description": "函数地址（0x401000 或十进制）或函数名"
                    }
                },
                "required": ["path", "va"]
            }
        }
    })
}

// ---------------------------------------------------------------- 准备

struct Prepared {
    backend: BackendStatus,
    program: PathBuf,
    sample: StagedSample,
    meta: Value,
    sample_ref: SampleRef,
    /// `~/.prism/cache/programs/<sha>/ghidra`；保留给反编译复用。
    project_dir: PathBuf,
    /// 交给 `-import` 的路径：文件名固定 `sample.bin`。
    import_path: PathBuf,
}

fn prepare(ctx: &ToolContext<'_>, raw: &str) -> Result<(Prepared, JobDir), String> {
    let job = job_dir("ghidra")?;
    let sample = stage_sample(ctx, raw, &job, MAX_STAGED)?;
    // 格式/架构只读文件头：IR 里要有"这份索引描述的是什么"，不靠猜。
    let head = read_head(&sample.path, 4096).unwrap_or_default();
    let meta = identify(&head, sample.size);
    let prepared = prepare_staged(&job, sample, meta)?;
    Ok((prepared, job))
}

/// 从已经准备好的样本构造 Ghidra 任务（下游 `index_of` 复用，避免二次哈希）。
fn prepare_staged(
    job: &JobDir,
    sample: StagedSample,
    meta: Value,
) -> Result<Prepared, String> {
    let backend = require_ghidra()?;
    let program = backend.path.clone().ok_or_else(|| {
        "Ghidra 状态是 ready，但没有 analyzeHeadless 路径：点「重新检测」再试。".to_string()
    })?;
    let sample_ref = sample_ref(&sample, &meta);
    let project_dir = index::cache_root()
        .ok_or_else(|| "找不到 ~/.prism 目录".to_string())?
        .join(&sample.sha256)
        .join("ghidra");
    std::fs::create_dir_all(&project_dir).map_err(|error| {
        format!(
            "建 Ghidra 项目目录失败（{}）：{error}",
            project_dir.display()
        )
    })?;
    let import_path = ensure_import_name(job, &sample)?;

    Ok(Prepared {
        backend,
        program,
        sample,
        meta,
        sample_ref,
        project_dir,
        import_path,
    })
}

/// 下游工具用：复用已经 staged 的样本生成/读取 IR（缓存优先）。
pub(crate) fn ensure_index_from(
    ctx: &ToolContext<'_>,
    job: &JobDir,
    sample: &StagedSample,
    meta: &Value,
) -> Result<ProgramIndex, String> {
    let prepared = prepare_staged(job, sample.clone(), meta.clone())?;
    let _guard = jvm_lock();
    Ok(analyze_inner(ctx, &prepared, job, true)?.index)
}

/// Ghidra 按导入文件名给程序命名；固定成 `sample.bin`，反编译才能按名字找回。
///
/// 优先硬链接（不复制数据、同一文件系统时几乎零成本），其次符号链接，
/// 最后才复制。样本目录仍然一个字节都不写。
fn ensure_import_name(job: &JobDir, sample: &StagedSample) -> Result<PathBuf, String> {
    if sample
        .path
        .file_name()
        .map(|name| name.to_string_lossy() == PROGRAM_NAME)
        .unwrap_or(false)
    {
        return Ok(sample.path.clone());
    }
    let target = job.input().join(PROGRAM_NAME);
    if target.is_file() {
        return Ok(target);
    }
    if std::fs::hard_link(&sample.path, &target).is_ok() {
        return Ok(target);
    }
    #[cfg(unix)]
    if std::os::unix::fs::symlink(&sample.path, &target).is_ok() {
        return Ok(target);
    }
    if sample.size > MAX_STAGED {
        return Err(format!(
            "样本 {:.1} MiB，超过 Ghidra 复制上限 {} MiB；先脱壳或换个小样本。",
            sample.size as f64 / (1024.0 * 1024.0),
            MAX_STAGED / (1024 * 1024)
        ));
    }
    std::fs::copy(&sample.path, &target)
        .map_err(|error| format!("准备 Ghidra 输入失败（{}）：{error}", target.display()))?;
    Ok(target)
}

/// Ghidra 没接上时用 §11 的原文案，而不是通用的后端提示。
fn require_ghidra() -> Result<BackendStatus, String> {
    let status =
        super::status("ghidra").ok_or_else(|| "没有登记过的后端：ghidra".to_string())?;
    match &status.state {
        BackendState::Ready => Ok(status),
        BackendState::Missing => Err(
            "Ghidra 没接上：没找到 analyzeHeadless。装好后在 设置 → 工具链 里指一下 \
             support/analyzeHeadless（或设 GHIDRA_HOME）。"
                .to_string(),
        ),
        BackendState::TooOld { found, need } => Err(format!(
            "Ghidra 版本太低：检测到 {found}，至少要 {need}。升级：brew upgrade --cask ghidra"
        )),
        BackendState::Broken(why) => Err(format!(
            "找到 Ghidra，但跑不起来（{why}）。Ghidra 需要 JDK 17+，装好后点「重新检测」。"
        )),
        BackendState::Unknown => {
            Err("Ghidra 的状态还没探到，点「重新检测」再试。".to_string())
        }
    }
}

/// 脚本目录：环境变量 → Tauri 资源目录 → 开发时的 `resources/ghidra` → 可执行文件旁。
fn script_dir(ctx: &ToolContext<'_>) -> Result<PathBuf, String> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(raw) = std::env::var_os("PRISM_GHIDRA_SCRIPTS").filter(|value| !value.is_empty()) {
        candidates.push(PathBuf::from(raw));
    }
    if let Some(app) = ctx.app {
        use tauri::Manager;
        if let Ok(dir) = app.path().resource_dir() {
            candidates.push(dir.join("resources/ghidra"));
            candidates.push(dir.join("ghidra"));
        }
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/ghidra"));
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("resources/ghidra"));
            candidates.push(dir.join("../Resources/resources/ghidra"));
        }
    }
    for dir in candidates {
        if dir.join("PrismExport.java").is_file() && dir.join("PrismDecompile.java").is_file() {
            return Ok(dir);
        }
    }
    Err(
        "找不到 Ghidra 脚本（PrismExport.java / PrismDecompile.java）。这是安装包不完整；\
         重装 Prism，或设 PRISM_GHIDRA_SCRIPTS 指向 resources/ghidra。"
            .to_string(),
    )
}

/// `JAVA_HOME` 不往子进程传，Ghidra 可能找不到 JDK；显式透传。
fn java_home() -> Option<String> {
    std::env::var_os("JAVA_HOME")
        .filter(|value| !value.is_empty())
        .map(|value| PathBuf::from(value).display().to_string())
}

fn run_ghidra(
    program: &Path,
    args: Vec<String>,
    cwd: &Path,
    timeout: Duration,
    stdout_cap: usize,
    stderr_cap: usize,
) -> Result<exec::ExecOutput, String> {
    let mut spec = ExecSpec::new(program).args(args).cwd(cwd).timeout(timeout);
    spec.stdout_cap = stdout_cap;
    spec.stderr_cap = stderr_cap;
    if let Some(home) = java_home() {
        spec = spec.env("JAVA_HOME", home);
    }
    exec::run(&spec)
}

fn project_exists(dir: &Path) -> bool {
    dir.join(format!("{PROJECT_NAME}.gpr")).is_file()
        || dir.join(format!("{PROJECT_NAME}.rep")).is_dir()
}

fn cache_path(sha256: &str) -> Option<PathBuf> {
    index::cache_root().map(|root| root.join(sha256).join("index.json"))
}

// ---------------------------------------------------------------- analyze

struct Analyzed {
    index: ProgramIndex,
    cached: bool,
    elapsed_ms: u64,
    project_dir: PathBuf,
    cache_path: Option<PathBuf>,
    cache_error: Option<String>,
    timed_out: bool,
    log: String,
    log_truncated: bool,
}

fn analyze_inner(
    ctx: &ToolContext<'_>,
    prepared: &Prepared,
    job: &JobDir,
    allow_cache: bool,
) -> Result<Analyzed, String> {
    let version = prepared
        .backend
        .version
        .clone()
        .unwrap_or_else(|| "unknown".to_string());

    if allow_cache {
        if let Some(found) = index::load(&prepared.sample.sha256, "ghidra", &version) {
            return Ok(Analyzed {
                index: found,
                cached: true,
                elapsed_ms: 0,
                project_dir: prepared.project_dir.clone(),
                cache_path: cache_path(&prepared.sample.sha256),
                cache_error: None,
                timed_out: false,
                log: String::new(),
                log_truncated: false,
            });
        }
    }

    let scripts = script_dir(ctx)?;
    let out_json = job.out().join("functions.json");
    let args = vec![
        prepared.project_dir.display().to_string(),
        PROJECT_NAME.to_string(),
        "-import".to_string(),
        prepared.import_path.display().to_string(),
        "-overwrite".to_string(),
        "-scriptPath".to_string(),
        scripts.display().to_string(),
        "-postScript".to_string(),
        "PrismExport.java".to_string(),
        out_json.display().to_string(),
        "-analysisTimeoutPerFile".to_string(),
        ANALYSIS_TIMEOUT_PER_FILE.to_string(),
        "-max-cpu".to_string(),
        "2".to_string(),
    ];
    let output = run_ghidra(
        &prepared.program,
        args,
        job.path(),
        ANALYZE_TIMEOUT,
        ANALYZE_STDOUT_CAP,
        ANALYZE_STDERR_CAP,
    )?;
    let elapsed_ms = output.duration.as_millis() as u64;
    let log = one_line(&output.stdout_text(), &output.stderr_text(), 400);
    let log_truncated = output.stdout_truncated || output.stderr_truncated;

    // 硬超时且脚本没来得及写 JSON：这才是真失败。
    if output.timed_out && !out_json.is_file() {
        return Err(format!(
            "Ghidra 分析超时（{}s）：{}\n\
             样本较大或函数极多；作业目录已清理，样本没有被改动。",
            ANALYZE_TIMEOUT.as_secs(),
            output.tail_text(300)
        ));
    }

    let text = match std::fs::read_to_string(&out_json) {
        Ok(text) => text,
        Err(error) => {
            return Err(format!(
                "Ghidra 说分析完了，但读不到 {}（退出码 {}）：{error}\n{}",
                out_json.display(),
                output.code.unwrap_or(-1),
                output.tail_text(400)
            ))
        }
    };

    let mut parsed = match index::from_ghidra(&text, prepared.sample_ref.clone(), &version) {
        Ok(parsed) => parsed,
        Err(error) => {
            let head: String = text.chars().take(200).collect();
            let raw_hint = keep_raw(ctx, &prepared.sample, &text);
            return Err(format!(
                "Ghidra 导出的不是合法 JSON：{error}。原文前 200 字符：{head}\
                 （已存为 functions_raw{raw_hint}，未做猜测解析）"
            ));
        }
    };

    let mut notes: Vec<String> = Vec::new();
    if output.timed_out {
        parsed.truncated = true;
        notes.push(format!(
            "Ghidra 分析超时（{}s）：已导出前 {} 个函数，标了 truncated。\
             样本较大可以先脱壳，或调小 -analysisTimeoutPerFile。",
            ANALYZE_TIMEOUT.as_secs(),
            parsed.functions.len()
        ));
    } else if !output.ok() {
        notes.push(format!(
            "Ghidra 退出码 {}，但 functions.json 已写出，按已导出的结果交付。",
            output.code.unwrap_or(-1)
        ));
    }
    if log_truncated {
        notes.push("Ghidra 的输出超过上限，日志只保留了头尾。".to_string());
    }
    if let Some(existing) = parsed.note.take() {
        notes.push(existing);
    }
    if !notes.is_empty() {
        parsed.note = Some(notes.join(" "));
    }

    let mut cache_error = None;
    match index::save(&parsed) {
        Ok(_) => {
            // 每次分析结束清一次缓存（§3.5）；失败不影响本次结果。
            index::prune_cache(index::CACHE_LIMIT);
        }
        // 缓存写不进去不是分析失败：结果还在，继续往下交付。
        Err(error) => cache_error = Some(error),
    }

    Ok(Analyzed {
        index: parsed,
        cached: false,
        elapsed_ms,
        project_dir: prepared.project_dir.clone(),
        cache_path: cache_path(&prepared.sample.sha256),
        cache_error,
        timed_out: output.timed_out,
        log,
        log_truncated,
    })
}

fn run_analyze(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let raw = args
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(raw) = raw else {
        return ToolResult::fail("缺少 path");
    };
    let (prepared, job) = match prepare(ctx, raw) {
        Ok(prepared) => prepared,
        Err(error) => return ToolResult::fail(error),
    };

    let analyzed = {
        let _guard = jvm_lock();
        match analyze_inner(ctx, &prepared, &job, true) {
            Ok(analyzed) => analyzed,
            Err(error) => return ToolResult::fail(error),
        }
    };

    let preview: Vec<Value> = analyzed
        .index
        .functions
        .iter()
        .take(PREVIEW)
        .map(|function| {
            json!({
                "va": format!("0x{:x}", function.va),
                "name": function.name,
                "size": function.size,
                "kind": function.kind,
                "blocks": function.blocks.as_ref().map(Vec::len).unwrap_or(0),
            })
        })
        .collect();
    let with_blocks = analyzed
        .index
        .functions
        .iter()
        .filter(|function| function.blocks.is_some())
        .count();
    let block_count: usize = analyzed
        .index
        .functions
        .iter()
        .filter_map(|function| function.blocks.as_ref())
        .map(Vec::len)
        .sum();

    let mut payload = json!({
        "ok": true,
        "source": source_json(raw, &prepared),
        "backend": backend_json(&prepared),
        "project": {
            "dir": analyzed.project_dir.display().to_string(),
            "name": PROJECT_NAME,
            "program": PROGRAM_NAME,
        },
        "index": {
            "schema": analyzed.index.schema,
            "cache": analyzed.cache_path.as_ref().map(|path| path.display().to_string()),
            "cached": analyzed.cached,
        },
        "functions": {
            "count": analyzed.index.functions.len(),
            "truncated": analyzed.index.truncated,
            "withBlocks": with_blocks,
            "blocks": block_count,
            "preview": preview,
        },
        "imports": analyzed.index.imports.len(),
        "spawned": !analyzed.cached,
        "elapsedMs": analyzed.elapsed_ms,
        "timedOut": analyzed.timed_out,
        "logTruncated": analyzed.log_truncated,
        "note": analyzed.index.note,
    });
    if !analyzed.log.is_empty() {
        payload["log"] = json!(analyzed.log);
    }
    if let Some(error) = analyzed.cache_error {
        payload["cacheError"] = json!(error);
    }

    // DAG 里把完整 IR 落成制品；聊天里缓存文件就是交付物。
    match ctx.dag {
        Some(scope) => match ArtifactStore::global() {
            Ok(store) => {
                let body = serde_json::to_string(&analyzed.index).unwrap_or_default();
                match store.put_text(
                    &scope.dag_id,
                    &scope.node_id,
                    "functions",
                    &body,
                    prepared.sample.artifact.as_deref(),
                ) {
                    Ok(reference) => {
                        payload["artifact"] = json!(reference.id);
                        payload["wrote"] = json!(format!(
                            "制品库（key=functions，{} 字节）",
                            reference.size
                        ));
                    }
                    Err(error) => payload["artifactError"] = json!(error),
                }
            }
            Err(error) => payload["artifactError"] = json!(error),
        },
        None => {
            payload["wrote"] = json!(analyzed
                .cache_path
                .as_ref()
                .map(|path| format!("缓存（{}）", path.display()))
                .unwrap_or_else(|| "缓存不可用".to_string()));
        }
    }

    ToolResult::plain(ToolOutcome::ok(
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()),
    ))
}

// ---------------------------------------------------------------- decompile

fn run_decompile(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let raw = args
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(raw) = raw else {
        return ToolResult::fail("缺少 path");
    };
    let target = args
        .get("va")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(target) = target else {
        return ToolResult::fail("缺少 va（0x401000、十进制地址或函数名）");
    };
    if target.chars().any(char::is_whitespace) {
        return ToolResult::fail("va 不能带空格：传 0x401000、十进制地址或函数名");
    }
    if target.len() > 128 {
        return ToolResult::fail("va 太长（超过 128 字符）：这不像地址或函数名");
    }

    let (prepared, job) = match prepare(ctx, raw) {
        Ok(prepared) => prepared,
        Err(error) => return ToolResult::fail(error),
    };

    let (code, output, analyzed_first, cache_file, cache_error) = {
        let _guard = jvm_lock();

        // 没有项目缓存就先全量分析一次（只一次，不递归）。
        let mut analyzed_first = false;
        if !project_exists(&prepared.project_dir) {
            analyzed_first = true;
            if let Err(error) = analyze_inner(ctx, &prepared, &job, false) {
                return ToolResult::fail(format!(
                    "为反编译先做了一次全量分析，但失败了：{error}"
                ));
            }
        }

        let scripts = match script_dir(ctx) {
            Ok(dir) => dir,
            Err(error) => return ToolResult::fail(error),
        };
        let safe = safe_name(target);
        let out_c = job.out().join(format!("{safe}.c"));
        let args = vec![
            prepared.project_dir.display().to_string(),
            PROJECT_NAME.to_string(),
            "-process".to_string(),
            PROGRAM_NAME.to_string(),
            "-noanalysis".to_string(),
            "-scriptPath".to_string(),
            scripts.display().to_string(),
            "-postScript".to_string(),
            "PrismDecompile.java".to_string(),
            target.to_string(),
            out_c.display().to_string(),
        ];
        let output = match run_ghidra(
            &prepared.program,
            args,
            job.path(),
            DECOMPILE_TIMEOUT,
            DECOMPILE_STDOUT_CAP,
            DECOMPILE_STDERR_CAP,
        ) {
            Ok(output) => output,
            Err(error) => return ToolResult::fail(error),
        };
        if output.timed_out {
            return ToolResult::fail(format!(
                "Ghidra 反编译超时（{}s）：函数太大或反编译器卡住。\
                 可以先用 bin.ghidra.analyze 看函数大小。",
                DECOMPILE_TIMEOUT.as_secs()
            ));
        }
        if !output.ok() {
            return ToolResult::fail(format!(
                "Ghidra 反编译失败（退出码 {}）：{}",
                output.code.unwrap_or(-1),
                output.tail_text(600)
            ));
        }

        let code = match std::fs::read_to_string(&out_c) {
            Ok(code) => code,
            Err(error) => {
                return ToolResult::fail(format!(
                    "Ghidra 说反编译完了，但读不到 {}：{error}",
                    out_c.display()
                ))
            }
        };
        if code.trim().is_empty() {
            return ToolResult::fail(
                "Ghidra 反编译完成，但产物是空的（函数可能只有跳转/没有函数体）。",
            );
        }
        if code.len() > MAX_CODE_BYTES {
            return ToolResult::fail(format!(
                "伪 C 有 {:.1} MiB，超过 {} MiB 上限，没有读取。",
                code.len() as f64 / (1024.0 * 1024.0),
                MAX_CODE_BYTES / (1024 * 1024)
            ));
        }

        // 写一份到缓存：聊天模式没有制品库，这份文件就是交付物。
        let safe = safe_name(target);
        let cache_file = index::cache_root().map(|root| {
            root.join(&prepared.sample.sha256)
                .join("pseudocode")
                .join(format!("{safe}.c"))
        });
        let mut cache_error = None;
        if let Some(path) = &cache_file {
            if let Some(parent) = path.parent() {
                if let Err(error) = std::fs::create_dir_all(parent) {
                    cache_error = Some(format!("建伪 C 缓存目录失败：{error}"));
                }
            }
            if cache_error.is_none() {
                if let Err(error) = std::fs::write(path, code.as_bytes()) {
                    cache_error = Some(format!("写伪 C 缓存失败：{error}"));
                }
            }
        }
        (code, output, analyzed_first, cache_file, cache_error)
    };

    let (va, name, size) = parse_function_line(&output.stdout_text()).unwrap_or_else(|| {
        (
            0,
            target.to_string(),
            0,
        )
    });
    let chars = code.chars().count();
    let lines = code.lines().count();
    let code_preview: String = code.chars().take(CODE_PREVIEW).collect();
    let preview_truncated = chars > CODE_PREVIEW;

    let mut payload = json!({
        "ok": true,
        "source": source_json(raw, &prepared),
        "backend": backend_json(&prepared),
        "project": {
            "dir": prepared.project_dir.display().to_string(),
            "name": PROJECT_NAME,
            "program": PROGRAM_NAME,
        },
        "function": {
            "requested": target,
            "name": name,
            "va": format!("0x{:x}", va),
            "size": size,
        },
        "pseudocode": {
            "chars": chars,
            "lines": lines,
            "truncated": preview_truncated,
            "cache": cache_file.as_ref().map(|path| path.display().to_string()),
        },
        "code": code_preview,
        "analyzedFirst": analyzed_first,
        "elapsedMs": output.duration.as_millis() as u64,
        "note": if analyzed_first {
            "项目缓存里没有这份样本，先做了一次全量分析（只做这一次）。"
        } else {
            "复用已有的 Ghidra 项目，没有重新分析。"
        },
    });
    if preview_truncated {
        payload["note"] = json!(format!(
            "伪 C 有 {chars} 字符，只返回前 {CODE_PREVIEW} 字符；完整文本在制品/缓存里。"
        ));
    }
    if let Some(error) = cache_error {
        payload["cacheError"] = json!(error);
    }

    if let Some(scope) = ctx.dag {
        match ArtifactStore::global() {
            Ok(store) => {
                let file_name = format!("{}.c", safe_name(target));
                match store.put(PutRequest {
                    dag_id: &scope.dag_id,
                    node_id: &scope.node_id,
                    key: "pseudocode",
                    name: &file_name,
                    kind: "text",
                    parent_id: prepared.sample.artifact.as_deref(),
                    bytes: code.as_bytes(),
                }) {
                    Ok(reference) => {
                        payload["pseudocode"]["artifact"] = json!(reference.id);
                        payload["wrote"] = json!(format!(
                            "制品库（key=pseudocode，{} 字节）",
                            reference.size
                        ));
                    }
                    Err(error) => payload["artifactError"] = json!(error),
                }
            }
            Err(error) => payload["artifactError"] = json!(error),
        }
    } else {
        payload["wrote"] = json!(cache_file
            .as_ref()
            .map(|path| format!("缓存（{}）", path.display()))
            .unwrap_or_else(|| "缓存不可用".to_string()));
    }

    ToolResult::plain(ToolOutcome::ok(
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()),
    ))
}

/// `PRISM_FUNCTION\t<va>\t<name>\t<size>`：脚本自己报回函数的真实身份。
fn parse_function_line(stdout: &str) -> Option<(u64, String, u64)> {
    for line in stdout.lines() {
        let Some(rest) = line.strip_prefix("PRISM_FUNCTION\t") else {
            continue;
        };
        let mut parts = rest.splitn(3, '\t');
        let va = parts.next()?.parse().ok()?;
        let name = parts.next()?.to_string();
        let size = parts.next()?.parse().ok()?;
        return Some((va, name, size));
    }
    None
}

// ---------------------------------------------------------------- 小工具

fn source_json(raw: &str, prepared: &Prepared) -> Value {
    json!({
        "input": raw,
        "path": prepared
            .sample
            .origin_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| prepared.sample.path.display().to_string()),
        "name": prepared.sample.name,
        "size": prepared.sample.size,
        "sha256": prepared.sample.sha256,
        "format": prepared.meta["format"],
        "arch": prepared.meta["arch"],
        "bits": prepared.meta["bits"],
    })
}

fn backend_json(prepared: &Prepared) -> Value {
    json!({
        "id": "ghidra",
        "version": prepared.backend.version,
        "path": prepared.program.display().to_string(),
        "note": prepared.backend.note,
    })
}

fn one_line(stdout: &str, stderr: &str, limit: usize) -> String {
    let source = if stderr.trim().is_empty() { stdout } else { stderr };
    let text: String = source
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    text.chars().take(limit).collect()
}

/// 解析失败时把原文留在缓存目录；有 DAG 作用域就再落一份 `functions_raw` 制品。
fn keep_raw(ctx: &ToolContext<'_>, sample: &StagedSample, text: &str) -> String {
    let mut hint = match index::write_raw(&sample.sha256, text) {
        Ok(path) => format!("（{}）", path.display()),
        Err(error) => format!("（写入失败：{error}）"),
    };
    if let Some(scope) = ctx.dag {
        if let Ok(store) = ArtifactStore::global() {
            match store.put_text(
                &scope.dag_id,
                &scope.node_id,
                "functions_raw",
                text,
                sample.artifact.as_deref(),
            ) {
                Ok(reference) => hint.push_str(&format!("，制品 {}", reference.id)),
                Err(error) => hint.push_str(&format!("，制品写入失败：{error}")),
            }
        }
    }
    hint
}

/// 缓存文件名只允许安全字符：函数名里可能带 `::`、`/`、`..`。
fn safe_name(target: &str) -> String {
    let mut out = String::with_capacity(target.len());
    for ch in target.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let out: String = out.chars().take(80).collect();
    if out.is_empty() || out == "." || out == ".." {
        "function".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::artifact::sha256_of;
    use crate::engines::native::tools::EnabledTools;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "prism-ghidra-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建测试目录");
        dir
    }

    #[test]
    fn tool_is_registered_with_ghidra_need() {
        let tools = tools();
        assert_eq!(tools.len(), 2);
        for tool in &tools {
            assert_eq!(tool.permission, Permission::ReadOnly);
            assert_eq!(tool.meta.risk, Risk::Read);
            assert_eq!(tool.meta.scope, ToolScope::Any);
            assert!(matches!(tool.meta.needs, [Need::Backend("ghidra")]));
        }
        assert_eq!(tools[0].name.as_ref(), "bin.ghidra.analyze");
        assert_eq!(tools[1].name.as_ref(), "bin.ghidra.decompile_func");
    }

    #[test]
    fn safe_names_do_not_escape_the_cache_dir() {
        assert_eq!(safe_name("0x401000"), "0x401000");
        assert_eq!(safe_name("Class::method"), "Class__method");
        assert_eq!(safe_name("../../etc/passwd"), ".._.._etc_passwd");
        assert_eq!(safe_name(".."), "function");
        assert!(safe_name(&"a".repeat(200)).len() <= 80);
    }

    #[test]
    fn function_line_is_parsed() {
        let stdout = "PrismDecompile: entry @ 00401000\nPRISM_FUNCTION\t4198400\tentry\t64\n";
        assert_eq!(
            parse_function_line(stdout),
            Some((4198400, "entry".to_string(), 64))
        );
        assert!(parse_function_line("nothing here").is_none());
    }

    /// 用假 analyzeHeadless 跑完整条链路：探测 → analyze → 缓存 → decompile。
    ///
    /// 真 Ghidra 只在 `#[ignore]` 的回归里用：它要 JDK + 几个 GB 的安装。
    #[test]
    fn analyzes_and_decompiles_through_a_fake_backend() {
        let root = scratch("run");
        let home = root.join("ghidra");
        std::fs::create_dir_all(home.join("support")).expect("建 support");
        std::fs::create_dir_all(home.join("Ghidra")).expect("建 Ghidra");
        std::fs::write(
            home.join("Ghidra/application.properties"),
            "application.version=11.2.1\n",
        )
        .expect("写版本");
        let jdk = root.join("jdk");
        std::fs::create_dir_all(jdk.join("bin")).expect("建 jdk");
        let java = jdk.join("bin/java");
        std::fs::write(
            &java,
            "#!/bin/sh\necho 'openjdk version \"21.0.1\" 2024-04-16' >&2\nexit 0\n",
        )
        .expect("写假 java");
        make_executable(&java);

        let fake = home.join("support/analyzeHeadless");
        std::fs::write(&fake, FAKE_HEADLESS).expect("写假后端");
        make_executable(&fake);

        let sample = root.join("sample.exe");
        std::fs::write(&sample, b"FAKE GHIDRA SAMPLE").expect("写样本");

        let previous_home = std::env::var_os("PRISM_GHIDRA_HOME");
        let previous_java = std::env::var_os("JAVA_HOME");
        std::env::set_var("PRISM_GHIDRA_HOME", &home);
        std::env::set_var("JAVA_HOME", &jdk);
        super::super::refresh("ghidra");

        let enabled = EnabledTools::all();
        let ctx = ToolContext {
            app: None,
            session_id: "test",
            directory: root.to_str().expect("临时目录是 utf8"),
            call_id: "call",
            provider: None,
            model: None,
            permission: None,
            enabled: &enabled,
            dag: None,
            should_stop: &|| false,
        };

        // 1. analyze：spawn 一次，出 IR 和项目目录。
        let first = run_analyze(&ctx, &json!({ "path": "sample.exe" }));
        let payload: Value = serde_json::from_str(&first.outcome.text)
            .unwrap_or_else(|error| panic!("{error}: {}", first.outcome.text));
        assert!(first.outcome.error.is_none(), "{}", first.outcome.text);
        assert_eq!(payload["spawned"], true);
        assert_eq!(payload["index"]["cached"], false);
        assert_eq!(payload["functions"]["count"], 2);
        assert_eq!(payload["functions"]["preview"][0]["name"], "entry");
        assert_eq!(payload["functions"]["withBlocks"], 1);
        assert_eq!(payload["functions"]["blocks"], 1);
        assert_eq!(payload["imports"], 1);
        let project = PathBuf::from(payload["project"]["dir"].as_str().expect("项目目录"));
        assert!(project.join("prism.gpr").is_file(), "项目目录要保留");

        // 2. 第二次 analyze：缓存命中，不再 spawn。
        let second = run_analyze(&ctx, &json!({ "path": "sample.exe" }));
        let payload: Value =
            serde_json::from_str(&second.outcome.text).expect("结果是 JSON");
        assert!(second.outcome.error.is_none(), "{}", second.outcome.text);
        assert_eq!(payload["spawned"], false);
        assert_eq!(payload["index"]["cached"], true);

        // 3. decompile：复用项目，不再分析。
        let decompiled = run_decompile(&ctx, &json!({ "path": "sample.exe", "va": "0x401000" }));
        let payload: Value =
            serde_json::from_str(&decompiled.outcome.text).expect("结果是 JSON");
        assert!(decompiled.outcome.error.is_none(), "{}", decompiled.outcome.text);
        assert_eq!(payload["analyzedFirst"], false);
        assert_eq!(payload["function"]["name"], "entry");
        assert_eq!(payload["function"]["va"], "0x401000");
        assert!(payload["code"].as_str().unwrap_or("").contains("return 42"));
        let cache = PathBuf::from(payload["pseudocode"]["cache"].as_str().expect("缓存路径"));
        assert!(cache.is_file(), "伪 C 要落缓存");

        // 4. 删掉项目目录：decompile 自己补一次分析。
        std::fs::remove_dir_all(&project).expect("删项目目录");
        let again = run_decompile(&ctx, &json!({ "path": "sample.exe", "va": "entry" }));
        let payload: Value =
            serde_json::from_str(&again.outcome.text).expect("结果是 JSON");
        assert!(again.outcome.error.is_none(), "{}", again.outcome.text);
        assert_eq!(payload["analyzedFirst"], true);
        assert_eq!(payload["function"]["name"], "entry");

        // 假后端计数：analyze 1 次 + 缓存命中 0 次 + decompile 1 次 + 补分析 1 次
        // + decompile 1 次 = 4。
        let count = std::fs::read_to_string(format!("{}.count", fake.display()))
            .expect("假后端应留下计数");
        assert_eq!(count.lines().count(), 4, "{count}");

        match previous_home {
            Some(value) => std::env::set_var("PRISM_GHIDRA_HOME", value),
            None => std::env::remove_var("PRISM_GHIDRA_HOME"),
        }
        match previous_java {
            Some(value) => std::env::set_var("JAVA_HOME", value),
            None => std::env::remove_var("JAVA_HOME"),
        }
        super::super::refresh("ghidra");

        if let Some(dir) = index::cache_root().map(|root| root.join(sha256_of(b"FAKE GHIDRA SAMPLE"))) {
            let _ = std::fs::remove_dir_all(dir);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 真文件回归：`PRISM_TEST_GHIDRA_SAMPLE=/path/to/sample cargo test -- --ignored`
    #[test]
    #[ignore = "需要本机装 Ghidra + JDK 和一个真样本（PRISM_TEST_GHIDRA_SAMPLE）"]
    fn analyzes_and_decompiles_a_real_sample() {
        let Ok(sample) = std::env::var("PRISM_TEST_GHIDRA_SAMPLE") else {
            panic!("先设 PRISM_TEST_GHIDRA_SAMPLE 指向一个样本");
        };
        let sample = PathBuf::from(sample);
        let dir = sample.parent().expect("样本要有父目录");
        let enabled = EnabledTools::all();
        let ctx = ToolContext {
            app: None,
            session_id: "test",
            directory: dir.to_str().expect("utf8"),
            call_id: "call",
            provider: None,
            model: None,
            permission: None,
            enabled: &enabled,
            dag: None,
            should_stop: &|| false,
        };
        let name = sample.file_name().unwrap().to_string_lossy().to_string();
        let analyzed = run_analyze(&ctx, &json!({ "path": name }));
        assert!(
            analyzed.outcome.error.is_none(),
            "{}",
            analyzed.outcome.text
        );
        let payload: Value =
            serde_json::from_str(&analyzed.outcome.text).expect("结果是 JSON");
        assert!(payload["functions"]["count"].as_u64().unwrap_or(0) > 0);
        let first = payload["functions"]["preview"][0]["va"]
            .as_str()
            .expect("第一个函数的 va");
        let decompiled = run_decompile(&ctx, &json!({ "path": name, "va": first }));
        assert!(
            decompiled.outcome.error.is_none(),
            "{}",
            decompiled.outcome.text
        );
        let payload: Value =
            serde_json::from_str(&decompiled.outcome.text).expect("结果是 JSON");
        assert!(!payload["code"].as_str().unwrap_or("").is_empty());
        if let Some(path) = payload["pseudocode"]["cache"].as_str() {
            if let Some(dir) = PathBuf::from(path).parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
        if let Some(path) = payload["project"]["dir"].as_str() {
            let _ = std::fs::remove_dir_all(path);
        }
    }

    fn make_executable(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
                .expect("加执行位");
        }
    }

    /// 假 analyzeHeadless：按 `-postScript` 的类型写 JSON / 伪 C。
    const FAKE_HEADLESS: &str = r#"#!/bin/sh
echo run >> "$0.count"
loc="$1"
proj="$2"
shift 2
mode=analyze
out=""
while [ $# -gt 0 ]; do
  case "$1" in
    -postScript)
      shift
      script="$1"
      shift
      if [ "$script" = "PrismExport.java" ]; then
        out="$1"
        shift
      elif [ "$script" = "PrismDecompile.java" ]; then
        target="$1"
        out="$2"
        shift 2
        mode=decompile
      fi
      ;;
    -process)
      mode=decompile
      shift 2
      ;;
    -import|-scriptPath|-analysisTimeoutPerFile|-max-cpu)
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done
if [ -z "$out" ]; then
  echo "fake analyzeHeadless: 没拿到输出路径" >&2
  exit 2
fi
mkdir -p "$(dirname "$out")"
if [ "$mode" = "decompile" ]; then
  printf 'int entry(void)\n{\n  return 42;\n}\n' > "$out"
  printf 'PRISM_FUNCTION\t4198400\tentry\t64\n'
else
  printf '%s\n' '{"functions":[{"va":4198400,"name":"entry","size":64,"kind":"normal","blocks":[{"va":4198400,"size":24,"succ":[4198424]}]},{"va":4198424,"name":"thunk_FUN_1","size":5,"kind":"thunk","blocks":[]}],"imports":[{"name":"printf","library":"libc"}],"truncated":false,"imageBase":4294967296}' > "$out"
  : > "$loc/$proj.gpr"
fi
exit 0
"#;
}
