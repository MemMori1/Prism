//! `bin.unpack.upx`：用 UPX 自带的解压器脱标准 UPX 壳。
//!
//! 这是整条外部程序链路的**最小闭环**：探测（后端在不在）→ 执行（超时/上限/
//! 进程组）→ 产物（落制品、带 parent）→ 结果（如实写清做了什么）。
//!
//! 三条硬规矩：
//!
//! 1. **先 `-t` 再 `-d`**：`upx -t` 是它自己的校验器。不认就不解——
//!    改过头的样本硬解出来是垃圾，还会让人以为脱成功了；
//! 2. **不给 `--force`**：不是"默认关"，是这个工具根本没有这个参数；
//! 3. **样本目录一个字节都不写**：`upx -d -o` 的输出落在作业目录，
//!    读完就随作业目录一起删。DAG 里进制品库，聊天里进 `~/.prism/unpacked/`。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::exec::{self, ExecSpec};
use super::{job_dir, prune_dir};
use crate::dag::artifact::{sha256_of, ArtifactStore, PutRequest};
use crate::engines::native::forensics::{read_head, target_of};
use crate::engines::native::tools::{
    builtin_full, Network, Permission, Risk, ToolContext, ToolDef, ToolMeta, ToolOutcome, ToolResult,
    ToolScope,
};
use crate::engines::native::tools::Need;

/// 脱壳产物上限：再大就不该整份读进内存（也不该塞进制品库）。
const MAX_UNPACKED: u64 = 256 * 1024 * 1024;
/// UPX 解一个 100 MB 的样本通常几秒；给到 2 分钟是给慢盘留余量。
const UNPACK_TIMEOUT: Duration = Duration::from_secs(120);
/// 聊天模式下最多留多少个脱壳产物。
const KEEP_UNPACKED: usize = 20;

pub fn tools() -> Vec<Arc<ToolDef>> {
    vec![builtin_full(
        "bin.unpack.upx",
        "UPX 脱壳",
        schema,
        Permission::ReadOnly,
        None,
        None,
        "",
        true,
        ToolMeta {
            tags: &["bin:static", "bin:unpack"],
            risk: Risk::Read,
            network: Network::None,
            sandbox: None,
            scope: ToolScope::Any,
            needs: &[Need::Backend("upx")],
        },
        run,
    )]
}

fn schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bin.unpack.upx",
            "description": "用 UPX 解压标准 UPX 壳，产物进制品库（key=unpacked，带 parent）。先用 upx -t 校验，不认就不硬解；不支持 --force，改过头的样本请走动态 dump。只读样本，不执行样本。",
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

fn run(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let raw = args
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(raw) = raw else {
        return ToolResult::fail("缺少 path");
    };
    let backend = match super::require("upx") {
        Ok(status) => status,
        Err(error) => return ToolResult::fail(error),
    };
    let program = match backend.path.clone() {
        Some(path) => path,
        None => return ToolResult::fail("UPX 状态是 ready，但没有路径：点「重新检测」再试"),
    };
    let job = match job_dir("upx") {
        Ok(job) => job,
        Err(error) => return ToolResult::fail(error),
    };
    let sample = match stage(ctx, raw, &job) {
        Ok(sample) => sample,
        Err(error) => return ToolResult::fail(error),
    };

    // 1. 校验：UPX 自己的判断比我们的特征匹配可靠。
    let test = exec::run(
        &ExecSpec::new(&program)
            .arg("-t")
            .arg(sample.path.display().to_string())
            .cwd(job.path())
            .timeout(Duration::from_secs(30)),
    );
    let test = match test {
        Ok(output) => output,
        Err(error) => return ToolResult::fail(error),
    };
    if test.timed_out {
        return ToolResult::fail(
            "UPX 校验超时（30s）：样本可能卡在异常结构上。作业目录已清理，样本没有被改动。",
        );
    }
    if !test.ok() {
        return ToolResult::fail(refusal(&test, "UPX 不认这份文件"));
    }

    // 2. 解压：输出只落在作业目录里。
    let out_path = job.out().join(&sample.out_name);
    let unpack = exec::run(
        &ExecSpec::new(&program)
            .arg("-d")
            .arg("-o")
            .arg(out_path.display().to_string())
            .arg(sample.path.display().to_string())
            .cwd(job.path())
            .timeout(UNPACK_TIMEOUT),
    );
    let unpack = match unpack {
        Ok(output) => output,
        Err(error) => return ToolResult::fail(error),
    };
    if unpack.timed_out {
        return ToolResult::fail(format!(
            "UPX 解压超时（{}s）：{}\n样本较大或磁盘很慢；作业目录已清理，样本没有被改动。",
            UNPACK_TIMEOUT.as_secs(),
            unpack.tail_text(200)
        ));
    }
    if !unpack.ok() {
        return ToolResult::fail(format!(
            "UPX 解压失败（退出码 {}）：{}\n样本目录没有被写入（输出只在临时作业目录）。",
            unpack.code.unwrap_or(-1),
            unpack.tail_text(300)
        ));
    }

    // 3. 读产物。
    let size = match std::fs::metadata(&out_path) {
        Ok(meta) if meta.is_file() => meta.len(),
        Ok(_) => {
            return ToolResult::fail(format!(
                "UPX 说解好了，但 {} 不是文件",
                out_path.display()
            ))
        }
        Err(error) => {
            return ToolResult::fail(format!(
                "UPX 说解好了，但读不到产物（{}）：{error}",
                out_path.display()
            ))
        }
    };
    if size == 0 {
        return ToolResult::fail("UPX 产出的是空文件：这份样本大概不是标准 UPX");
    }
    if size > MAX_UNPACKED {
        return ToolResult::fail(format!(
            "脱壳产物 {:.1} MiB，超过 {} MiB 上限，没有读取。",
            size as f64 / (1024.0 * 1024.0),
            MAX_UNPACKED / (1024 * 1024)
        ));
    }
    let bytes = match std::fs::read(&out_path) {
        Ok(bytes) => bytes,
        Err(error) => return ToolResult::fail(format!("读脱壳产物失败：{error}")),
    };

    // 4. 交付：DAG 里进制品库，聊天里落 Prism 自己的目录。
    let sha = sha256_of(&bytes);
    let parent = parent_of(ctx, &sample, &sha, size as u64);
    let mut payload = json!({
        "ok": true,
        "source": {
            "input": raw,
            "path": sample.origin_path.as_ref().map(|path| path.display().to_string()),
            "size": sample.size,
            "sha256": sample.sha256,
        },
        "unpacked": {
            "name": sample.out_name,
            "size": size,
            "sha256": sha,
            "ratio": format!("{:.1}%", size as f64 / sample.size.max(1) as f64 * 100.0),
        },
        "upx": {
            "version": backend.version,
            "path": program.display().to_string(),
            "test": one_line(&test.stdout_text(), &test.stderr_text()),
            "log": one_line(&unpack.stdout_text(), &unpack.stderr_text()),
            "logTruncated": unpack.stdout_truncated || unpack.stderr_truncated,
            "elapsedMs": unpack.duration.as_millis() as u64,
        },
        "wrote": "作业目录（读完即删）",
        "note": "产物是 UPX 自解压后的原始映像；若样本改过头，UPX 会拒绝而不是硬解。",
    });

    match ctx.dag {
        Some(scope) => match ArtifactStore::global() {
            Ok(store) => {
                let put = store.put(PutRequest {
                    dag_id: &scope.dag_id,
                    node_id: &scope.node_id,
                    key: "unpacked",
                    name: &sample.out_name,
                    kind: "binary",
                    parent_id: parent.as_deref(),
                    bytes: &bytes,
                });
                match put {
                    Ok(reference) => {
                        payload["artifact"] = json!(reference.id);
                        payload["wrote"] = json!(format!(
                            "制品库（key=unpacked，{} 字节）",
                            reference.size
                        ));
                        if parent.is_some() {
                            payload["parent"] = json!(parent);
                        }
                    }
                    Err(error) => {
                        // 制品写不进去（配额/磁盘）也不能把唯一产物扔了：
                        // 退回聊天模式的落点，至少让人拿得到文件。
                        payload["artifactError"] = json!(error);
                        if let Ok(path) = keep_unpacked(&sample.out_name, &sha, &bytes) {
                            payload["file"] = json!(path.display().to_string());
                            payload["wrote"] = json!("~/.prism/unpacked（制品写入失败后的兜底）");
                        }
                    }
                }
            }
            Err(error) => payload["artifactError"] = json!(error),
        },
        None => match keep_unpacked(&sample.out_name, &sha, &bytes) {
            Ok(path) => payload["file"] = json!(path.display().to_string()),
            Err(error) => payload["fileError"] = json!(error),
        },
    }

    ToolResult::plain(ToolOutcome::ok(
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()),
    ))
}

/// 一句人话：优先 stderr，其次 stdout，压成单行并截断。
fn one_line(stdout: &str, stderr: &str) -> String {
    let source = if stderr.trim().is_empty() { stdout } else { stderr };
    let text: String = source
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    text.chars().take(300).collect()
}

/// UPX 拒绝时的文案：给它的原话，再说清 Prism 不硬解。
fn refusal(output: &exec::ExecOutput, prefix: &str) -> String {
    let tail = output.tail_text(400);
    let hint = if tail.to_ascii_lowercase().contains("modified")
        || tail.to_ascii_lowercase().contains("hacked")
        || tail.to_ascii_lowercase().contains("protected")
    {
        "这份样本被改过头（UPX 原话里提到 modified/hacked/protected）。Prism 不提供 --force：硬解出来的东西看着像成功，实际跑不起来。"
    } else {
        "它可能不是 UPX 壳、版本不匹配，或者只是被别的工具改过。先用 bin.pack.detect 确认壳型。"
    };
    format!(
        "{prefix}（退出码 {}）：{tail}\n{hint}",
        output.code.unwrap_or(-1)
    )
}

/// 样本在磁盘上的位置、原名、大小与哈希。
struct Sample {
    /// UPX 真正读的那个文件（可能是作业目录里的副本）。
    path: PathBuf,
    /// 用户给的那个路径（制品输入时为空）。
    origin_path: Option<PathBuf>,
    /// 产物文件名（`<原名>.unpacked`）。
    out_name: String,
    size: u64,
    sha256: String,
    /// 输入来自哪个制品（`artifact:<id>`）。
    artifact: Option<String>,
}

/// 把输入准备成"UPX 能读的一个文件"。
///
/// `artifact:<id>` 必须先落到作业目录：UPX 只认路径，而制品库不该被外部
/// 程序直接打开（它按 id 寻址，不是按路径）。
fn stage(ctx: &ToolContext<'_>, raw: &str, job: &super::JobDir) -> Result<Sample, String> {
    if let Some(id) = raw.strip_prefix("artifact:") {
        let scope = ctx
            .dag
            .ok_or_else(|| "读制品需要 DAG 作用域；在聊天里请改用文件路径".to_string())?;
        let store = ArtifactStore::global()?;
        let reference = store.stat(&scope.dag_id, id.trim())?;
        if reference.size > MAX_UNPACKED {
            return Err(format!(
                "制品 {} 有 {:.1} MiB，超过 {} MiB 上限",
                reference.id,
                reference.size as f64 / (1024.0 * 1024.0),
                MAX_UNPACKED / (1024 * 1024)
            ));
        }
        let (_, bytes) = store.read(&scope.dag_id, &reference.id)?;
        let path = job.input().join("sample.bin");
        std::fs::write(&path, &bytes).map_err(|error| format!("写临时样本失败：{error}"))?;
        Ok(Sample {
            path,
            origin_path: None,
            out_name: format!("{}.unpacked", sanitize(&reference.name)),
            size: bytes.len() as u64,
            sha256: reference.sha256.clone(),
            artifact: Some(reference.id),
        })
    } else {
        let (path, size) = target_of(ctx, &json!({ "path": raw }))
            .map_err(|failure| failure.outcome.text)?;
        // 词法归一挡不住符号链接：`ln -s /etc/passwd x` 在词法上仍在工作目录里。
        // 这个工具会把文件交给外部程序处理，所以在这里多走一次真实路径校验。
        let real = std::fs::canonicalize(&path)
            .map_err(|error| format!("读不到 {raw}：{error}"))?;
        let root = std::fs::canonicalize(ctx.directory)
            .unwrap_or_else(|_| PathBuf::from(ctx.directory));
        if !real.starts_with(&root) {
            return Err(format!("路径越出工作目录：{raw}（真实路径 {}）", real.display()));
        }
        // 只读文件头算哈希不够：parent 要按整份比。上限之内才读全量。
        let sha256 = if size <= MAX_UNPACKED {
            match std::fs::read(&real) {
                Ok(bytes) => sha256_of(&bytes),
                Err(error) => return Err(format!("读不到 {raw}：{error}")),
            }
        } else {
            // 超大样本也要能脱壳，只是没法给出整份哈希（UPX 自己会校验）。
            let head = read_head(&real, 1024 * 1024)?;
            format!("{}-partial", sha256_of(&head))
        };
        let name = real
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "sample.bin".to_string());
        Ok(Sample {
            // 交给 UPX 的是真实路径：校验完到真正打开之间，符号链接可能被换掉。
            path: real,
            origin_path: Some(path),
            out_name: format!("{}.unpacked", sanitize(&name)),
            size,
            sha256,
            artifact: None,
        })
    }
}

/// 产物该挂在谁下面：显式给的制品 id，或同一 DAG 里哈希相同的那份。
fn parent_of(ctx: &ToolContext<'_>, sample: &Sample, sha: &str, size: u64) -> Option<String> {
    if let Some(id) = &sample.artifact {
        return Some(id.clone());
    }
    let scope = ctx.dag?;
    let store = ArtifactStore::global().ok()?;
    store
        .list(&scope.dag_id)
        .into_iter()
        .find(|item| item.sha256 == sha && item.size == size)
        .map(|item| item.id)
}

/// 聊天模式：产物落 `~/.prism/unpacked/`，留最近 [`KEEP_UNPACKED`] 个。
///
/// 为什么不丢临时目录：`upx -d` 的产物是这次调用的**唯一交付物**，
/// 扔了等于什么都没做。放 Prism 自己的目录，用户找得到，也不污染样本目录。
fn keep_unpacked(name: &str, sha: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let dir = crate::engines::opencode::search::prism_dir()
        .ok_or_else(|| "找不到 ~/.prism 目录".to_string())?
        .join("unpacked");
    std::fs::create_dir_all(&dir).map_err(|error| format!("建目录失败：{error}"))?;
    let path = dir.join(format!("{}-{name}", &sha[..sha.len().min(12)]));
    std::fs::write(&path, bytes).map_err(|error| format!("写产物失败：{error}"))?;
    prune_dir(&dir, KEEP_UNPACKED);
    Ok(path)
}

/// 文件名只留安全字符：`-o` 拿到的是路径，名字里的 `/` 会改变落点。
fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('.');
    if trimmed.is_empty() {
        "sample.bin".to_string()
    } else {
        trimmed.chars().take(120).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::native::tools::{EnabledTools, ToolContext};
    use serde_json::json;

    #[test]
    fn tool_is_registered_with_upx_need() {
        let tools = tools();
        assert_eq!(tools.len(), 1);
        let tool = &tools[0];
        assert_eq!(tool.name.as_ref(), "bin.unpack.upx");
        assert_eq!(tool.permission, Permission::ReadOnly);
        assert_eq!(tool.meta.risk, Risk::Read);
        assert!(matches!(tool.meta.needs, [Need::Backend("upx")]));
        // 名字里有 `/` 会改变 `-o` 的落点。
        assert_eq!(sanitize("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(sanitize("a b.exe"), "a_b.exe");
        assert_eq!(sanitize(""), "sample.bin");
    }

    #[test]
    fn refusal_points_at_force_and_detect() {
        let output = exec::ExecOutput {
            code: Some(2),
            stderr: b"CantUnpackException: file is modified/hacked/protected; use --force to override\n"
                .to_vec(),
            ..exec::ExecOutput::default()
        };
        let text = refusal(&output, "UPX 不认这份文件");
        assert!(text.contains("modified/hacked/protected"), "{text}");
        assert!(text.contains("不提供 --force"), "{text}");

        // 不是"改过头"的情况：引导去确认壳型，而不是空口说 --force。
        let other = exec::ExecOutput {
            code: Some(1),
            stderr: b"upx: sample.exe: NotPackedException: not packed by UPX\n".to_vec(),
            ..exec::ExecOutput::default()
        };
        let text = refusal(&other, "UPX 不认这份文件");
        assert!(text.contains("bin.pack.detect"), "{text}");
        assert!(!text.contains("不提供 --force"), "{text}");
    }

    #[test]
    fn one_line_flattens_progress_output() {
        let text = one_line("Unpacked 1 file.\n", "  File: a.exe\n");
        assert_eq!(text, "File: a.exe");
        assert!(!text.contains('\n'));
    }

    /// 用假后端跑完整条链路：探测（env 覆盖）→ `-t` → `-d` → 读产物。
    ///
    /// 真 UPX 只在 `#[ignore]` 的回归里用：CI 与开发机不一定装了它。
    /// 这里要验的是**我们这一侧**的接线，不是 UPX 的行为。
    #[test]
    fn unpacks_through_a_fake_backend() {
        let root = std::env::temp_dir().join(format!("prism-upx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("建临时目录");

        // 假 upx：认 `--version` / `-t` / `-d -o <out> <in>`，产物固定四个字节。
        let fake = root.join("upx");
        std::fs::write(
            &fake,
            "#!/bin/sh\n\
             case \"$1\" in\n\
             --version) echo 'upx 4.2.4' ;;\n\
             -t) echo 'tested' ;;\n\
             -d) printf 'PACK' > \"$3\" ;;\n\
             *) exit 9 ;;\n\
             esac\n",
        )
        .expect("写假后端");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))
                .expect("加执行位");
        }

        let sample = root.join("packed.bin");
        std::fs::write(&sample, b"not really packed").expect("写样本");

        let key = "PRISM_UPX";
        let previous = std::env::var_os(key);
        std::env::set_var(key, &fake);
        // 探测缓存是进程级的：先清掉可能存在的旧结论。
        super::super::refresh("upx");
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
        let result = run(&ctx, &json!({ "path": "packed.bin" }));
        match previous {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
        super::super::refresh("upx");

        assert!(result.outcome.error.is_none(), "{}", result.outcome.text);
        let payload: Value = serde_json::from_str(&result.outcome.text).expect("结果是 JSON");
        assert_eq!(payload["unpacked"]["size"], 4);
        assert_eq!(payload["unpacked"]["sha256"], sha256_of(b"PACK"));
        // 聊天模式：产物落在 Prism 自己的目录里，且真的存在。
        let file = payload["file"].as_str().expect("应给出产物路径");
        assert_eq!(std::fs::read(file).expect("读产物"), b"PACK");
        let _ = std::fs::remove_file(file);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 真文件回归：`PRISM_TEST_UPX_SAMPLE=/path/to/packed.bin cargo test -- --ignored`
    #[test]
    #[ignore = "需要本机装 upx 和一个真 UPX 样本（PRISM_TEST_UPX_SAMPLE）"]
    fn unpacks_a_real_sample() {
        let Ok(sample) = std::env::var("PRISM_TEST_UPX_SAMPLE") else {
            panic!("先设 PRISM_TEST_UPX_SAMPLE 指向一个 UPX 样本");
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
        let before = std::fs::read_dir(dir).expect("列样本目录").count();
        let result = run(&ctx, &json!({ "path": name }));
        assert!(result.outcome.error.is_none(), "{}", result.outcome.text);
        let payload: Value = serde_json::from_str(&result.outcome.text).expect("结果是 JSON");
        let file = payload["file"].as_str().expect("应给出产物路径");
        let bytes = std::fs::read(file).expect("读产物");
        assert!(bytes.len() as u64 > payload["source"]["size"].as_u64().unwrap_or(0) / 2);
        // 样本目录一个字节都不该变。
        assert_eq!(std::fs::read_dir(dir).expect("列样本目录").count(), before);
        let _ = std::fs::remove_file(file);
    }
}
