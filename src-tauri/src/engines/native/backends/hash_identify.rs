//! `bin.crypto.hash_identify`：在样本里找哈希算法的常量，再归到函数、看有没有循环。
//!
//! 和 `bin.crypto.scan` 的分工：
//!
//! - `scan` 报"样本里有 SHA-256 常量"，给偏移；
//! - `hash_identify` 报"常量落在哪个函数、这个函数有没有回边"，并明确写清
//!   **没有**做指令级确认。
//!
//! 两阶段（`docs/BIN_TOOLCHAIN.md` §6.2）：
//!
//! | 阶段 | 依赖 | 判据 | 置信度 |
//! |---|---|---|---|
//! | P1 常量 | 无（共用 `analysis::SIGNATURES`） | IV / K 表 / CRC32 多项式 | high |
//! | P2 循环确认 | IR + blocks | 常量所在函数有回边 | 仍停在 high |
//!
//! 为什么 P2 不给 `certain`：确认"64/80 轮、4 个 32 位状态变量、消息扩展"
//! 需要指令级解码，当前 IR 只有函数边界与基本块。把"有回边"说成"确认是
//! SHA-256 实现"就是编证据。`rounds` 与 `stateVariables` 一律留空，等真正
//! 的指令级 IR 接进来再填。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};

use super::index::{FunctionEntry, ProgramIndex};
use super::ir;
use crate::dag::artifact::{ArtifactStore, PutRequest};
use crate::engines::native::analysis::{find_all, map_offset, SIGNATURES};
use crate::engines::native::forensics::{read_head, target_of};
use crate::engines::native::tools::{
    builtin_full, Network, Permission, Risk, ToolContext, ToolDef, ToolMeta, ToolOutcome, ToolResult,
    ToolScope,
};

/// 和 `bin.crypto.scan` 一样只扫前 64 MiB：再往后多半是资源/附加数据。
const SCAN_LIMIT: u64 = 64 * 1024 * 1024;
/// 每个签名最多留几处命中：报 100 处相同常量没有信息量。
const HITS_PER_SIGNATURE: usize = 4;
/// 循环候选最多列几个：全列出来只会淹没结论。
const MAX_LOOP_CANDIDATES: usize = 20;

pub fn tools() -> Vec<Arc<ToolDef>> {
    vec![builtin_full(
        "bin.crypto.hash_identify",
        "哈希算法识别",
        schema,
        Permission::ReadOnly,
        None,
        None,
        "",
        true,
        ToolMeta {
            tags: &["bin:static", "bin:crypto", "bin:ir"],
            risk: Risk::Read,
            network: Network::None,
            sandbox: None,
            scope: ToolScope::Any,
            // 后端由 `ir::index_of` 在运行时决定：DEX 走内置解析器，
            // 其他格式需要 Ghidra / radare2。这里不写死 needs，否则 DEX 也被挡掉。
            needs: &[],
        },
        run,
    )]
}

fn schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bin.crypto.hash_identify",
            "description": "识别样本里的哈希算法：先找 MD5/SHA1/SHA256/SHA512/CRC32 等常量，再按 IR 把常量归到函数并检查基本块回边。只读样本、不执行样本。注意：没有做指令级轮数确认，confidence 最高 high，rounds 为空。",
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

fn run(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let raw = args
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(raw) = raw else {
        return ToolResult::fail("缺少 path");
    };

    // 先拿 IR：这是 B 桶的统一前置。P1 的常量扫描也在拿到 IR 之后做，
    // 这样失败时只报一次错，不会出现"常量扫了但没函数"的半截结果。
    let indexed = match ir::index_of(ctx, raw) {
        Ok(indexed) => indexed,
        Err(error) => return ToolResult::fail(error),
    };

    let (path, size) = match target_of(ctx, args) {
        Ok(found) => found,
        Err(failure) => return failure,
    };
    let bytes = match read_head(&path, SCAN_LIMIT) {
        Ok(bytes) => bytes,
        Err(error) => return ToolResult::fail(error),
    };

    let mut payload = analyze(&indexed.index, &bytes, size);
    payload["source"] = indexed.source.clone();
    payload["scanned"] = json!(bytes.len());
    if size > SCAN_LIMIT {
        payload["note"] = json!(format!(
            "{} 只扫了前 {} MiB（文件 {:.1} MiB）。",
            payload["note"].as_str().unwrap_or(""),
            SCAN_LIMIT / (1024 * 1024),
            size as f64 / (1024.0 * 1024.0)
        ));
    }

    // 聊天模式落缓存；DAG 里落制品。两处内容一致，都是完整 JSON。
    let body = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
    let cache = write_cache(&indexed.sample.sha256, &body);
    payload["wrote"] = json!(cache
        .as_ref()
        .map(|path| format!("缓存（{}）", path.display()))
        .unwrap_or_else(|| "缓存不可用".to_string()));

    if let Some(scope) = ctx.dag {
        match ArtifactStore::global() {
            Ok(store) => match store.put(PutRequest {
                dag_id: &scope.dag_id,
                node_id: &scope.node_id,
                key: "hashes",
                name: "hash_identify.json",
                kind: "json",
                parent_id: indexed.sample.artifact.as_deref(),
                bytes: body.as_bytes(),
            }) {
                Ok(reference) => {
                    payload["artifact"] = json!(reference.id);
                    payload["wrote"] =
                        json!(format!("制品库（key=hashes，{} 字节）", reference.size));
                }
                Err(error) => payload["artifactError"] = json!(error),
            },
            Err(error) => payload["artifactError"] = json!(error),
        }
    }

    ToolResult::plain(ToolOutcome::ok(
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()),
    ))
}

// ---------------------------------------------------------------- 分析

/// 一条常量命中，已经映射到段/VA。
struct Hit {
    signature: &'static str,
    offset: usize,
    note: &'static str,
    confidence: &'static str,
    section: Option<String>,
    va: Option<u64>,
}

/// 纯函数：给 IR 与文件字节，产出完整结论（单测直接跑这里）。
fn analyze(index: &ProgramIndex, bytes: &[u8], size: u64) -> Value {
    let loops = loop_candidates(index);
    let mut groups: BTreeMap<&'static str, Vec<Hit>> = BTreeMap::new();

    for (name, pattern, note, confidence) in SIGNATURES {
        if pattern.is_empty() {
            continue;
        }
        let (offsets, _) = find_all(bytes, pattern, HITS_PER_SIGNATURE);
        for offset in offsets {
            let (section, va) = if index.backend == "dex" {
                // DEX 的 va 就是 code_off（文件偏移），可以直接比。
                (None, Some(offset as u64))
            } else {
                map_offset(bytes, offset as u64)
            };
            groups.entry(algorithm_of(name)).or_default().push(Hit {
                signature: name,
                offset,
                note,
                confidence,
                section,
                va,
            });
        }
    }

    let algorithms: Vec<Value> = groups
        .iter()
        .map(|(algorithm, hits)| algorithm_json(index, &loops, algorithm, hits))
        .collect();

    let note = if algorithms.is_empty() {
        "没命中已知常量：可能用了查表/位运算内联实现，或样本被压缩/加密。\
         这不能证明没有哈希算法，只能说常量表没出现。"
            .to_string()
    } else {
        "P2 只做到基本块回边：没有指令级轮数 / 4 个状态变量 / 消息扩展确认，\
         所以 confidence 最高 high，rounds 与 stateVariables 一律留空。\
         常量在数据段（映射不到函数）时不会硬归给某个函数。"
            .to_string()
    };

    json!({
        "ok": true,
        "size": size,
        "backend": index.backend,
        "backendVersion": index.backend_version,
        "algorithms": algorithms,
        "loops": {
            "functionsWithBlocks": loops.with_blocks,
            "withBackEdges": loops.candidates.len(),
            "candidates": loops.candidates.iter().take(MAX_LOOP_CANDIDATES).map(|candidate| json!({
                "va": format!("0x{:x}", candidate.va),
                "function": candidate.name,
                "backEdges": candidate.back_edges,
                "blocks": candidate.blocks,
            })).collect::<Vec<_>>(),
        },
        "note": note,
    })
}

fn algorithm_json(
    index: &ProgramIndex,
    loops: &Loops,
    algorithm: &str,
    hits: &[Hit],
) -> Value {
    let base = hits
        .iter()
        .map(|hit| hit.confidence)
        .max_by_key(|confidence| rank(confidence))
        .unwrap_or("low");

    // 优先挑"落在带回边函数里"的命中；没有就挑第一个能映射到函数的；
    // 再没有就只报偏移。绝不按函数大小猜。
    let mut best_loop: Option<(usize, &LoopCandidate)> = None;
    let mut best_function: Option<usize> = None;
    for hit in hits {
        let Some(va) = hit.va else {
            continue;
        };
        let Some(function_index) = containing_function(index, va) else {
            continue;
        };
        if best_function.is_none() {
            best_function = Some(function_index);
        }
        if let Some(candidate) = loops
            .candidates
            .iter()
            .find(|candidate| candidate.function_index == function_index)
        {
            let better = best_loop
                .map(|(_, current)| candidate.back_edges > current.back_edges)
                .unwrap_or(true);
            if better {
                best_loop = Some((function_index, candidate));
            }
        }
    }

    let loop_confirmed = best_loop.is_some();
    let confidence = if loop_confirmed {
        // 常量 + 回边只能证明"像压缩循环"，到不了 certain。
        "high"
    } else {
        base
    };
    let attributed = best_loop
        .map(|(index, _)| index)
        .or(best_function);
    let (va, function, rounds, state_variables, evidence) =
        match attributed {
            Some(function_index) => {
                let function = &index.functions[function_index];
                let mut evidence: Vec<String> = Vec::new();
                for hit in hits.iter().take(3) {
                    evidence.push(format!(
                        "{} 常量 @ 0x{:x}{}",
                        hit.signature,
                        hit.offset,
                        hit.section
                            .as_ref()
                            .map(|section| format!("（{section}）"))
                            .unwrap_or_default()
                    ));
                }
                if let Some((_, candidate)) = best_loop {
                    evidence.push(format!(
                        "函数 {} @ 0x{:x} 有 {} 条回边 / {} 个基本块，像压缩循环",
                        candidate.name, candidate.va, candidate.back_edges, candidate.blocks
                    ));
                } else {
                    evidence.push(format!(
                        "常量落在函数 {} @ 0x{:x} 内，但这个函数没有回边",
                        function.name, function.va
                    ));
                }
                if hits.len() > 1 {
                    evidence.push(format!("同一算法共命中 {} 处常量", hits.len()));
                }
                (
                    Some(format!("0x{:x}", function.va)),
                    Some(function.name.clone()),
                    Value::Null,
                    Value::Null,
                    evidence,
                )
            }
            None => {
                let mut evidence: Vec<String> = hits
                    .iter()
                    .take(3)
                    .map(|hit| {
                        format!(
                            "{} 常量 @ 0x{:x}{}",
                            hit.signature,
                            hit.offset,
                            hit.section
                                .as_ref()
                                .map(|section| format!("（{section}）"))
                                .unwrap_or_default()
                        )
                    })
                    .collect();
                evidence.push(
                    "常量不在任何已知函数范围内（可能在数据段），未归到函数".to_string(),
                );
                (None, None, Value::Null, Value::Null, evidence)
            }
        };

    json!({
        "algorithm": algorithm,
        "confidence": confidence,
        "va": va,
        "function": function,
        "rounds": rounds,
        "stateVariables": state_variables,
        "loopConfirmed": loop_confirmed,
        "signatures": hits.iter().map(|hit| hit.signature).collect::<Vec<_>>(),
        "offsets": hits.iter().map(|hit| format!("0x{:x}", hit.offset)).collect::<Vec<_>>(),
        "note": hits.first().map(|hit| hit.note).unwrap_or(""),
        "evidence": evidence,
    })
}

/// 签名名 → 算法名。`sha512-iv-be` 与 `sha512-iv` 是同一个算法。
fn algorithm_of(signature: &str) -> &'static str {
    if signature.starts_with("sha512") {
        "sha512"
    } else if signature.starts_with("sha256") {
        "sha256"
    } else if signature.starts_with("sha1") {
        "sha1"
    } else if signature.starts_with("md5") {
        "md5"
    } else if signature.starts_with("crc32") {
        "crc32"
    } else if signature.starts_with("aes") {
        "aes"
    } else if signature.starts_with("des") {
        "des"
    } else if signature.starts_with("chacha") {
        "chacha20"
    } else {
        "unknown"
    }
}

fn rank(confidence: &str) -> u8 {
    match confidence {
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

// ---------------------------------------------------------------- 循环

struct LoopCandidate {
    function_index: usize,
    va: u64,
    name: String,
    back_edges: usize,
    blocks: usize,
}

struct Loops {
    with_blocks: usize,
    candidates: Vec<LoopCandidate>,
}

/// 回边 = 后继地址不高于当前块（含自环）。这是"有循环"的最低成本判据，
/// 也是当前 IR 能诚实支持的唯一判据。
fn back_edges(function: &FunctionEntry) -> Vec<(u64, u64)> {
    let Some(blocks) = function.blocks.as_deref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for block in blocks {
        for succ in &block.succ {
            if *succ <= block.va {
                out.push((block.va, *succ));
            }
        }
    }
    out
}

fn loop_candidates(index: &ProgramIndex) -> Loops {
    let mut with_blocks = 0;
    let mut candidates = Vec::new();
    for (function_index, function) in index.functions.iter().enumerate() {
        if function.blocks.is_some() {
            with_blocks += 1;
        }
        let edges = back_edges(function);
        if edges.is_empty() {
            continue;
        }
        candidates.push(LoopCandidate {
            function_index,
            va: function.va,
            name: function.name.clone(),
            back_edges: edges.len(),
            blocks: function.blocks.as_ref().map(Vec::len).unwrap_or(0),
        });
    }
    candidates.sort_by(|a, b| b.back_edges.cmp(&a.back_edges));
    Loops {
        with_blocks,
        candidates,
    }
}

fn containing_function(index: &ProgramIndex, va: u64) -> Option<usize> {
    // DEX 的 size 不含 code_item 头（16 字节），把范围放宽一点；
    // 其他格式多算 16 字节也无害（相邻函数起始地址优先由精确匹配命中）。
    index.functions.iter().position(|function| {
        va >= function.va && va < function.va.saturating_add(function.size as u64 + 16)
    })
}

// ---------------------------------------------------------------- 缓存

fn write_cache(sha256: &str, body: &str) -> Option<PathBuf> {
    let dir = super::index::cache_root()?.join(sha256);
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("hash_identify.json");
    std::fs::write(&path, body.as_bytes()).ok()?;
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::native::backends::index::{
        BlockEntry, FunctionEntry, ImportEntry, SampleRef, SCHEMA,
    };

    fn function(va: u64, name: &str, blocks: Option<Vec<BlockEntry>>) -> FunctionEntry {
        FunctionEntry {
            va,
            name: name.to_string(),
            size: 0x100,
            kind: "normal".to_string(),
            blocks,
        }
    }

    fn index(functions: Vec<FunctionEntry>) -> ProgramIndex {
        ProgramIndex {
            schema: SCHEMA,
            backend: "ghidra".to_string(),
            backend_version: "11.2.1".to_string(),
            sample: SampleRef {
                sha256: "a".repeat(64),
                size: 4096,
                format: "pe".to_string(),
                arch: Some("x86-64".to_string()),
                bits: Some(64),
                image_base: None,
            },
            functions,
            imports: Vec::<ImportEntry>::new(),
            truncated: false,
            note: None,
        }
    }

    #[test]
    fn signature_names_group_into_algorithms() {
        assert_eq!(algorithm_of("sha512-iv-be"), "sha512");
        assert_eq!(algorithm_of("sha512-k"), "sha512");
        assert_eq!(algorithm_of("sha256-iv"), "sha256");
        assert_eq!(algorithm_of("aes-inv-sbox"), "aes");
        assert_eq!(algorithm_of("chacha-sigma16"), "chacha20");
        assert_eq!(algorithm_of("something-new"), "unknown");
    }

    #[test]
    fn back_edges_include_self_loops_and_ignore_forward_edges() {
        let f = function(
            0x1000,
            "f",
            Some(vec![
                BlockEntry {
                    va: 0x1000,
                    size: 0x10,
                    succ: vec![0x1010],
                },
                BlockEntry {
                    va: 0x1010,
                    size: 0x10,
                    succ: vec![0x1000, 0x1020, 0x1010],
                },
                BlockEntry {
                    va: 0x1020,
                    size: 0x10,
                    succ: vec![],
                },
            ]),
        );
        let edges = back_edges(&f);
        assert_eq!(edges, vec![(0x1010, 0x1000), (0x1010, 0x1010)]);
        assert!(back_edges(&function(0x2000, "g", None)).is_empty());
    }

    #[test]
    fn loops_are_sorted_by_back_edge_count() {
        let index = index(vec![
            function(
                0x1000,
                "one",
                Some(vec![BlockEntry {
                    va: 0x1010,
                    size: 0x10,
                    succ: vec![0x1000],
                }]),
            ),
            function(
                0x2000,
                "two",
                Some(vec![BlockEntry {
                    va: 0x2010,
                    size: 0x10,
                    succ: vec![0x2000, 0x2010],
                }]),
            ),
        ]);
        let loops = loop_candidates(&index);
        assert_eq!(loops.with_blocks, 2);
        assert_eq!(loops.candidates.len(), 2);
        assert_eq!(loops.candidates[0].name, "two");
        assert_eq!(loops.candidates[0].back_edges, 2);
    }

    #[test]
    fn constants_are_attributed_only_when_mapped_into_a_function() {
        // 两个 SHA-512 IV（小端）放在文件 0x200；IR 里没有段映射（不是 PE/ELF），
        // 所以只能报偏移，不能硬说属于哪个函数。
        let mut bytes = vec![0u8; 0x400];
        let iv: &[u8] = &[
            0x08, 0xc9, 0xbc, 0xf3, 0x67, 0xe6, 0x09, 0x6a, 0x3b, 0xa7, 0xca, 0x84, 0x85, 0xae,
            0x67, 0xbb,
        ];
        bytes[0x200..0x200 + iv.len()].copy_from_slice(iv);
        let index = index(vec![function(0x1000, "hash", None)]);
        let payload = analyze(&index, &bytes, bytes.len() as u64);
        let algorithms = payload["algorithms"].as_array().expect("algorithms");
        let sha512 = algorithms
            .iter()
            .find(|item| item["algorithm"] == "sha512")
            .expect("应识别出 SHA-512");
        assert_eq!(sha512["va"], Value::Null);
        assert_eq!(sha512["loopConfirmed"], false);
        assert_eq!(sha512["rounds"], Value::Null);
        assert_eq!(sha512["confidence"], "high");
        assert!(sha512["offsets"][0].as_str().unwrap_or("").starts_with("0x200"));
    }

    #[test]
    fn pe_offsets_map_into_functions_and_confirm_loops() {
        // 最小 PE：一个 .text 段（文件 0x200 → VA 0x1000）。
        let mut bytes = vec![0u8; 0x400];
        bytes[0..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
        // COFF：1 个段，可选头 0xE0。
        bytes[0x86..0x88].copy_from_slice(&1u16.to_le_bytes());
        bytes[0x84 + 16..0x84 + 18].copy_from_slice(&0xE0u16.to_le_bytes());
        let table = 0x84 + 20 + 0xE0;
        bytes[table..table + 5].copy_from_slice(b".text");
        bytes[table + 8..table + 12].copy_from_slice(&0x100u32.to_le_bytes());
        bytes[table + 12..table + 16].copy_from_slice(&0x1000u32.to_le_bytes());
        bytes[table + 16..table + 20].copy_from_slice(&0x100u32.to_le_bytes());
        bytes[table + 20..table + 24].copy_from_slice(&0x200u32.to_le_bytes());

        let iv: &[u8] = &[
            0x08, 0xc9, 0xbc, 0xf3, 0x67, 0xe6, 0x09, 0x6a, 0x3b, 0xa7, 0xca, 0x84, 0x85, 0xae,
            0x67, 0xbb,
        ];
        bytes[0x200..0x200 + iv.len()].copy_from_slice(iv);

        let index = index(vec![function(
            0x1000,
            "sha512_compress",
            Some(vec![BlockEntry {
                va: 0x1000,
                size: 0x20,
                succ: vec![0x1000],
            }]),
        )]);
        let payload = analyze(&index, &bytes, bytes.len() as u64);
        let sha512 = payload["algorithms"]
            .as_array()
            .expect("algorithms")
            .iter()
            .find(|item| item["algorithm"] == "sha512")
            .expect("应识别出 SHA-512");
        assert_eq!(sha512["va"], "0x1000");
        assert_eq!(sha512["function"], "sha512_compress");
        assert_eq!(sha512["loopConfirmed"], true);
        assert_eq!(sha512["confidence"], "high");
        assert_eq!(sha512["rounds"], Value::Null);
        assert!(sha512["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line.as_str().unwrap_or("").contains("回边")));
    }

    #[test]
    fn tool_is_registered_and_read_only() {
        let tools = tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name.as_ref(), "bin.crypto.hash_identify");
        assert_eq!(tools[0].permission, Permission::ReadOnly);
        assert_eq!(tools[0].meta.risk, Risk::Read);
        assert!(tools[0].meta.needs.is_empty());
    }
}
