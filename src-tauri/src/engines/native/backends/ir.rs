//! IR 获取层：`bin.cfg.extract` / `bin.crypto.hash_identify` / `bin.vm.detect` 共用。
//!
//! 三件事：
//!
//! 1. **一份样本只 stage 一次**：路径输入不复制，制品输入落到作业目录；
//! 2. **后端优先级固定**：DEX 走内置解析器；其余 Ghidra → radare2 → 明确报错；
//! 3. **radare2 的 blocks 按需补**：`aflj` 不给基本块，下游要画图时跑一次
//!    `afbj` detail pass，结果写回同一份缓存。
//!
//! 这一层不产生工具，只给下游四个工具提供 `Indexed`。

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{json, Value};

use super::exec::{self, ExecSpec};
use super::probe::BackendState;
use super::{ghidra, index, job_dir, radare2, sample_ref, stage_sample, JobDir, StagedSample};
use crate::engines::native::dex;
use crate::engines::native::forensics::{identify, read_head};
use crate::engines::native::tools::ToolContext;

/// 制品输入复制到作业目录的上限。
const MAX_STAGED: u64 = 1024 * 1024 * 1024;
/// DEX 整份读进内存的上限（解析器要随机访问）。
const MAX_DEX: u64 = 256 * 1024 * 1024;
/// 一次 detail pass 最多补多少个函数：r2 一个进程跑完，但命令太长也没意义。
const DETAIL_LIMIT: usize = 200;
const DETAIL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
const DETAIL_STDOUT_CAP: usize = 8 * 1024 * 1024;
const DETAIL_STDERR_CAP: usize = 512 * 1024;

/// 一份已经拿到的 IR + 它对应的样本。
pub struct Indexed {
    pub index: index::ProgramIndex,
    pub source: Value,
    pub job: JobDir,
    pub sample: StagedSample,
}

impl Indexed {
    pub fn cache_path(&self) -> Option<PathBuf> {
        index::cache_root().map(|root| root.join(&self.sample.sha256).join("index.json"))
    }

    /// 把补过 blocks 的 IR 写回缓存；失败不算工具失败。
    pub fn save(&self) -> Result<PathBuf, String> {
        index::save(&self.index)
    }
}

/// 取一份 IR：DEX 内置解析，其余 Ghidra 优先、radare2 兜底。
pub fn index_of(ctx: &ToolContext<'_>, raw: &str) -> Result<Indexed, String> {
    let job = job_dir("downstream")?;
    let sample = stage_sample(ctx, raw, &job, MAX_STAGED)?;
    let head = read_head(&sample.path, 4096).unwrap_or_default();
    let meta = identify(&head, sample.size);
    let source = source_json(raw, &sample, &meta);

    match meta["format"].as_str().unwrap_or("unknown") {
        "dex" => {
            let index = dex_index(&sample, &meta)?;
            Ok(Indexed {
                index,
                source,
                job,
                sample,
            })
        }
        "apk" => Err(
            "APK 里的 classes.dex 还没接：先把 classes.dex 取出来\
             （unzip -p sample.apk classes.dex > classes.dex），再对 .dex 跑这个工具。"
                .to_string(),
        ),
        _ => {
            if ready("ghidra") {
                let index = ghidra::ensure_index_from(ctx, &job, &sample, &meta)?;
                return Ok(Indexed {
                    index,
                    source,
                    job,
                    sample,
                });
            }
            if ready("radare2") {
                let index = radare2::ensure_index_from(ctx, &job, &sample, &meta)?;
                return Ok(Indexed {
                    index,
                    source,
                    job,
                    sample,
                });
            }
            Err(
                "这个工具需要函数边界：先跑 bin.ghidra.analyze 或 bin.radare2.scan\
                 （DEX 样本不用外部程序，自动用内置解析器）。本机两个后端都没接上：\
                 装 Ghidra 或 radare2 后点「重新检测」。"
                    .to_string(),
            )
        }
    }
}

fn ready(id: &str) -> bool {
    super::status(id)
        .map(|status| status.state == BackendState::Ready)
        .unwrap_or(false)
}

fn dex_index(sample: &StagedSample, meta: &Value) -> Result<index::ProgramIndex, String> {
    let version = index::dex_backend_version();
    if let Some(found) = index::load(&sample.sha256, "dex", &version) {
        return Ok(found);
    }
    if sample.size > MAX_DEX {
        return Err(format!(
            "DEX 有 {:.1} MiB，超过 {} MiB 上限，没有整份读入。",
            sample.size as f64 / (1024.0 * 1024.0),
            MAX_DEX / (1024 * 1024)
        ));
    }
    let bytes = std::fs::read(&sample.path)
        .map_err(|error| format!("读 DEX 失败（{}）：{error}", sample.path.display()))?;
    let found = dex::methods(&bytes)?;
    let parsed = index::from_dex_methods(
        found.methods,
        sample_ref(sample, meta),
        found.skipped_classes,
    );
    // 缓存写不进去不影响本次结果；下次重新解析即可。
    if index::save(&parsed).is_ok() {
        index::prune_cache(index::CACHE_LIMIT);
    }
    Ok(parsed)
}

pub fn source_json(raw: &str, sample: &StagedSample, meta: &Value) -> Value {
    json!({
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
    })
}

// ---------------------------------------------------------------- detail pass

#[derive(Debug, Deserialize)]
struct R2Block {
    #[serde(default)]
    addr: u64,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    jump: Option<u64>,
    #[serde(default)]
    fail: Option<u64>,
}

/// 给选中的函数补基本块（只支持 radare2；Ghidra 的导出里已经有）。
///
/// 返回实际补上的函数数。失败是硬错误：下游画图不能拿"没有 blocks"冒充"没有边"。
pub fn detail_blocks(indexed: &mut Indexed, targets: &[usize]) -> Result<usize, String> {
    if indexed.index.backend != "radare2" {
        return Ok(0);
    }
    let targets: Vec<usize> = targets.iter().copied().take(DETAIL_LIMIT).collect();
    if targets.is_empty() {
        return Ok(0);
    }

    let backend = super::require("radare2")?;
    let program = backend
        .path
        .clone()
        .ok_or_else(|| "radare2 状态是 ready，但没有路径：点「重新检测」再试".to_string())?;

    let mut args = vec![
        "-q".to_string(),
        "-e".to_string(),
        "scr.color=0".to_string(),
        "-c".to_string(),
        "aaa".to_string(),
    ];
    for index in &targets {
        let function = &indexed.index.functions[*index];
        args.push("-c".to_string());
        args.push(format!("?e PRISM_FN {}", function.va));
        args.push("-c".to_string());
        args.push(format!("afbj @ 0x{:x}", function.va));
    }
    args.push(indexed.sample.path.display().to_string());

    let mut spec = ExecSpec::new(&program)
        .args(args)
        .cwd(indexed.job.path())
        .timeout(DETAIL_TIMEOUT);
    spec.stdout_cap = DETAIL_STDOUT_CAP;
    spec.stderr_cap = DETAIL_STDERR_CAP;
    let output = exec::run(&spec)?;
    if output.timed_out {
        return Err(format!(
            "radare2 基本块 detail pass 超时（{}s）：函数太多或样本太大。\
             可以指定 function 只补一个函数。",
            DETAIL_TIMEOUT.as_secs()
        ));
    }
    if !output.ok() {
        return Err(format!(
            "radare2 基本块 detail pass 失败（退出码 {}）：{}",
            output.code.unwrap_or(-1),
            output.tail_text(400)
        ));
    }

    let parsed = parse_detail(&output.stdout_text())?;
    let mut updated = 0;
    for index in targets {
        if let Some(blocks) = parsed.get(&indexed.index.functions[index].va) {
            if !blocks.is_empty() {
                indexed.index.functions[index].blocks = Some(blocks.clone());
                updated += 1;
            }
        }
    }
    if updated == 0 {
        return Err(
            "radare2 没有导出任何基本块（afbj 输出为空）。函数边界可能来自缓存，\
             但 r2 当前认不出这些函数；重新跑 bin.radare2.scan 再试。"
                .to_string(),
        );
    }
    // 写回缓存；失败只影响下次是否要重跑 detail pass。
    let _ = indexed.save();
    Ok(updated)
}

/// 解析 `?e PRISM_FN <va>` + `afbj` 的拼接输出。
///
/// `afbj` 是 JSON 数组；不同 r2 版本可能压成一行也可能换行，所以按"能解析
/// 就收一段"处理，不假设一行一个 JSON。
fn parse_detail(stdout: &str) -> Result<std::collections::HashMap<u64, Vec<index::BlockEntry>>, String> {
    let mut out = std::collections::HashMap::new();
    let mut current: Option<u64> = None;
    let mut buffer = String::new();
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("PRISM_FN ") {
            if let Some(va) = current.take() {
                if let Some(blocks) = parse_blocks(&buffer) {
                    out.insert(va, blocks);
                }
                buffer.clear();
            }
            current = rest.trim().parse::<u64>().ok();
            continue;
        }
        if current.is_none() {
            continue;
        }
        if !buffer.is_empty() {
            buffer.push('\n');
        }
        buffer.push_str(line);
        if let Some(blocks) = parse_blocks(&buffer) {
            if let Some(va) = current.take() {
                out.insert(va, blocks);
            }
            buffer.clear();
        }
    }
    if let Some(va) = current {
        if let Some(blocks) = parse_blocks(&buffer) {
            out.insert(va, blocks);
        }
    }
    if out.is_empty() {
        return Err(format!(
            "radare2 的 afbj 输出不是合法 JSON。原文前 200 字符：{}",
            stdout.chars().take(200).collect::<String>()
        ));
    }
    Ok(out)
}

fn parse_blocks(text: &str) -> Option<Vec<index::BlockEntry>> {
    let blocks: Vec<R2Block> = serde_json::from_str(text.trim()).ok()?;
    Some(
        blocks
            .into_iter()
            .map(|block| {
                let mut succ = Vec::new();
                if let Some(jump) = block.jump.filter(|value| *value != 0) {
                    succ.push(jump);
                }
                if let Some(fail) = block.fail.filter(|value| *value != 0) {
                    if !succ.contains(&fail) {
                        succ.push(fail);
                    }
                }
                index::BlockEntry {
                    va: block.addr,
                    size: block.size.min(u32::MAX as u64) as u32,
                    succ,
                }
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_concatenated_afbj_blocks() {
        let stdout = "PRISM_FN 4198400\n\
            [{\"addr\":4198400,\"size\":24,\"jump\":4198424,\"fail\":null},\
             {\"addr\":4198424,\"size\":8,\"jump\":null,\"fail\":null}]\n\
            PRISM_FN 4198448\n\
            [{\"addr\":4198448,\"size\":16,\"jump\":4198464,\"fail\":4198464}]\n";
        let parsed = parse_detail(stdout).expect("应能解析");
        assert_eq!(parsed.len(), 2);
        let first = &parsed[&4198400];
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].succ, vec![4198424]);
        assert!(first[1].succ.is_empty());
        // jump 与 fail 相同只留一条边。
        assert_eq!(parsed[&4198448][0].succ, vec![4198464]);
    }

    #[test]
    fn rejects_non_json_detail_output() {
        let error = parse_detail("PRISM_FN 4198400\nnot json\n").unwrap_err();
        assert!(error.contains("不是合法 JSON"), "{error}");
    }

    #[test]
    fn empty_detail_output_is_an_error() {
        assert!(parse_detail("").is_err());
    }
}
