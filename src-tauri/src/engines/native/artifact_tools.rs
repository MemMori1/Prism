//! 制品读写工具：`prism.artifact.put` / `prism.artifact.get`。
//!
//! 只在 DAG 节点里出现（`ToolScope::Dag`）：`dag_id` 由执行器注入，
//! 模型没有"指定别的 DAG"的入参——跨 DAG 隔离在参数层面就不成立。
//!
//! 为什么不是"把摘要塞进提示词就够了"：摘要只能让人看见有什么，做不了事。
//! 分析节点拿到 `strings` 摘要后往往要按地址回查具体条目，这时需要按需拉全文。

use serde_json::{json, Value};

use super::tools::{
    builtin_full, Network, Permission, Risk, ToolDef, ToolMeta, ToolOutcome, ToolResult, ToolScope,
};
use crate::dag::artifact::{ArtifactStore, PutRequest};

/// 单次 `get` 最多回多少字符：再长就该用工具筛，而不是整段倒进上下文。
const GET_LIMIT: usize = 40_000;

pub fn tools() -> Vec<std::sync::Arc<ToolDef>> {
    vec![
        builtin_full(
            "prism.artifact.put",
            "存制品",
            put_schema,
            Permission::WriteWorkspace,
            None,
            None,
            "",
            true,
            ToolMeta {
                tags: &["artifact"],
                risk: Risk::Write,
                network: Network::None,
                sandbox: None,
                scope: ToolScope::Dag,
            },
            run_put,
        ),
        builtin_full(
            "prism.artifact.get",
            "取制品",
            get_schema,
            Permission::ReadOnly,
            None,
            None,
            "",
            true,
            ToolMeta {
                tags: &["artifact"],
                risk: Risk::Read,
                network: Network::None,
                sandbox: None,
                scope: ToolScope::Dag,
            },
            run_get,
        ),
    ]
}

fn put_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "prism.artifact.put",
            "description": "把结果存成制品，返回制品 id。适合中大型产出（报告、JSON、导出文件）：下游节点只拿 id 与摘要，需要细节再取，避免把长文本塞进上下文。",
            "parameters": {
                "type": "object",
                "properties": {
                    "key": { "type": "string", "description": "语义键，如 report / strings / hashes / poc" },
                    "content": { "type": "string", "description": "文本内容；与 path 二选一" },
                    "path": { "type": "string", "description": "工作目录里的文件路径；与 content 二选一" },
                    "name": { "type": "string", "description": "展示用文件名，默认 <key>.json" },
                    "parent": { "type": "string", "description": "父制品 id：派生件（脱壳、清洗）指向原始样本，保血缘" }
                },
                "required": ["key"]
            }
        }
    })
}

fn get_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "prism.artifact.get",
            "description": "按 id 取回当前 DAG 里的制品内容。超过四万字符会截断并提示；二进制制品只给说明。",
            "parameters": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "制品 id（art-...）" },
                    "limit": { "type": "integer", "description": "最多取多少字符，默认 40000" }
                },
                "required": ["id"]
            }
        }
    })
}

/// 有没有仓库与作用域。缺一个就说清缺哪个，别返回空字符串。
fn run_put(ctx: &super::tools::ToolContext<'_>, args: &Value) -> ToolResult {
    let Some(scope) = ctx.dag else {
        return ToolResult::fail("制品工具只在 DAG 节点里可用（缺少节点作用域）");
    };
    let store = match ArtifactStore::global() {
        Ok(store) => store,
        Err(error) => return ToolResult::fail(format!("找不到制品仓库：{error}")),
    };
    let key = args.get("key").and_then(Value::as_str).unwrap_or("").trim();
    if key.is_empty() {
        return ToolResult::fail("缺少 key：它决定下游怎么引用这份制品");
    }
    let content = args.get("content").and_then(Value::as_str);
    let path = args.get("path").and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty());
    let bytes: Vec<u8> = match (content, path) {
        (Some(text), _) => text.as_bytes().to_vec(),
        (None, Some(relative)) => {
            let root = std::path::Path::new(ctx.directory);
            let full = match super::tools::resolve_path(root, relative) {
                Ok(path) => path,
                Err(error) => return ToolResult::fail(error),
            };
            match std::fs::read(&full) {
                Ok(bytes) => bytes,
                Err(error) => return ToolResult::fail(format!("读取 {relative} 失败：{error}")),
            }
        }
        (None, None) => {
            return ToolResult::fail("要么给 content（文本），要么给 path（工作目录里的文件）")
        }
    };
    let name = args.get("name").and_then(Value::as_str).unwrap_or("");
    let parent = args
        .get("parent")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());

    match store.put(PutRequest {
        dag_id: &scope.dag_id,
        node_id: &scope.node_id,
        key,
        name,
        kind: "",
        parent_id: parent,
        bytes: &bytes,
    }) {
        Ok(reference) => ToolResult::plain(ToolOutcome::ok(
            serde_json::to_string(&reference).unwrap_or_else(|_| reference.id.clone()),
        )),
        Err(error) => ToolResult::fail(error),
    }
}

fn run_get(ctx: &super::tools::ToolContext<'_>, args: &Value) -> ToolResult {
    let Some(scope) = ctx.dag else {
        return ToolResult::fail("制品工具只在 DAG 节点里可用（缺少节点作用域）");
    };
    let store = match ArtifactStore::global() {
        Ok(store) => store,
        Err(error) => return ToolResult::fail(format!("找不到制品仓库：{error}")),
    };
    let id = args.get("id").and_then(Value::as_str).unwrap_or("").trim();
    if id.is_empty() {
        return ToolResult::fail("缺少 id（art-...）");
    }
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .filter(|value| *value > 0)
        .unwrap_or(GET_LIMIT)
        .min(200_000);

    match store.read_text(&scope.dag_id, id, limit) {
        Ok((reference, text, truncated)) => {
            let mut out = format!(
                "【{}】 {} · {} 字节 · sha256 {}\n\n",
                reference.key,
                reference.name,
                reference.size,
                // 只给前 16 位：够核对，又不至于把整行哈希糊满对话。
                reference.sha256.chars().take(16).collect::<String>()
            );
            out.push_str(&text);
            if truncated {
                out.push_str(&format!(
                    "\n\n（已截到 {limit} 字符；用 json/文本工具筛出需要的部分，或调小 limit 定位）"
                ));
            }
            ToolResult::plain(ToolOutcome::ok(out))
        }
        Err(error) => ToolResult::fail(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_is_dag_scoped() {
        for tool in tools() {
            assert_eq!(tool.meta.scope, ToolScope::Dag, "{} 不该出现在聊天里", tool.name);
            assert!(tool.name.starts_with("prism.artifact."));
        }
    }
}
