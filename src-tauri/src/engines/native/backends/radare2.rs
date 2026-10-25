//! `bin.radare2.scan`：用 radare2 自动分析，导出函数边界（`aflj`）→ `ProgramIndex`。
//!
//! 这是 Ghidra 没装时的降级后端，也是 Ghidra 装了之后的交叉校验来源。
//! 只做函数边界，**不做伪 C**：`pdc` 的输出质量撑不起结论，给了模型就会当真。
//!
//! 三条硬规矩：
//!
//! 1. **缓存键是 `sha256 + backend + backend_version`**：同一份样本第二次调用
//!    不再 spawn（缓存命中直接返回）；
//! 2. **输出解析失败不猜**：原文落 `functions_raw`，报错并给前 200 字符；
//! 3. **样本目录一个字节都不写**：r2 在作业目录里跑，样本只读。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::exec::{self, ExecSpec};
use super::index;
use super::{job_dir, sample_ref, stage_sample, JobDir, StagedSample};
use crate::dag::artifact::ArtifactStore;
use crate::engines::native::forensics::{identify, read_head};
use crate::engines::native::tools::Need;
use crate::engines::native::tools::{
    builtin_full, Network, Permission, Risk, ToolContext, ToolDef, ToolMeta, ToolOutcome, ToolResult,
    ToolScope,
};

/// `aaa` 分析一个 10 MB 的样本通常几十秒；300s 是给大样本留的余量（§5.2）。
const SCAN_TIMEOUT: Duration = Duration::from_secs(300);
/// `aflj` 的函数条目带几十个字段，8 MiB 大约能装 2 万函数。
const STDOUT_CAP: usize = 8 * 1024 * 1024;
const STDERR_CAP: usize = 512 * 1024;
/// 制品输入复制到作业目录的上限；路径输入不复制，不设限。
const MAX_STAGED: u64 = 512 * 1024 * 1024;
/// 返回给模型看的函数预览条数：完整清单在缓存/制品里。
const PREVIEW: usize = 30;

pub fn tools() -> Vec<Arc<ToolDef>> {
    vec![builtin_full(
        "bin.radare2.scan",
        "radare2 函数边界（机器码）",
        schema,
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
            needs: &[Need::Backend("radare2")],
        },
        run,
    )]
}

fn schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bin.radare2.scan",
            "description": "用 radare2 自动分析并导出函数边界（aflj），生成 ProgramIndex 反汇编索引（缓存键 sha256+版本，同一份样本第二次调用直接读缓存）。**只处理机器码**（ELF / PE / apk 里的 .so）；DEX 是字节码，用 bin.dex.disasm。只读样本、不执行样本、不产出伪 C；要伪 C 用 Ghidra。",
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

// ---------------------------------------------------------------- 执行

struct Scanned {
    index: index::ProgramIndex,
    cached: bool,
    elapsed_ms: u64,
    cache_error: Option<String>,
    version: String,
    program: PathBuf,
}

/// 下游工具用：复用已经 staged 的样本生成/读取 IR（缓存优先）。
pub(crate) fn ensure_index_from(
    ctx: &ToolContext<'_>,
    job: &JobDir,
    sample: &StagedSample,
    meta: &Value,
) -> Result<index::ProgramIndex, String> {
    Ok(scan_index(ctx, job, sample, meta, true)?.index)
}

fn scan_index(
    ctx: &ToolContext<'_>,
    job: &JobDir,
    sample: &StagedSample,
    meta: &Value,
    allow_cache: bool,
) -> Result<Scanned, String> {
    let backend = super::require("radare2")?;
    let program = backend
        .path
        .clone()
        .ok_or_else(|| "radare2 状态是 ready，但没有路径：点「重新检测」再试".to_string())?;
    let version = backend
        .version
        .clone()
        .unwrap_or_else(|| "unknown".to_string());

    if allow_cache {
        if let Some(found) = index::load(&sample.sha256, "radare2", &version) {
            return Ok(Scanned {
                index: found,
                cached: true,
                elapsed_ms: 0,
                cache_error: None,
                version,
                program,
            });
        }
    }

    let sample_ref = sample_ref(sample, meta);
    let mut spec = ExecSpec::new(&program)
        .args(["-q", "-e", "scr.color=0", "-e", "bin.cache=true", "-c", "aaa; aflj"])
        .arg(sample.path.display().to_string())
        .cwd(job.path())
        .timeout(SCAN_TIMEOUT);
    spec.stdout_cap = STDOUT_CAP;
    spec.stderr_cap = STDERR_CAP;

    let output = exec::run(&spec)?;
    let elapsed_ms = output.duration.as_millis() as u64;

    if output.timed_out {
        return Err(format!(
            "radare2 分析超时（{}s）：{}\n样本较大或函数极多；可以先脱壳，或改用 Ghidra。作业目录已清理，样本没有被改动。",
            SCAN_TIMEOUT.as_secs(),
            output.tail_text(200)
        ));
    }
    if output.stdout_truncated {
        return Err(format!(
            "radare2 的输出超过 {} MiB 上限（函数太多），已截断，无法解析完整 JSON。先用 bin.unpack.upx 脱壳，或改用 Ghidra。样本没有被改动。",
            STDOUT_CAP / (1024 * 1024)
        ));
    }

    let stdout = output.stdout_text();
    if !output.ok() {
        let raw_hint = keep_raw(ctx, sample, &stdout);
        return Err(format!(
            "radare2 分析失败（退出码 {}）：{}\n原文已存为 functions_raw{}。",
            output.code.unwrap_or(-1),
            output.tail_text(400),
            raw_hint
        ));
    }

    let parsed = match index::from_aflj(&stdout, sample_ref, &version) {
        Ok(parsed) => parsed,
        Err(error) => {
            let head: String = stdout.chars().take(200).collect();
            let raw_hint = keep_raw(ctx, sample, &stdout);
            return Err(format!(
                "radare2 的输出不是合法 JSON（退出码 0）：{error}。原文前 200 字符：{head}（已存为 functions_raw{raw_hint}，未做猜测解析）"
            ));
        }
    };
    let mut cache_error = None;
    match index::save(&parsed) {
        Ok(_) => {
            // 每次分析结束清一次缓存（§3.5）；失败不影响本次结果。
            index::prune_cache(index::CACHE_LIMIT);
        }
        // 缓存写不进去不是分析失败：结果还在，继续往下交付。
        Err(error) => cache_error = Some(error),
    }

    Ok(Scanned {
        index: parsed,
        cached: false,
        elapsed_ms,
        cache_error,
        version,
        program,
    })
}

fn run(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let raw = args
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(raw) = raw else {
        return ToolResult::fail("缺少 path");
    };
    let job = match job_dir("radare2") {
        Ok(job) => job,
        Err(error) => return ToolResult::fail(error),
    };
    let sample = match stage_sample(ctx, raw, &job, MAX_STAGED) {
        Ok(sample) => sample,
        Err(error) => return ToolResult::fail(error),
    };

    // 格式/架构只读文件头：IR 里要有"这份索引描述的是什么"，不靠猜。
    let head = read_head(&sample.path, 4096).unwrap_or_default();
    let meta = identify(&head, sample.size);

    let scanned = match scan_index(ctx, &job, &sample, &meta, true) {
        Ok(scanned) => scanned,
        Err(error) => return ToolResult::fail(error),
    };
    let index = scanned.index;
    let version = scanned.version;
    let program = scanned.program;
    let cached = scanned.cached;
    let elapsed_ms = scanned.elapsed_ms;
    let cache_error = scanned.cache_error;

    let cache_path = index::cache_root().map(|root| {
        root.join(&sample.sha256)
            .join("index.json")
            .display()
            .to_string()
    });

    let preview: Vec<Value> = index
        .functions
        .iter()
        .take(PREVIEW)
        .map(|function| {
            json!({
                "va": format!("0x{:x}", function.va),
                "name": function.name,
                "size": function.size,
                "kind": function.kind,
            })
        })
        .collect();

    let mut payload = json!({
        "ok": true,
        "source": {
            "input": raw,
            "path": sample
                .origin_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| sample.path.display().to_string()),
            "name": sample.name,
            "size": sample.size,
            "sha256": sample.sha256,
            "format": meta["format"],
            "arch": meta["arch"],
            "bits": meta["bits"],
        },
        "backend": {
            "id": "radare2",
            "version": version,
            "path": program.display().to_string(),
        },
        "index": {
            "schema": index.schema,
            "cache": cache_path,
            "cached": cached,
        },
        "functions": {
            "count": index.functions.len(),
            "truncated": index.truncated,
            "preview": preview,
        },
        "imports": index.imports.len(),
        "spawned": !cached,
        "elapsedMs": elapsed_ms,
        "note": index.note,
    });
    if let Some(error) = cache_error {
        payload["cacheError"] = json!(error);
    }

    // DAG 里把**完整 IR** 落成制品（不是摘要），下游 cfg.extract 直接取；
    // 聊天里缓存文件就是交付物。
    match ctx.dag {
        Some(scope) => match ArtifactStore::global() {
            Ok(store) => {
                let body = serde_json::to_string(&index).unwrap_or_default();
                let put = store.put_text(
                    &scope.dag_id,
                    &scope.node_id,
                    "functions",
                    &body,
                    sample.artifact.as_deref(),
                );
                match put {
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
            payload["wrote"] = json!(cache_path
                .as_deref()
                .map(|path| format!("缓存（{path}）"))
                .unwrap_or_else(|| "缓存不可用".to_string()));
        }
    }

    ToolResult::plain(ToolOutcome::ok(
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()),
    ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::artifact::sha256_of;
    use crate::engines::native::tools::EnabledTools;
    use std::path::PathBuf;

    /// 缓存清理：避免测试把假样本留在真缓存里。
    fn cache_dir(sha: &str) -> Option<PathBuf> {
        index::cache_root().map(|root| root.join(sha))
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "prism-r2-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建测试目录");
        dir
    }

    #[test]
    fn tool_is_registered_with_radare2_need() {
        let tools = tools();
        assert_eq!(tools.len(), 1);
        let tool = &tools[0];
        assert_eq!(tool.name.as_ref(), "bin.radare2.scan");
        assert_eq!(tool.permission, Permission::ReadOnly);
        assert_eq!(tool.meta.risk, Risk::Read);
        assert_eq!(tool.meta.scope, ToolScope::Any);
        assert!(matches!(tool.meta.needs, [Need::Backend("radare2")]));
    }

    /// 用假 r2 跑完整条链路：探测（env 覆盖）→ spawn → 解析 → 缓存。
    ///
    /// 第二次调用必须不 spawn：假后端自己往 `<脚本>.count` 记一行。
    /// 真 radare2 只在 `#[ignore]` 的回归里用。
    #[test]
    fn scans_through_a_fake_backend_and_reuses_cache() {
        let root = scratch("scan");
        let fake = root.join("r2");
        std::fs::write(
            &fake,
            "#!/bin/sh\n\
             if [ \"$1\" = \"-v\" ]; then\n\
               echo 'radare2 5.9.8 0 @ darwin-arm-64'\n\
               exit 0\n\
             fi\n\
             echo run >> \"$0.count\"\n\
             case \"$*\" in\n\
               *bad.bin*) echo 'not json at all' ;;\n\
               *) printf '%s\\n' '[{\"offset\":4198400,\"name\":\"entry0\",\"size\":64,\"type\":\"sym\"},{\"offset\":4198464,\"name\":\"sub_401040\",\"size\":128,\"type\":\"fcn\"},{\"offset\":4198592,\"name\":\"sym.imp.printf\",\"size\":16,\"type\":\"plt\"}]' ;;\n\
             esac\n",
        )
        .expect("写假后端");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))
                .expect("加执行位");
        }

        let good = root.join("good.bin");
        std::fs::write(&good, b"GOOD SAMPLE").expect("写样本");
        let bad = root.join("bad.bin");
        std::fs::write(&bad, b"BAD SAMPLE").expect("写样本");

        let key = "PRISM_R2";
        let previous = std::env::var_os(key);
        std::env::set_var(key, &fake);
        super::super::refresh("radare2");

        let enabled = EnabledTools::all();
        let ctx = ToolContext {
            app: None,
            session_id: "test",
            directory: root.to_str().expect("临时目录是 utf8"),
            call_id: "call",
            provider: None,
            model: None,
            permission: None,
            selection: None,
            enabled: &enabled,
            dag: None,
            should_stop: &|| false,
        };

        let first = run(&ctx, &json!({ "path": "good.bin" }));
        let first_payload: Value =
            serde_json::from_str(&first.outcome.text).expect("结果是 JSON");
        assert!(first.outcome.error.is_none(), "{}", first.outcome.text);
        assert_eq!(first_payload["spawned"], true);
        assert_eq!(first_payload["index"]["cached"], false);
        assert_eq!(first_payload["functions"]["count"], 3);
        assert_eq!(first_payload["functions"]["preview"][0]["name"], "entry0");

        let second = run(&ctx, &json!({ "path": "good.bin" }));
        let second_payload: Value =
            serde_json::from_str(&second.outcome.text).expect("结果是 JSON");
        assert!(second.outcome.error.is_none(), "{}", second.outcome.text);
        assert_eq!(second_payload["spawned"], false, "缓存命中不该再 spawn");
        assert_eq!(second_payload["index"]["cached"], true);
        assert_eq!(second_payload["functions"]["count"], 3);

        let count = std::fs::read_to_string(format!("{}.count", fake.display()))
            .expect("假后端应留下计数");
        assert_eq!(count.lines().count(), 1, "第二次调用不该 spawn");

        // 非 JSON：失败 + 原文落 functions_raw。
        let broken = run(&ctx, &json!({ "path": "bad.bin" }));
        assert!(broken.outcome.error.is_some());
        let text = &broken.outcome.text;
        assert!(text.contains("不是合法 JSON"), "{text}");
        let raw = index::cache_root()
            .expect("缓存根")
            .join(sha256_of(b"BAD SAMPLE"))
            .join("functions_raw.txt");
        assert_eq!(std::fs::read_to_string(&raw).expect("原文"), "not json at all\n");

        match previous {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
        super::super::refresh("radare2");

        for sample in [b"GOOD SAMPLE".as_slice(), b"BAD SAMPLE".as_slice()] {
            if let Some(dir) = cache_dir(&sha256_of(sample)) {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 真文件回归：`PRISM_TEST_R2_SAMPLE=/path/to/sample cargo test -- --ignored`
    #[test]
    #[ignore = "需要本机装 radare2 和一个真样本（PRISM_TEST_R2_SAMPLE）"]
    fn scans_a_real_sample() {
        let Ok(sample) = std::env::var("PRISM_TEST_R2_SAMPLE") else {
            panic!("先设 PRISM_TEST_R2_SAMPLE 指向一个样本");
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
            selection: None,
            enabled: &enabled,
            dag: None,
            should_stop: &|| false,
        };
        let name = sample.file_name().unwrap().to_string_lossy().to_string();
        let before = std::fs::read_dir(dir).expect("列样本目录").count();
        let result = run(&ctx, &json!({ "path": name }));
        assert!(result.outcome.error.is_none(), "{}", result.outcome.text);
        let payload: Value = serde_json::from_str(&result.outcome.text).expect("结果是 JSON");
        assert!(payload["functions"]["count"].as_u64().unwrap_or(0) > 0);
        // 样本目录一个字节都不该变。
        assert_eq!(std::fs::read_dir(dir).expect("列样本目录").count(), before);
        if let Some(path) = payload["index"]["cache"].as_str() {
            if let Some(dir) = PathBuf::from(path).parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }
}
