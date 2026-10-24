//! `bin.vm.detect`：从基本块形状里找"虚机保护"的结构信号。
//!
//! 它**不报壳名**——那是 `bin.pack.detect` 的活。这里只回答一个问题：
//! 这段代码像不像"取指 → 查处理表 → 跳到 handler → 回分发器"的虚机循环。
//!
//! 当前 IR 只有函数边界与基本块，没有指令。所以只有两项强信号能可靠判：
//!
//! | 信号 | 判据 | 权重 |
//! |---|---|---|
//! | 分发器 | 单块 ≥ 8 个不同后继，且 ≥ 75% 的后继有边回到该块 | 强 |
//! | 处理表 | 单块 ≥ 16 个不同后继（跳转表形状） | 强 |
//! | 上下文 | 多个 handler 读写同一结构体偏移 | 未判定（要指令） |
//! | 反调试 | 导入 `IsDebuggerPresent` 等；`rdtsc`/`cpuid` 要指令 | 中（旁证） |
//!
//! 两项强信号 → `medium`；一项 → `low`，并写明"可能是普通状态机（如解析器）"。
//! 三项全中才 `high`，但第三项当前判不了，所以**最高就是 medium**。
//! 误报代价很高：把状态机说成 VMProtect，会让人直接放弃静态分析。

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};

use super::index::ProgramIndex;
use super::ir;
use crate::dag::artifact::{ArtifactStore, PutRequest};
use crate::engines::native::tools::{
    builtin_full, Network, Permission, Risk, ToolContext, ToolDef, ToolMeta, ToolOutcome, ToolResult,
    ToolScope,
};

/// 分发器最少几个后继才算"多路"。
const DISPATCHER_SUCC: usize = 8;
/// 处理表最少多少项。
const HANDLER_TABLE: usize = 16;
/// 分发器的后继里至少这个比例要回到分发器头部，否则只算普通 switch。
const BACK_RATIO: usize = 75;
/// 候选最多列几个。
const MAX_CANDIDATES: usize = 20;

/// 反调试导入：只列"看到就值得记一笔"的名字，不把整个导入表搬进来。
const ANTI_DEBUG_IMPORTS: &[&str] = &[
    "IsDebuggerPresent",
    "CheckRemoteDebuggerPresent",
    "NtQueryInformationProcess",
    "OutputDebugStringA",
    "OutputDebugStringW",
    "ptrace",
    "IsProcessorFeaturePresent",
];

pub fn tools() -> Vec<Arc<ToolDef>> {
    vec![builtin_full(
        "bin.vm.detect",
        "虚机保护结构检测",
        schema,
        Permission::ReadOnly,
        None,
        None,
        "",
        true,
        ToolMeta {
            tags: &["bin:static", "bin:ir", "bin:vm"],
            risk: Risk::Read,
            network: Network::None,
            sandbox: None,
            scope: ToolScope::Any,
            needs: &[],
        },
        run,
    )]
}

fn schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bin.vm.detect",
            "description": "从基本块形状判断是否存在虚机保护（分发器 / 处理表）。只报结构，不报壳名；不执行样本。没有指令级信息，最高置信度 medium，context 与 rdtsc/cpuid 信号会如实标未判定。",
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

    let indexed = match ir::index_of(ctx, raw) {
        Ok(indexed) => indexed,
        Err(error) => return ToolResult::fail(error),
    };

    let mut payload = analyze(&indexed.index);
    payload["source"] = indexed.source.clone();

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
                key: "vm",
                name: "vm_detect.json",
                kind: "json",
                parent_id: indexed.sample.artifact.as_deref(),
                bytes: body.as_bytes(),
            }) {
                Ok(reference) => {
                    payload["artifact"] = json!(reference.id);
                    payload["wrote"] = json!(format!("制品库（key=vm，{} 字节）", reference.size));
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct Dispatcher {
    function_index: usize,
    function: String,
    va: u64,
    successors: usize,
    back: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HandlerTable {
    function_index: usize,
    function: String,
    va: u64,
    entries: usize,
}

/// 纯函数：给 IR 产出结论（单测直接跑这里）。
fn analyze(index: &ProgramIndex) -> Value {
    let mut dispatchers: Vec<Dispatcher> = Vec::new();
    let mut tables: Vec<HandlerTable> = Vec::new();
    let mut functions_with_blocks = 0usize;

    for (function_index, function) in index.functions.iter().enumerate() {
        let Some(blocks) = function.blocks.as_deref() else {
            continue;
        };
        if blocks.is_empty() {
            continue;
        }
        functions_with_blocks += 1;
        for block in blocks {
            let successors = distinct_successors(block);
            if successors.len() >= DISPATCHER_SUCC {
                let back = successors
                    .iter()
                    .filter(|succ| {
                        blocks
                            .iter()
                            .find(|candidate| candidate.va == **succ)
                            .map(|candidate| candidate.succ.contains(&block.va))
                            .unwrap_or(false)
                    })
                    .count();
                if back * 100 >= successors.len() * BACK_RATIO {
                    dispatchers.push(Dispatcher {
                        function_index,
                        function: function.name.clone(),
                        va: block.va,
                        successors: successors.len(),
                        back,
                    });
                }
            }
            if successors.len() >= HANDLER_TABLE {
                tables.push(HandlerTable {
                    function_index,
                    function: function.name.clone(),
                    va: block.va,
                    entries: successors.len(),
                });
            }
        }
    }

    let dispatcher_detected = !dispatchers.is_empty();
    let table_detected = !tables.is_empty();
    let strong = usize::from(dispatcher_detected) + usize::from(table_detected);

    // 两项强信号最好落在同一个函数里；分开出现时降一档，避免把两个
    // 不相干的 switch 拼成"虚机"。
    let same_function = dispatchers.iter().any(|dispatcher| {
        tables
            .iter()
            .any(|table| table.function_index == dispatcher.function_index)
    });
    let effective = if strong == 2 && !same_function { 1 } else { strong };

    let (verdict, confidence, note) = match effective {
        0 => (
            "none",
            "low",
            "没有看到分发器/处理表形状。这不能证明没有虚机保护：指令级分发、\
             被内联的 handler、或只有数据表没有跳转的实现都可能漏掉。壳名看 bin.pack.detect。",
        ),
        1 => (
            "possible",
            "low",
            "只中一项强信号，可能是普通状态机（如解析器、协议解包），也可能是虚机。\
             不要仅凭这一项下结论；先看 bin.pack.detect 与字符串/导入。",
        ),
        _ => (
            "suspected",
            "medium",
            "两项强信号都来自基本块形状，仍可能是复杂状态机；没有指令级确认，\
             所以不给 high。要确认 handler 上下文与 rdtsc/cpuid，需要指令级 IR。",
        ),
    };

    let anti_debug: Vec<String> = index
        .imports
        .iter()
        .filter(|import| {
            ANTI_DEBUG_IMPORTS
                .iter()
                .any(|name| import.name.eq_ignore_ascii_case(name))
        })
        .map(|import| import.name.clone())
        .collect();

    json!({
        "ok": true,
        "backend": index.backend,
        "backendVersion": index.backend_version,
        "verdict": verdict,
        "virtualized": effective >= 2,
        "confidence": confidence,
        "strongSignals": effective,
        "rawStrongSignals": strong,
        "signals": {
            "dispatcher": {
                "detected": dispatcher_detected,
                "count": dispatchers.len(),
                "candidates": dispatchers.iter().take(MAX_CANDIDATES).map(|dispatcher| json!({
                    "function": dispatcher.function,
                    "va": format!("0x{:x}", dispatcher.va),
                    "successors": dispatcher.successors,
                    "backEdges": dispatcher.back,
                })).collect::<Vec<_>>(),
            },
            "handlerTable": {
                "detected": table_detected,
                "count": tables.len(),
                "candidates": tables.iter().take(MAX_CANDIDATES).map(|table| json!({
                    "function": table.function,
                    "va": format!("0x{:x}", table.va),
                    "entries": table.entries,
                })).collect::<Vec<_>>(),
            },
            "context": {
                "detected": Value::Null,
                "note": "未判定：需要指令级信息（handler 读写的结构体偏移），当前 IR 只有基本块。",
            },
            "antiDebug": {
                "detected": !anti_debug.is_empty(),
                "imports": anti_debug,
                "note": "只看了导入表。rdtsc 差值、cpuid 叶子检查需要指令级信息，未判定。",
            },
        },
        "functionsWithBlocks": functions_with_blocks,
        "note": note,
        "sameFunction": same_function,
    })
}

fn distinct_successors(block: &super::index::BlockEntry) -> Vec<u64> {
    let mut out: Vec<u64> = Vec::new();
    for succ in &block.succ {
        if !out.contains(succ) {
            out.push(*succ);
        }
    }
    out
}

// ---------------------------------------------------------------- 缓存

fn write_cache(sha256: &str, body: &str) -> Option<PathBuf> {
    let dir = super::index::cache_root()?.join(sha256);
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("vm_detect.json");
    std::fs::write(&path, body.as_bytes()).ok()?;
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::native::backends::index::{
        BlockEntry, FunctionEntry, ImportEntry, SampleRef, SCHEMA,
    };

    fn block(va: u64, succ: Vec<u64>) -> BlockEntry {
        BlockEntry {
            va,
            size: 0x10,
            succ,
        }
    }

    fn function(va: u64, name: &str, blocks: Vec<BlockEntry>) -> FunctionEntry {
        FunctionEntry {
            va,
            name: name.to_string(),
            size: 0x400,
            kind: "normal".to_string(),
            blocks: Some(blocks),
        }
    }

    fn index(functions: Vec<FunctionEntry>, imports: Vec<ImportEntry>) -> ProgramIndex {
        ProgramIndex {
            schema: SCHEMA,
            backend: "ghidra".to_string(),
            backend_version: "11.2.1".to_string(),
            sample: SampleRef {
                sha256: "b".repeat(64),
                size: 4096,
                format: "pe".to_string(),
                arch: Some("x86-64".to_string()),
                bits: Some(64),
                image_base: None,
            },
            functions,
            imports,
            truncated: false,
            note: None,
        }
    }

    /// 8 个 handler，每个都跳回 0x1000：分发器信号成立。
    fn vm_function() -> FunctionEntry {
        let mut blocks = vec![block(
            0x1000,
            (0..8).map(|index| 0x1100 + index * 0x10).collect(),
        )];
        for index in 0..8u64 {
            blocks.push(block(0x1100 + index * 0x10, vec![0x1000]));
        }
        function(0x1000, "vm_dispatch", blocks)
    }

    #[test]
    fn dispatcher_needs_many_successors_and_back_edges() {
        let index = index(vec![vm_function()], Vec::new());
        let payload = analyze(&index);
        assert_eq!(payload["signals"]["dispatcher"]["detected"], true);
        assert_eq!(payload["signals"]["dispatcher"]["candidates"][0]["successors"], 8);
        assert_eq!(payload["signals"]["dispatcher"]["candidates"][0]["backEdges"], 8);
    }

    #[test]
    fn ordinary_switch_without_back_edges_is_not_a_dispatcher() {
        // 8 个后继，但都跳到出口，不回头部：这是普通 switch。
        let mut blocks = vec![block(
            0x1000,
            (0..8).map(|index| 0x1100 + index * 0x10).collect(),
        )];
        for index in 0..8u64 {
            blocks.push(block(0x1100 + index * 0x10, vec![0x2000]));
        }
        blocks.push(block(0x2000, vec![]));
        let index = index(vec![function(0x1000, "parser", blocks)], Vec::new());
        let payload = analyze(&index);
        assert_eq!(payload["signals"]["dispatcher"]["detected"], false);
        assert_eq!(payload["verdict"], "none");
    }

    #[test]
    fn handler_table_is_a_sixteen_way_block() {
        let mut blocks = vec![block(
            0x3000,
            (0..16).map(|index| 0x4000 + index * 0x10).collect(),
        )];
        for index in 0..16u64 {
            blocks.push(block(0x4000 + index * 0x10, vec![0x3000]));
        }
        let index = index(vec![function(0x3000, "table", blocks)], Vec::new());
        let payload = analyze(&index);
        assert_eq!(payload["signals"]["handlerTable"]["detected"], true);
        assert_eq!(
            payload["signals"]["handlerTable"]["candidates"][0]["entries"],
            16
        );
    }

    #[test]
    fn two_signals_in_one_function_reach_medium() {
        let payload = analyze(&index(vec![vm_function()], Vec::new()));
        // vm_function 的分发器同时有 8 个后继（不到 16），所以只有一项；
        // 这里手工把后继加到 16，验证两项同函数 → medium。
        let mut blocks = vec![block(
            0x1000,
            (0..16).map(|index| 0x1100 + index * 0x10).collect(),
        )];
        for index in 0..16u64 {
            blocks.push(block(0x1100 + index * 0x10, vec![0x1000]));
        }
        let payload2 = analyze(&index(
            vec![function(0x1000, "vm_dispatch", blocks)],
            Vec::new(),
        ));
        assert_eq!(payload["strongSignals"], 1);
        assert_eq!(payload2["strongSignals"], 2);
        assert_eq!(payload2["verdict"], "suspected");
        assert_eq!(payload2["confidence"], "medium");
        assert_eq!(payload2["sameFunction"], true);
    }

    #[test]
    fn signals_in_different_functions_are_downgraded() {
        let dispatcher = vm_function();
        let table = function(
            0x3000,
            "table",
            vec![
                block(0x3000, (0..16).map(|index| 0x4000 + index * 0x10).collect()),
                block(0x4000, vec![]),
            ],
        );
        let payload = analyze(&index(vec![dispatcher, table], Vec::new()));
        assert_eq!(payload["rawStrongSignals"], 2);
        assert_eq!(payload["strongSignals"], 1);
        assert_eq!(payload["verdict"], "possible");
    }

    #[test]
    fn anti_debug_imports_are_reported_without_changing_verdict() {
        let index = index(
            vec![function(0x5000, "main", vec![block(0x5000, vec![])])],
            vec![
                ImportEntry {
                    name: "IsDebuggerPresent".to_string(),
                    library: Some("kernel32.dll".to_string()),
                },
                ImportEntry {
                    name: "printf".to_string(),
                    library: Some("msvcrt.dll".to_string()),
                },
            ],
        );
        let payload = analyze(&index);
        assert_eq!(payload["signals"]["antiDebug"]["detected"], true);
        assert_eq!(payload["signals"]["antiDebug"]["imports"][0], "IsDebuggerPresent");
        assert_eq!(payload["signals"]["context"]["detected"], Value::Null);
        assert_eq!(payload["verdict"], "none");
    }

    #[test]
    fn dex_without_blocks_is_honest_about_missing_signals() {
        let mut dex = index(vec![function(0x1000, "Lfoo;->bar", Vec::new())], Vec::new());
        dex.backend = "dex".to_string();
        dex.functions[0].blocks = None;
        let payload = analyze(&dex);
        assert_eq!(payload["functionsWithBlocks"], 0);
        assert_eq!(payload["verdict"], "none");
        assert!(payload["note"]
            .as_str()
            .unwrap_or("")
            .contains("不能证明"));
    }

    #[test]
    fn tool_is_registered_and_read_only() {
        let tools = tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name.as_ref(), "bin.vm.detect");
        assert_eq!(tools[0].permission, Permission::ReadOnly);
        assert_eq!(tools[0].meta.risk, Risk::Read);
    }
}
