//! `bin.cfg.extract`：把 `ProgramIndex` 的基本块导出成 json / dot / mermaid。
//!
//! 三个输出共用一份节点/边：json 给下游，dot 给 Graphviz，mermaid 给前端。
//! 节点 id 一律 `b_<va>`，**不塞函数名**——名字里有引号、空格、`::`，
//! 直接进 dot/mermaid 会把图炸掉。
//!
//! `aflj` 不给基本块，所以 radare2 的 IR 第一次画图时会跑一次 `afbj` detail pass；
//! 结果写回缓存，第二次不再 spawn。Ghidra 的导出里本来就有 blocks。

use std::collections::HashSet;
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

/// 默认画多少个函数；设计上限 2000。
const DEFAULT_FUNCTIONS: usize = 200;
const MAX_FUNCTIONS: usize = 2000;
/// 边上限：超过就不是"看控制流"，是"画毛线"。
const MAX_EDGES: usize = 5000;
/// 节点上限：比边宽松，但也要防超大函数把前端卡死。
const MAX_NODES: usize = 10_000;

pub fn tools() -> Vec<Arc<ToolDef>> {
    vec![builtin_full(
        "bin.cfg.extract",
        "控制流图导出",
        schema,
        Permission::ReadOnly,
        None,
        None,
        "",
        true,
        ToolMeta {
            tags: &["bin:static", "bin:cfg"],
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
            "name": "bin.cfg.extract",
            "description": "从反汇编 IR 导出控制流图（json / dot / mermaid）。blocks 缺失时对 radare2 跑一次 afbj detail pass 并写回缓存。只读样本、不执行样本。",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "样本路径（相对工作目录），或 DAG 制品引用 artifact:<id>"
                    },
                    "function": {
                        "type": "string",
                        "description": "只画一个函数：0x401000 / 十进制地址 / 函数名；不填画前 limit 个函数"
                    },
                    "format": {
                        "type": "string",
                        "enum": ["json", "dot", "mermaid"],
                        "description": "输出格式，默认 json"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "最多画多少个函数，默认 200，最大 2000"
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
    let format = args
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("json")
        .trim()
        .to_ascii_lowercase();
    if !matches!(format.as_str(), "json" | "dot" | "mermaid") {
        return ToolResult::fail(format!(
            "不支持的 format：{format}（只支持 json / dot / mermaid）"
        ));
    }
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_FUNCTIONS as u64)
        .clamp(1, MAX_FUNCTIONS as u64) as usize;

    let mut indexed = match ir::index_of(ctx, raw) {
        Ok(indexed) => indexed,
        Err(error) => return ToolResult::fail(error),
    };

    let selected: Vec<usize> = match args.get("function").and_then(Value::as_str) {
        Some(target) if !target.trim().is_empty() => {
            match find_function(&indexed.index, target.trim()) {
                Some(index) => vec![index],
                None => {
                    return ToolResult::fail(format!(
                        "没有这个函数：{target}。用 0x 地址、十进制地址或 IR 里的函数名。"
                    ))
                }
            }
        }
        _ => (0..indexed.index.functions.len().min(limit)).collect(),
    };
    if selected.is_empty() {
        return ToolResult::fail("IR 里一个函数都没有：样本可能不是可执行文件，或后端没识别出代码。");
    }

    let functions_truncated = indexed.index.functions.len() > selected.len();
    let need_detail: Vec<usize> = selected
        .iter()
        .copied()
        .filter(|index| indexed.index.functions[*index].blocks.is_none())
        .collect();
    let mut detail_note = None;
    if !need_detail.is_empty() {
        if indexed.index.backend == "radare2" {
            match ir::detail_blocks(&mut indexed, &need_detail) {
                Ok(count) => {
                    detail_note = Some(format!("afbj detail pass 补了 {count} 个函数的基本块。"));
                }
                Err(error) => return ToolResult::fail(error),
            }
        } else if indexed.index.backend == "dex" {
            detail_note = Some(
                "DEX 没有基本块（IR 只到方法边界）；json 里只有函数，dot/mermaid 会是空图。"
                    .to_string(),
            );
        } else {
            detail_note = Some("这个后端的 IR 没有基本块；dot/mermaid 只会包含节点。".to_string());
        }
    }

    let graph = build_graph(&indexed.index, &selected);
    let truncated = indexed.index.truncated
        || functions_truncated
        || graph.nodes_truncated
        || graph.edges_truncated;

    let payload = json!({
        "ok": true,
        "source": indexed.source,
        "backend": indexed.index.backend,
        "backendVersion": indexed.index.backend_version,
        "format": format,
        "function": if selected.len() == 1 {
            json!({
                "name": indexed.index.functions[selected[0]].name,
                "va": hex(indexed.index.functions[selected[0]].va),
                "size": indexed.index.functions[selected[0]].size,
            })
        } else {
            Value::Null
        },
        "functions": selected.len(),
        "blocks": graph.nodes.len(),
        "edges": graph.edges.len(),
        "truncated": truncated,
        "note": join_notes(&[
            detail_note,
            functions_truncated.then(|| format!("只画了前 {} 个函数。", selected.len())),
            graph.nodes_truncated.then(|| format!("节点超过 {MAX_NODES}，已截断。")),
            graph.edges_truncated.then(|| format!("边超过 {MAX_EDGES}，已截断。")),
            indexed.index.note.clone(),
        ]),
    });

    let (body, extension, kind) = match format.as_str() {
        "dot" => (
            render_dot(&indexed.index, &selected, &graph),
            "dot",
            "text",
        ),
        "mermaid" => (
            render_mermaid(&indexed.index, &selected, &graph),
            "mermaid",
            "text",
        ),
        _ => (
            serde_json::to_string_pretty(&json!({
                "function": payload["function"],
                "blocks": graph.nodes_json,
                "edges": graph.edges_json,
                "backend": indexed.index.backend,
                "truncated": truncated,
                "note": payload["note"],
            }))
            .unwrap_or_default(),
            "json",
            "json",
        ),
    };

    // 落一份缓存：聊天模式没有制品库，这份文件就是交付物。
    let cache = write_cache(&indexed.sample.sha256, &selected, &indexed.index, extension, &body);
    let mut payload = payload;
    payload["irCache"] = json!(indexed
        .cache_path()
        .map(|path| path.display().to_string()));
    payload["wrote"] = json!(cache
        .as_ref()
        .map(|path| format!("缓存（{}）", path.display()))
        .unwrap_or_else(|| "缓存不可用".to_string()));

    if let Some(scope) = ctx.dag {
        match ArtifactStore::global() {
            Ok(store) => {
                let name = cache
                    .as_ref()
                    .and_then(|path| path.file_name())
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| format!("cfg.{extension}"));
                match store.put(PutRequest {
                    dag_id: &scope.dag_id,
                    node_id: &scope.node_id,
                    key: "cfg",
                    name: &name,
                    kind,
                    parent_id: indexed.sample.artifact.as_deref(),
                    bytes: body.as_bytes(),
                }) {
                    Ok(reference) => {
                        payload["artifact"] = json!(reference.id);
                        payload["wrote"] =
                            json!(format!("制品库（key=cfg，{} 字节）", reference.size));
                    }
                    Err(error) => payload["artifactError"] = json!(error),
                }
            }
            Err(error) => payload["artifactError"] = json!(error),
        }
    }

    if format == "json" {
        ToolResult::plain(ToolOutcome::ok(
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()),
        ))
    } else {
        // dot / mermaid 的正文就是图；元信息放在注释里，避免污染语法。
        let comment = if format == "dot" { "//" } else { "%%" };
        let meta = serde_json::to_string(&payload).unwrap_or_default();
        ToolResult::plain(ToolOutcome::ok(format!(
            "{comment} Prism CFG\n{comment} {meta}\n{body}"
        )))
    }
}

// ---------------------------------------------------------------- 图

struct Graph {
    nodes: Vec<(u64, u32)>,
    edges: Vec<(u64, u64, &'static str)>,
    nodes_json: Vec<Value>,
    edges_json: Vec<Value>,
    nodes_truncated: bool,
    edges_truncated: bool,
}

fn build_graph(index: &ProgramIndex, selected: &[usize]) -> Graph {
    let mut nodes: Vec<(u64, u32)> = Vec::new();
    let mut seen_nodes: HashSet<u64> = HashSet::new();
    let mut edges: Vec<(u64, u64, &'static str)> = Vec::new();
    let mut seen_edges: HashSet<(u64, u64)> = HashSet::new();
    let mut nodes_truncated = false;
    let mut edges_truncated = false;

    for function_index in selected {
        let function = &index.functions[*function_index];
        let Some(blocks) = function.blocks.as_deref() else {
            continue;
        };
        for block in blocks {
            if seen_nodes.insert(block.va) {
                if nodes.len() >= MAX_NODES {
                    nodes_truncated = true;
                } else {
                    nodes.push((block.va, block.size));
                }
            }
            for succ in &block.succ {
                let kind = if *succ == block.va + block.size as u64 {
                    "fall"
                } else {
                    "jump"
                };
                if seen_edges.insert((block.va, *succ)) {
                    if edges.len() >= MAX_EDGES {
                        edges_truncated = true;
                    } else {
                        edges.push((block.va, *succ, kind));
                    }
                }
            }
        }
    }

    let nodes_json = nodes
        .iter()
        .map(|(va, size)| json!({ "va": hex(*va), "size": size }))
        .collect();
    let edges_json = edges
        .iter()
        .map(|(from, to, kind)| json!([hex(*from), hex(*to), kind]))
        .collect();

    Graph {
        nodes,
        edges,
        nodes_json,
        edges_json,
        nodes_truncated,
        edges_truncated,
    }
}

fn render_dot(index: &ProgramIndex, selected: &[usize], graph: &Graph) -> String {
    let mut out = String::from("digraph cfg {\n  rankdir=TB;\n  node [shape=box, fontname=\"monospace\"];\n");
    for (cluster, function_index) in selected.iter().enumerate() {
        let function = &index.functions[*function_index];
        out.push_str(&format!(
            "  subgraph cluster_{cluster} {{\n    label=\"{}\";\n",
            dot_escape(&function.name)
        ));
        if let Some(blocks) = function.blocks.as_deref() {
            for block in blocks {
                out.push_str(&format!(
                    "    {} [label=\"0x{:x}\"];\n",
                    node_id(block.va),
                    block.va
                ));
            }
        }
        out.push_str("  }\n");
    }
    for (from, to, kind) in &graph.edges {
        out.push_str(&format!(
            "  {} -> {} [label=\"{kind}\"];\n",
            node_id(*from),
            node_id(*to)
        ));
    }
    if graph.nodes.is_empty() {
        out.push_str("  empty [label=\"没有基本块\"];\n");
    }
    out.push_str("}\n");
    out
}

fn render_mermaid(index: &ProgramIndex, selected: &[usize], graph: &Graph) -> String {
    let mut out = String::from("flowchart TD\n");
    for (cluster, function_index) in selected.iter().enumerate() {
        let function = &index.functions[*function_index];
        out.push_str(&format!(
            "  subgraph f{cluster}[\"{}\"]\n",
            mermaid_escape(&function.name)
        ));
        if let Some(blocks) = function.blocks.as_deref() {
            for block in blocks {
                out.push_str(&format!(
                    "    {}[\"0x{:x}\"]\n",
                    node_id(block.va),
                    block.va
                ));
            }
        }
        out.push_str("  end\n");
    }
    for (from, to, kind) in &graph.edges {
        out.push_str(&format!(
            "  {} -->|{kind}| {}\n",
            node_id(*from),
            node_id(*to)
        ));
    }
    if graph.nodes.is_empty() {
        out.push_str("  empty[\"没有基本块\"]\n");
    }
    out
}

fn node_id(va: u64) -> String {
    format!("b_{va:x}")
}

fn dot_escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

fn mermaid_escape(text: &str) -> String {
    text.replace('"', "'").replace('|', "/")
}

// ---------------------------------------------------------------- 辅助

fn find_function(index: &ProgramIndex, target: &str) -> Option<usize> {
    if let Some(va) = parse_va(target) {
        if let Some(found) = index.functions.iter().position(|function| function.va == va) {
            return Some(found);
        }
        if let Some(found) = index.functions.iter().position(|function| {
            va >= function.va && va < function.va.saturating_add(function.size as u64)
        }) {
            return Some(found);
        }
    }
    index
        .functions
        .iter()
        .position(|function| function.name == target)
}

fn parse_va(text: &str) -> Option<u64> {
    let text = text.trim();
    if let Some(hex) = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
    {
        return u64::from_str_radix(hex, 16).ok();
    }
    text.parse().ok()
}

fn hex(va: u64) -> String {
    format!("0x{va:x}")
}

fn join_notes(notes: &[Option<String>]) -> Option<String> {
    let parts: Vec<&str> = notes.iter().flatten().map(String::as_str).collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn write_cache(
    sha256: &str,
    selected: &[usize],
    index: &ProgramIndex,
    extension: &str,
    body: &str,
) -> Option<PathBuf> {
    let name = if selected.len() == 1 {
        format!("cfg_{:x}.{extension}", index.functions[selected[0]].va)
    } else {
        format!("cfg_all.{extension}")
    };
    let dir = super::index::cache_root()?.join(sha256).join("cfg");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(name);
    std::fs::write(&path, body.as_bytes()).ok()?;
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::native::backends::index::{
        BlockEntry, FunctionEntry, ImportEntry, SampleRef, SCHEMA,
    };

    fn sample_index() -> ProgramIndex {
        ProgramIndex {
            schema: SCHEMA,
            backend: "ghidra".to_string(),
            backend_version: "11.2.1".to_string(),
            sample: SampleRef {
                sha256: "a".repeat(64),
                size: 1024,
                format: "elf".to_string(),
                arch: Some("x86-64".to_string()),
                bits: Some(64),
                image_base: None,
            },
            functions: vec![
                FunctionEntry {
                    va: 0x401000,
                    name: "entry".to_string(),
                    size: 32,
                    kind: "normal".to_string(),
                    blocks: Some(vec![
                        BlockEntry {
                            va: 0x401000,
                            size: 8,
                            succ: vec![0x401008],
                        },
                        BlockEntry {
                            va: 0x401008,
                            size: 8,
                            succ: vec![0x401010],
                        },
                    ]),
                },
                FunctionEntry {
                    va: 0x401020,
                    name: "other".to_string(),
                    size: 16,
                    kind: "normal".to_string(),
                    blocks: Some(vec![BlockEntry {
                        va: 0x401020,
                        size: 16,
                        succ: vec![0x401000],
                    }]),
                },
            ],
            imports: Vec::<ImportEntry>::new(),
            truncated: false,
            note: None,
        }
    }

    #[test]
    fn graph_nodes_and_edges_are_deduped() {
        let index = sample_index();
        let graph = build_graph(&index, &[0, 1]);
        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.edges.len(), 3);
        assert_eq!(graph.edges[0], (0x401000, 0x401008, "fall"));
        assert_eq!(graph.edges[2], (0x401020, 0x401000, "jump"));
    }

    #[test]
    fn dot_and_mermaid_use_safe_node_ids() {
        let index = sample_index();
        let graph = build_graph(&index, &[0, 1]);
        let dot = render_dot(&index, &[0, 1], &graph);
        assert!(dot.contains("b_401000"));
        assert!(dot.contains("label=\"fall\""));
        assert!(dot.contains("subgraph cluster_0"));
        let mermaid = render_mermaid(&index, &[0, 1], &graph);
        assert!(mermaid.contains("flowchart TD"));
        assert!(mermaid.contains("b_401000 -->|fall| b_401008"));
        assert!(mermaid.contains("subgraph f0[\"entry\"]"));
    }

    #[test]
    fn function_lookup_accepts_hex_decimal_and_name() {
        let index = sample_index();
        assert_eq!(find_function(&index, "0x401020"), Some(1));
        assert_eq!(find_function(&index, "4198432"), Some(1));
        assert_eq!(find_function(&index, "entry"), Some(0));
        assert_eq!(find_function(&index, "0x401004"), Some(0));
        assert_eq!(find_function(&index, "nope"), None);
    }

    #[test]
    fn node_ids_are_valid_graphviz_and_mermaid() {
        let id = node_id(0x401000);
        assert!(id.starts_with("b_"));
        assert!(id.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_'));
    }
}
