//! 安全研究线的 P1 分析工具：`bin.json.jq` / `bin.xref.search` /
//! `bin.importexport.list` / `bin.crypto.scan` / `bin.diff`。
//!
//! 与 `forensics.rs` 的分工：那边回答"这个文件是什么"（识别、哈希、字符串），
//! 这边回答"里面有什么、被谁用了"（结构、引用、算法、差异）。
//!
//! 三个共同约束，写在这里免得每个工具各说一遍：
//! - **只读**：全部 `Risk::Read`，不写工作目录、不联网、不执行样本；
//! - **有上限**：每个入口都卡读取字节数，超了标注 `scanned` 而不是硬扫到卡死；
//! - **不猜**：解析不出来的字段直接省略。安全分析里编出来的一个架构名、
//!   一个"疑似 AES"，会让人做出错误判断，比"未知"有害得多。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};

use super::forensics::{
    hex, identify, read_head, read_u16, read_u32, read_u64, respond, target_of,
};
use super::tools::{
    builtin_full, resolve_path, Network, Permission, Risk, ToolContext, ToolDef, ToolMeta,
    ToolResult, ToolRun, ToolScope,
};
use crate::dag::artifact::{sha256_of, ArtifactStore};

/// JSON 查询的读取上限：再大就不是配置/报告而是数据集。
const JSON_LIMIT: u64 = 8 * 1024 * 1024;
/// 结构分析的读取上限。超过这个大小的样本，静态扫描的收益远低于耗时。
const SCAN_LIMIT: u64 = 64 * 1024 * 1024;
/// 命中条目默认 / 上限。
const HITS_DEFAULT: usize = 64;
const HITS_MAX: usize = 2000;
/// JSON 查询结果默认 / 上限条数。
const VALUES_DEFAULT: usize = 200;
const VALUES_MAX: usize = 5000;
/// 差异比较的分块大小。
const DIFF_CHUNK: usize = 4096;

pub fn tools() -> Vec<Arc<ToolDef>> {
    vec![
        static_tool("bin.json.jq", "JSON 路径查询", json_schema, run_json),
        static_tool("bin.xref.search", "交叉引用检索", xref_schema, run_xref),
        static_tool(
            "bin.importexport.list",
            "导入导出表",
            import_schema,
            run_import_export,
        ),
        static_tool("bin.crypto.scan", "密码特征扫描", crypto_schema, run_crypto),
        static_tool(
            "bin.crypto.asn1.parse",
            "证书与密钥解析",
            asn1_schema,
            run_asn1,
        ),
        static_tool("bin.crypto.xor_brute", "多字节 XOR 爆破", xor_schema, run_xor_brute),
        static_tool("bin.diff", "两样本差异", diff_schema, run_diff),
    ]
}

/// 五个工具共用同一份元信息：静态、只读、不出网、不限作用域。
fn static_tool(
    name: &'static str,
    blurb: &'static str,
    schema: fn() -> Value,
    run: ToolRun,
) -> Arc<ToolDef> {
    builtin_full(
        name,
        blurb,
        schema,
        Permission::ReadOnly,
        None,
        None,
        "",
        true,
        ToolMeta {
            tags: &["bin:static"],
            risk: Risk::Read,
            network: Network::None,
            sandbox: None,
            scope: ToolScope::Any,
            needs: &[],
        },
        run,
    )
}

// ---------------------------------------------------------------- 声明

fn json_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bin.json.jq",
            "description": "在 JSON 文件里按路径取值，用于读上一节点产出的分析结果、配置、清单。支持 .a.b / [0] / [-1] / [] 展开，不支持管道与函数。",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "相对工作目录的 JSON 文件路径" },
                    "json": { "type": "string", "description": "直接给 JSON 文本（与 path 二选一）" },
                    "query": { "type": "string", "description": "路径表达式，默认 . 表示整体" },
                    "limit": { "type": "integer", "description": "最多返回多少个元素，默认 200" }
                },
                "required": ["query"]
            }
        }
    })
}

fn xref_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bin.xref.search",
            "description": "在样本里搜索一个字节串、字符串或 UTF-16 串，返回全部文件偏移，并换算成所在段与虚拟地址。逆向里用来定位「这个字符串被哪段代码引用」。",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "相对工作目录的样本路径" },
                    "needle": { "type": "string", "description": "要搜的内容：文本，或 encoding=hex 时的十六进制串" },
                    "encoding": { "type": "string", "enum": ["ascii", "utf16", "hex"], "description": "默认 ascii" },
                    "limit": { "type": "integer", "description": "最多返回多少处，默认 64，上限 2000" }
                },
                "required": ["path", "needle"]
            }
        }
    })
}

fn import_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bin.importexport.list",
            "description": "列出导入导出表：PE 的 IAT / EAT、ELF 的动态符号。自动标记跨进程写、远程线程、反调试、加密、网络等高危 API，是判断样本行为的第一手材料。",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "相对工作目录的样本路径" },
                    "filter": { "type": "string", "enum": ["all", "imports", "exports"], "description": "默认 all" }
                },
                "required": ["path"]
            }
        }
    })
}

fn crypto_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bin.crypto.scan",
            "description": "扫描密码学常量：AES S 盒、SHA/MD5 初始向量、CRC32 表、DES S 盒、ChaCha 常量、RSA 公钥 DER、PEM 头。可选做单字节 XOR 试探。只报命中偏移与依据，不给不确定的结论。",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "相对工作目录的样本路径" },
                    "xorProbe": { "type": "string", "description": "已知明文片段（如 http://），用它去试单字节 XOR 密钥" }
                },
                "required": ["path"]
            }
        }
    })
}

fn asn1_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bin.crypto.asn1.parse",
            "description": "解析 PEM / DER 的 X.509 证书、PKCS#7 签名块（APK 的 CERT.RSA / CERT.EC）、RSA / EC 公钥与 PKCS#8 / PKCS#1 私钥：给出算法、位长、签发者、主体、有效期、SHA-256 指纹。只读结构，不做验签与解密；私钥只报元信息，内容不导出。",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "相对工作目录的样本路径（证书文件，或内嵌证书的二进制）" },
                    "offset": { "type": "string", "description": "十六进制偏移，定位内嵌 PEM/DER 的起点；不传则从文件头解析" }
                },
                "required": ["path"]
            }
        }
    })
}

fn xor_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bin.crypto.xor_brute",
            "description": "多字节 XOR 爆破：给一段已知明文（如 http://、admin），在样本里按 1..N 字节密钥逐位反推，返回命中的密钥与全部偏移。恢复出完整明文才是 certain；probe 不够长或落在文件尾的候选标 low，不当下结论。",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "相对工作目录的样本路径" },
                    "probe": { "type": "string", "description": "已知明文片段，如 http:// 或 android.intent" },
                    "minLen": { "type": "integer", "description": "密钥最小字节数，默认 1" },
                    "maxLen": { "type": "integer", "description": "密钥最大字节数，默认 8，上限 16" }
                },
                "required": ["path", "probe"]
            }
        }
    })
}

fn diff_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bin.diff",
            "description": "比较两个样本或两个制品：大小、哈希、格式字段、字节差异区间。脱壳前后、两次抓样本、打补丁前后都用它。a / b 可以写 artifact:<id> 引用同一次 DAG 的制品。",
            "parameters": {
                "type": "object",
                "properties": {
                    "a": { "type": "string", "description": "基准：文件路径或 artifact:<id>" },
                    "b": { "type": "string", "description": "对照：文件路径或 artifact:<id>" }
                },
                "required": ["a", "b"]
            }
        }
    })
}

// ---------------------------------------------------------------- JSON 查询

enum Step {
    Key(String),
    Index(i64),
    Each,
}

/// 解析极简路径语法。
///
/// 刻意不支持管道与函数：一旦支持，就等于在工具里重写一个 jq，
/// 而模型完全可以直接读下标量字段再自己判断——能力足够，出错面小得多。
fn parse_query(raw: &str) -> Result<Vec<Step>, String> {
    let text = raw.trim();
    if text.is_empty() || text == "." {
        return Ok(Vec::new());
    }
    let chars: Vec<char> = text.chars().collect();
    let mut steps = Vec::new();
    let mut index = 0usize;
    if chars[0] == '.' {
        index = 1;
    }
    while index < chars.len() {
        match chars[index] {
            '.' => {
                index += 1;
                if index >= chars.len() {
                    return Err("查询不能以 . 结尾".to_string());
                }
                if chars[index] == '[' {
                    continue;
                }
                let start = index;
                while index < chars.len() && chars[index] != '.' && chars[index] != '[' {
                    index += 1;
                }
                steps.push(Step::Key(chars[start..index].iter().collect()));
            }
            '[' => {
                let mut end = index + 1;
                while end < chars.len() && chars[end] != ']' {
                    end += 1;
                }
                if end >= chars.len() {
                    return Err("方括号没有闭合".to_string());
                }
                let inner: String = chars[index + 1..end].iter().collect();
                let inner = inner.trim();
                if inner.is_empty() {
                    steps.push(Step::Each);
                } else if inner.starts_with('"') || inner.starts_with('\'') {
                    steps.push(Step::Key(
                        inner.trim_matches(|ch| ch == '"' || ch == '\'').to_string(),
                    ));
                } else {
                    let number: i64 = inner
                        .parse()
                        .map_err(|_| format!("方括号里只接受整数或引号字符串：{inner}"))?;
                    steps.push(Step::Index(number));
                }
                index = end + 1;
            }
            _ => {
                let start = index;
                while index < chars.len() && chars[index] != '.' && chars[index] != '[' {
                    index += 1;
                }
                steps.push(Step::Key(chars[start..index].iter().collect()));
            }
        }
    }
    Ok(steps)
}

fn render_step(step: &Step) -> String {
    match step {
        Step::Key(key) => format!(".{key}"),
        Step::Index(at) => format!("[{at}]"),
        Step::Each => "[]".to_string(),
    }
}

fn apply_query(root: &Value, steps: &[Step]) -> Result<Vec<Value>, String> {
    let mut current = vec![root.clone()];
    for (depth, step) in steps.iter().enumerate() {
        let mut next = Vec::new();
        for value in current {
            match step {
                Step::Key(key) => {
                    if let Some(found) = value.get(key) {
                        next.push(found.clone());
                    }
                }
                Step::Index(at) => {
                    let found = value.as_array().and_then(|items| {
                        let length = items.len() as i64;
                        let position = if *at < 0 { length + *at } else { *at };
                        if position < 0 || position >= length {
                            None
                        } else {
                            items.get(position as usize).cloned()
                        }
                    });
                    if let Some(found) = found {
                        next.push(found);
                    }
                }
                Step::Each => {
                    if let Some(items) = value.as_array() {
                        next.extend(items.iter().cloned());
                    }
                }
            }
        }
        if next.is_empty() {
            let walked: String = steps[..=depth].iter().map(render_step).collect();
            return Err(format!(
                "{walked} 没有命中：上一层没有这个键，或者它不是数组"
            ));
        }
        current = next;
    }
    Ok(current)
}

fn run_json(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or(".")
        .to_string();
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map(|value| (value as usize).clamp(1, VALUES_MAX))
        .unwrap_or(VALUES_DEFAULT);

    let (text, source) = match args.get("json").and_then(Value::as_str) {
        Some(inline) => (inline.to_string(), "inline".to_string()),
        None => {
            let (path, size) = match target_of(ctx, args) {
                Ok(found) => found,
                Err(failure) => return failure,
            };
            if size > JSON_LIMIT {
                return ToolResult::fail(format!(
                    "文件 {size} 字节，超过 JSON 查询上限 {JSON_LIMIT}；请先用 bin.json.jq 读小块或换制品"
                ));
            }
            match read_head(&path, JSON_LIMIT) {
                Ok(bytes) => (
                    String::from_utf8_lossy(&bytes).to_string(),
                    path.display().to_string(),
                ),
                Err(error) => return ToolResult::fail(error),
            }
        }
    };

    let root: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(error) => return ToolResult::fail(format!("不是合法 JSON：{error}")),
    };
    let steps = match parse_query(&query) {
        Ok(steps) => steps,
        Err(error) => return ToolResult::fail(error),
    };
    let hits = match apply_query(&root, &steps) {
        Ok(hits) => hits,
        Err(error) => return ToolResult::fail(error),
    };

    let matched = hits.len();
    let shown: Vec<Value> = hits.into_iter().take(limit).collect();
    let value = if shown.len() == 1 {
        shown[0].clone()
    } else {
        json!(shown)
    };
    respond(
        ctx,
        "json",
        json!({
            "source": source,
            "query": query,
            "matched": matched,
            "returned": shown.len(),
            "truncated": matched > limit,
            "value": value,
        }),
    )
}

// ---------------------------------------------------------------- 段/节映射

/// 文件偏移到虚拟地址的映射单位。PE 用节表，ELF 用 PT_LOAD 段。
struct Region {
    name: String,
    offset: u64,
    size: u64,
    address: u64,
}

fn regions(head: &[u8]) -> Vec<Region> {
    match head.get(0..4) {
        Some([0x4d, 0x5a, ..]) => pe_regions(head),
        Some([0x7f, b'E', b'L', b'F']) => elf_regions(head),
        _ => Vec::new(),
    }
}

fn pe_regions(head: &[u8]) -> Vec<Region> {
    let mut out = Vec::new();
    let Some(pe_offset) = read_u32(head, 0x3c, true).map(|value| value as usize) else {
        return out;
    };
    if head.get(pe_offset..pe_offset + 4) != Some(b"PE\0\0".as_slice()) {
        return out;
    }
    let coff = pe_offset + 4;
    let Some(count) = read_u16(head, coff + 2, true) else {
        return out;
    };
    let Some(optional_size) = read_u16(head, coff + 16, true) else {
        return out;
    };
    let table = coff + 20 + optional_size as usize;
    for index in 0..count as usize {
        let at = table + index * 40;
        let Some(name_raw) = head.get(at..at + 8) else {
            break;
        };
        let name = String::from_utf8_lossy(name_raw)
            .trim_end_matches('\0')
            .to_string();
        let virtual_size = read_u32(head, at + 8, true).unwrap_or(0) as u64;
        let address = read_u32(head, at + 12, true).unwrap_or(0) as u64;
        let raw_size = read_u32(head, at + 16, true).unwrap_or(0) as u64;
        let raw_offset = read_u32(head, at + 20, true).unwrap_or(0) as u64;
        if raw_size == 0 {
            continue;
        }
        out.push(Region {
            name,
            offset: raw_offset,
            size: raw_size.max(virtual_size),
            address,
        });
    }
    out
}

fn elf_regions(head: &[u8]) -> Vec<Region> {
    let mut out = Vec::new();
    let Some(class) = head.get(4).copied() else {
        return out;
    };
    let bits = if class == 2 { 64 } else { 32 };
    let little = head.get(5).copied() != Some(2);
    let (phoff_at, phentsize_at, phnum_at) = if bits == 64 {
        (32usize, 54usize, 56usize)
    } else {
        (28, 42, 44)
    };
    let phoff = if bits == 64 {
        read_u64(head, phoff_at, little)
    } else {
        read_u32(head, phoff_at, little).map(u64::from)
    };
    let Some(phoff) = phoff else {
        return out;
    };
    let Some(entry_size) = read_u16(head, phentsize_at, little) else {
        return out;
    };
    let Some(count) = read_u16(head, phnum_at, little) else {
        return out;
    };
    for index in 0..count as usize {
        let at = phoff as usize + index * entry_size as usize;
        let Some(kind) = read_u32(head, at, little) else {
            break;
        };
        if kind != 1 {
            continue;
        }
        let (offset, address, file_size) = if bits == 64 {
            (
                read_u64(head, at + 8, little).unwrap_or(0),
                read_u64(head, at + 16, little).unwrap_or(0),
                read_u64(head, at + 32, little).unwrap_or(0),
            )
        } else {
            (
                read_u32(head, at + 4, little).unwrap_or(0) as u64,
                read_u32(head, at + 8, little).unwrap_or(0) as u64,
                read_u32(head, at + 16, little).unwrap_or(0) as u64,
            )
        };
        if file_size == 0 {
            continue;
        }
        out.push(Region {
            name: format!("LOAD{index}"),
            offset,
            size: file_size,
            address,
        });
    }
    out
}

/// 偏移 → (所在段, 虚拟地址)。落在任何段之外时两项都不给。
fn locate(regions: &[Region], offset: u64) -> (Option<&str>, Option<u64>) {
    for region in regions {
        if offset >= region.offset && offset < region.offset + region.size {
            return (
                Some(region.name.as_str()),
                Some(region.address + (offset - region.offset)),
            );
        }
    }
    (None, None)
}

/// 文件偏移 → (段名, 虚拟地址)。PE / ELF 可映射，其他格式返回 `(None, None)`。
///
/// `bin.crypto.hash_identify` 用它把"常量在第几字节"变成"常量在哪个函数里"。
/// 映射不上就如实留空，不按函数大小瞎猜。
pub(crate) fn map_offset(head: &[u8], offset: u64) -> (Option<String>, Option<u64>) {
    let regions = regions(head);
    let (name, va) = locate(&regions, offset);
    (name.map(str::to_string), va)
}

/// 按首字节过滤的全量搜索。比逐窗口 `position` 快，也不引新依赖。
pub(crate) fn find_all(haystack: &[u8], needle: &[u8], limit: usize) -> (Vec<usize>, bool) {
    let mut hits = Vec::new();
    if needle.is_empty() || haystack.len() < needle.len() {
        return (hits, false);
    }
    let first = needle[0];
    let span = needle.len();
    let mut at = 0usize;
    while at + span <= haystack.len() {
        if haystack[at] == first && &haystack[at..at + span] == needle {
            hits.push(at);
            if hits.len() >= limit {
                return (hits, true);
            }
            at += span;
        } else {
            at += 1;
        }
    }
    (hits, false)
}

// ---------------------------------------------------------------- 交叉引用

fn encode_needle(raw: &str, encoding: &str) -> Result<Vec<u8>, String> {
    match encoding {
        "hex" => {
            let cleaned: String = raw.chars().filter(|ch| !ch.is_whitespace() && *ch != ',').collect();
            if cleaned.len() % 2 != 0 {
                return Err("hex 串长度必须是偶数".to_string());
            }
            let mut bytes = Vec::with_capacity(cleaned.len() / 2);
            let chars: Vec<char> = cleaned.chars().collect();
            for pair in chars.chunks(2) {
                let text: String = pair.iter().collect();
                let byte = u8::from_str_radix(&text, 16)
                    .map_err(|_| format!("不是合法的十六进制：{text}"))?;
                bytes.push(byte);
            }
            Ok(bytes)
        }
        "utf16" => {
            let mut bytes = Vec::with_capacity(raw.len() * 2);
            for unit in raw.encode_utf16() {
                bytes.extend_from_slice(&unit.to_le_bytes());
            }
            Ok(bytes)
        }
        _ => Ok(raw.as_bytes().to_vec()),
    }
}

fn build_needle(args: &Value) -> Result<Vec<u8>, ToolResult> {
    let raw = args
        .get("needle")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ToolResult::fail("缺少 needle"))?;
    let encoding = args
        .get("encoding")
        .and_then(Value::as_str)
        .unwrap_or("ascii");
    encode_needle(raw, encoding).map_err(ToolResult::fail)
}

fn run_xref(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let (path, size) = match target_of(ctx, args) {
        Ok(found) => found,
        Err(failure) => return failure,
    };
    let needle = match build_needle(args) {
        Ok(bytes) => bytes,
        Err(failure) => return failure,
    };
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map(|value| (value as usize).clamp(1, HITS_MAX))
        .unwrap_or(HITS_DEFAULT);
    let bytes = match read_head(&path, SCAN_LIMIT) {
        Ok(bytes) => bytes,
        Err(error) => return ToolResult::fail(error),
    };

    let regions = regions(&bytes);
    let (found, truncated) = find_all(&bytes, &needle, limit);
    let hits: Vec<Value> = found
        .iter()
        .map(|offset| {
            let (section, address) = locate(&regions, *offset as u64);
            let from = offset.saturating_sub(8);
            let to = (offset + needle.len() + 8).min(bytes.len());
            json!({
                "offset": format!("0x{offset:x}"),
                "va": address.map(|value| format!("0x{value:x}")),
                "section": section,
                "context": hex(&bytes[from..to]),
            })
        })
        .collect();

    respond(
        ctx,
        "xref",
        json!({
            "path": path.display().to_string(),
            "size": size,
            "scanned": bytes.len(),
            "needle": hex(&needle),
            "needleLen": needle.len(),
            "hits": hits.len(),
            "truncated": truncated,
            "matches": hits,
        }),
    )
}

// ---------------------------------------------------------------- 导入导出

/// 高危 API → 它在样本里意味着什么。匹配是"名字包含"，所以 `VirtualAllocEx`
/// 也能命中 `VirtualAlloc`，不用为每个后缀变体各写一条。
const SENSITIVE: &[(&str, &str)] = &[
    ("VIRTUALALLOC", "内存分配"),
    ("VIRTUALPROTECT", "改内存权限"),
    ("WRITEPROCESSMEMORY", "跨进程写"),
    ("READPROCESSMEMORY", "跨进程读"),
    ("CREATEREMOTETHREAD", "远程线程注入"),
    ("NTCREATETHREADEX", "远程线程注入"),
    ("QUEUEUSERAPC", "APC 注入"),
    ("SETTHREADCONTEXT", "线程上下文改写"),
    ("OPENPROCESS", "打开其它进程"),
    ("LOADLIBRARY", "动态加载模块"),
    ("LDRLOADDLL", "动态加载模块"),
    ("GETPROCADDRESS", "动态取址（隐藏导入）"),
    ("SETWINDOWSHOOKEX", "全局钩子"),
    ("CREATE TOOLHELP32", "进程/模块枚举"),
    ("CREATETOOLHELP32", "进程/模块枚举"),
    ("ISDEBUGGERPRESENT", "反调试"),
    ("CHECKREMOTEDEBUGGERPRESENT", "反调试"),
    ("NTQUERYINFORMATIONPROCESS", "反调试"),
    ("OUTPUTDEBUGSTRING", "反调试"),
    ("CRYPTENCRYPT", "加密"),
    ("CRYPTDECRYPT", "加密"),
    ("CRYPTACQUIRECONTEXT", "加密"),
    ("BCRYPT", "加密"),
    ("INTERNETOPEN", "网络"),
    ("WINHTTPOPEN", "网络"),
    ("URLDOWNLOADTOFILE", "下载"),
    ("WINEXEC", "执行"),
    ("SHELLEXECUTE", "执行"),
    ("CREATEPROCESS", "执行"),
    ("REGSETVALUE", "注册表写"),
    ("REGCREATEKEY", "注册表写"),
    ("SETFILEATTRIBUTES", "改文件属性"),
    ("FINDFIRSTFILE", "文件枚举"),
    ("GETTICKCOUNT", "计时（沙箱探测）"),
    ("PTRACE", "调试"),
    ("DLOPEN", "动态加载模块"),
    ("DLSYM", "动态取址（隐藏导入）"),
    ("MPROTECT", "改内存权限"),
    ("EXECVE", "执行"),
    ("SYSTEM", "执行"),
    ("POPEN", "执行"),
    ("SOCKET", "网络"),
    ("CONNECT", "网络"),
    ("GETADDRINFO", "DNS"),
];

fn sensitive_of(name: &str) -> Option<&'static str> {
    let upper = name.to_ascii_uppercase();
    SENSITIVE
        .iter()
        .find(|(needle, _)| upper.contains(needle))
        .map(|(_, meaning)| *meaning)
}

/// RVA → 文件偏移。PE 里几乎所有指针都是 RVA，不能直接当偏移用。
fn rva_to_offset(regions: &[Region], rva: u64) -> Option<usize> {
    for region in regions {
        if rva >= region.address && rva < region.address + region.size {
            return Some((region.offset + (rva - region.address)) as usize);
        }
    }
    None
}

fn c_string(bytes: &[u8], at: usize, cap: usize) -> Option<String> {
    let slice = bytes.get(at..)?;
    let end = slice.iter().take(cap).position(|byte| *byte == 0)?;
    Some(String::from_utf8_lossy(&slice[..end]).to_string())
}

fn pe_imports(bytes: &[u8], regions: &[Region], dir_rva: u32, bits: u16) -> Vec<Value> {
    let mut out = Vec::new();
    let Some(mut cursor) = rva_to_offset(regions, dir_rva as u64) else {
        return out;
    };
    for _ in 0..500 {
        let Some(name_rva) = read_u32(bytes, cursor + 12, true) else {
            break;
        };
        let Some(thunk_rva) = read_u32(bytes, cursor + 16, true) else {
            break;
        };
        if name_rva == 0 || thunk_rva == 0 {
            break;
        }
        let Some(dll_at) = rva_to_offset(regions, name_rva as u64) else {
            break;
        };
        let Some(dll) = c_string(bytes, dll_at, 256) else {
            break;
        };

        let mut functions = Vec::new();
        let step = if bits == 64 { 8 } else { 4 };
        let mut thunk_at = rva_to_offset(regions, thunk_rva as u64);
        for _ in 0..2000 {
            let Some(at) = thunk_at else { break };
            let entry = if bits == 64 {
                read_u64(bytes, at, true).unwrap_or(0)
            } else {
                read_u32(bytes, at, true).unwrap_or(0) as u64
            };
            if entry == 0 {
                break;
            }
            let ordinal = if bits == 64 {
                entry & 0x8000_0000_0000_0000 != 0
            } else {
                entry & 0x8000_0000 != 0
            };
            if ordinal {
                functions.push(json!({
                    "ordinal": (entry & 0xffff) as u16,
                    "sensitive": null,
                }));
            } else {
                // 低 31 位是 IMAGE_IMPORT_BY_NAME 的 RVA：2 字节 hint 后跟名字。
                let name_at = rva_to_offset(regions, entry & 0x7fff_ffff);
                if let Some(text) = name_at.and_then(|at| c_string(bytes, at + 2, 512)) {
                    functions.push(json!({
                        "name": text,
                        "sensitive": sensitive_of(&text),
                    }));
                }
            }
            thunk_at = thunk_at.map(|value| value + step);
        }

        out.push(json!({
            "library": dll,
            "functions": functions.len(),
            "sensitive": functions
                .iter()
                .filter(|item| item.get("sensitive").map(|flag| !flag.is_null()).unwrap_or(false))
                .count(),
            "detail": functions,
        }));
        cursor += 20;
    }
    out
}

fn pe_exports(bytes: &[u8], regions: &[Region], dir_rva: u32) -> Value {
    let Some(at) = rva_to_offset(regions, dir_rva as u64) else {
        return json!({ "names": [], "count": 0 });
    };
    let count = read_u32(bytes, at + 24, true).unwrap_or(0) as usize;
    let names_rva = read_u32(bytes, at + 32, true).unwrap_or(0) as u64;
    let mut names = Vec::new();
    for index in 0..count.min(2000) {
        let Some(slot) = rva_to_offset(regions, names_rva + (index as u64) * 4) else {
            break;
        };
        let Some(name_rva) = read_u32(bytes, slot, true) else {
            break;
        };
        let Some(name_at) = rva_to_offset(regions, name_rva as u64) else {
            break;
        };
        if let Some(name) = c_string(bytes, name_at, 512) {
            names.push(name);
        }
    }
    json!({ "names": names, "count": count })
}

/// ELF 动态符号表。`sh_link` 指字符串表，未定义（`st_shndx == 0`）即导入。
fn elf_symbols(bytes: &[u8]) -> Vec<Value> {
    let mut out = Vec::new();
    let bits = if bytes.get(4) == Some(&2) { 64 } else { 32 };
    let little = bytes.get(5).copied() != Some(2);
    let (shoff_at, shentsize_at, shnum_at) = if bits == 64 {
        (40usize, 58usize, 60usize)
    } else {
        (32, 46, 48)
    };
    let shoff = if bits == 64 {
        read_u64(bytes, shoff_at, little)
    } else {
        read_u32(bytes, shoff_at, little).map(u64::from)
    };
    let Some(shoff) = shoff else {
        return out;
    };
    let Some(entry_size) = read_u16(bytes, shentsize_at, little) else {
        return out;
    };
    let Some(count) = read_u16(bytes, shnum_at, little) else {
        return out;
    };

    struct Section {
        kind: u32,
        offset: u64,
        size: u64,
        link: u32,
        entry_size: u64,
    }
    let mut sections = Vec::new();
    for index in 0..count as usize {
        let at = shoff as usize + index * entry_size as usize;
        let (offset, size, link, entry_size) = if bits == 64 {
            (
                read_u64(bytes, at + 24, little).unwrap_or(0),
                read_u64(bytes, at + 32, little).unwrap_or(0),
                read_u32(bytes, at + 40, little).unwrap_or(0),
                read_u64(bytes, at + 56, little).unwrap_or(0),
            )
        } else {
            (
                read_u32(bytes, at + 16, little).unwrap_or(0) as u64,
                read_u32(bytes, at + 20, little).unwrap_or(0) as u64,
                read_u32(bytes, at + 24, little).unwrap_or(0),
                read_u32(bytes, at + 36, little).unwrap_or(0) as u64,
            )
        };
        sections.push(Section {
            kind: read_u32(bytes, at + 4, little).unwrap_or(0),
            offset,
            size,
            link,
            entry_size,
        });
    }

    for section in &sections {
        if section.kind != 11 {
            continue;
        }
        let Some(strings) = sections.get(section.link as usize) else {
            continue;
        };
        let stride = if section.entry_size > 0 {
            section.entry_size as usize
        } else if bits == 64 {
            24
        } else {
            16
        };
        let total = (section.size as usize) / stride.max(1);
        for item in 0..total.min(5000) {
            let at = section.offset as usize + item * stride;
            let (name_at, info, shndx) = if bits == 64 {
                (
                    read_u32(bytes, at, little).unwrap_or(0) as usize,
                    bytes.get(at + 4).copied().unwrap_or(0),
                    read_u16(bytes, at + 6, little).unwrap_or(0),
                )
            } else {
                (
                    read_u32(bytes, at, little).unwrap_or(0) as usize,
                    bytes.get(at + 12).copied().unwrap_or(0),
                    read_u16(bytes, at + 14, little).unwrap_or(0),
                )
            };
            let kind = info & 0x0f;
            if kind != 1 && kind != 2 {
                continue;
            }
            if name_at == 0 {
                continue;
            }
            let Some(name) = c_string(bytes, strings.offset as usize + name_at, 512) else {
                continue;
            };
            let imported = shndx == 0;
            out.push(json!({
                "name": name,
                "kind": if kind == 2 { "func" } else { "object" },
                "imported": imported,
                "binding": match info >> 4 {
                    1 => "global",
                    2 => "weak",
                    _ => "local",
                },
                "sensitive": sensitive_of(&name),
            }));
        }
    }
    out
}

fn run_import_export(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let (path, size) = match target_of(ctx, args) {
        Ok(found) => found,
        Err(failure) => return failure,
    };
    let filter = args
        .get("filter")
        .and_then(Value::as_str)
        .unwrap_or("all")
        .to_string();
    let bytes = match read_head(&path, SCAN_LIMIT) {
        Ok(bytes) => bytes,
        Err(error) => return ToolResult::fail(error),
    };
    let meta = identify(&bytes, size);
    let format = meta
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let regions = regions(&bytes);

    let mut payload = json!({
        "path": path.display().to_string(),
        "size": size,
        "format": format,
        "scanned": bytes.len(),
    });

    match format.as_str() {
        "pe" => {
            let pe_offset = read_u32(&bytes, 0x3c, true).unwrap_or(0) as usize;
            let optional = pe_offset + 24;
            let magic = read_u16(&bytes, optional, true).unwrap_or(0x10b);
            let bits = if magic == 0x20b { 64u16 } else { 32 };
            let dir_base = if magic == 0x20b {
                optional + 112
            } else {
                optional + 96
            };
            let export_rva = read_u32(&bytes, dir_base, true).unwrap_or(0);
            let import_rva = read_u32(&bytes, dir_base + 8, true).unwrap_or(0);
            if filter != "exports" && import_rva != 0 {
                payload["imports"] = json!(pe_imports(&bytes, &regions, import_rva, bits));
            }
            if filter != "imports" && export_rva != 0 {
                payload["exports"] = pe_exports(&bytes, &regions, export_rva);
            }
        }
        "elf" => {
            let symbols = elf_symbols(&bytes);
            if filter != "exports" {
                let imports: Vec<&Value> = symbols
                    .iter()
                    .filter(|item| item.get("imported").and_then(Value::as_bool) == Some(true))
                    .collect();
                payload["imports"] = json!(imports);
            }
            if filter != "imports" {
                let exports: Vec<&Value> = symbols
                    .iter()
                    .filter(|item| item.get("imported").and_then(Value::as_bool) == Some(false))
                    .collect();
                payload["exports"] = json!(exports);
            }
        }
        _ => {
            return respond(
                ctx,
                "imports",
                json!({
                    "path": path.display().to_string(),
                    "size": size,
                    "format": format,
                    "note": "只支持 PE / ELF 的导入导出表；Mach-O 与 .NET 需要 Ghidra / dnlib（P2）",
                }),
            );
        }
    }

    // 汇总高危 API：模型扫一眼就知道该往哪儿看，不用自己遍历几千条。
    let mut flagged: Vec<String> = Vec::new();
    if let Some(imports) = payload.get("imports").and_then(Value::as_array) {
        for item in imports {
            if let Some(meaning) = item.get("sensitive").and_then(Value::as_str) {
                if !flagged.iter().any(|seen| seen == meaning) {
                    flagged.push(meaning.to_string());
                }
            }
            if let Some(detail) = item.get("detail").and_then(Value::as_array) {
                for function in detail {
                    if let Some(meaning) = function.get("sensitive").and_then(Value::as_str) {
                        if !flagged.iter().any(|seen| seen == meaning) {
                            flagged.push(meaning.to_string());
                        }
                    }
                }
            }
        }
    }
    payload["riskHints"] = json!(flagged);
    respond(ctx, "imports", payload)
}

// ---------------------------------------------------------------- 密码特征

/// 已知常量。用前 16-32 字节做指纹即可，不必比整表：
/// 命中一段连续常量已经足够可疑，而全表比对会把大样本的扫描时间拖长。
///
/// `bin.crypto.scan` 与 `bin.crypto.hash_identify` 共用这一张表：前者报
/// "样本里有 SHA-256 常量"，后者负责归到函数、看有没有循环。
pub(crate) const SIGNATURES: &[(&str, &[u8], &str, &str)] = &[
    (
        "aes-sbox",
        &[
            0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7,
            0xab, 0x76,
        ],
        "AES S 盒（很可能是 AES 查表实现）",
        "high",
    ),
    (
        "aes-inv-sbox",
        &[
            0x52, 0x09, 0x6a, 0xd5, 0x30, 0x36, 0xa5, 0x38, 0xbf, 0x40, 0xa3, 0x9e, 0x81, 0xf3,
            0xd7, 0xfb,
        ],
        "AES 逆 S 盒（解密方向查表）",
        "high",
    ),
    (
        "sha256-iv",
        &[
            0x6a, 0x09, 0xe6, 0x67, 0xbb, 0x67, 0xae, 0x85, 0x3c, 0x6e, 0xf3, 0x72, 0xa5, 0x4f,
            0xf5, 0x3a,
        ],
        "SHA-256 初始向量",
        "high",
    ),
    (
        "sha256-k",
        &[0x42, 0x8a, 0x2f, 0x98, 0x71, 0x37, 0x44, 0x91, 0xb5, 0xc0, 0xfb, 0xcf],
        "SHA-256 轮常量",
        "medium",
    ),
    (
        "sha1-iv",
        &[
            0x67, 0x45, 0x23, 0x01, 0xef, 0xcd, 0xab, 0x89, 0x98, 0xba, 0xdc, 0xfe, 0x10, 0x32,
            0x54, 0x76,
        ],
        "SHA-1 初始向量",
        "high",
    ),
    // SHA-512 的 IV / K 按内存字节序（小端）匹配：x86/ARM 上编译器把
    // 64 位常量按小端存成连续表，按大端写会一个都命中不了。
    (
        "sha512-iv",
        &[
            0x08, 0xc9, 0xbc, 0xf3, 0x67, 0xe6, 0x09, 0x6a, 0x3b, 0xa7, 0xca, 0x84, 0x85, 0xae,
            0x67, 0xbb,
        ],
        "SHA-512 初始向量",
        "high",
    ),
    (
        "sha512-iv-be",
        &[
            0x6a, 0x09, 0xe6, 0x67, 0xf3, 0xbc, 0xc9, 0x08, 0xbb, 0x67, 0xae, 0x85, 0x84, 0xca,
            0xa7, 0x3b,
        ],
        "SHA-512 初始向量（大端字节序）",
        "high",
    ),
    (
        "sha512-k",
        &[
            0x22, 0xae, 0x28, 0xd7, 0x98, 0x2f, 0x8a, 0x42, 0xcd, 0x65, 0xef, 0x23, 0x91, 0x44,
            0x37, 0x71,
        ],
        "SHA-512 轮常量",
        "medium",
    ),
    (
        "md5-iv",
        &[
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54,
            0x32, 0x10,
        ],
        "MD5 初始向量",
        "high",
    ),
    (
        "crc32-table",
        &[0x00, 0x00, 0x00, 0x00, 0x96, 0x30, 0x07, 0x77, 0x2c, 0x61, 0x0e, 0xee],
        "CRC32 查表",
        "medium",
    ),
    (
        "des-sbox",
        &[
            0x0e, 0x04, 0x0d, 0x01, 0x02, 0x0f, 0x0b, 0x08, 0x03, 0x0a, 0x06, 0x0c, 0x05, 0x09,
            0x00, 0x07,
        ],
        "DES S 盒",
        "medium",
    ),
    (
        "chacha-sigma",
        b"expand 32-byte k",
        "ChaCha20 / Salsa20 常量",
        "high",
    ),
    (
        "chacha-sigma16",
        b"expand 16-byte k",
        "ChaCha / Salsa 16 字节常量",
        "high",
    ),
];

fn run_crypto(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let (path, size) = match target_of(ctx, args) {
        Ok(found) => found,
        Err(failure) => return failure,
    };
    let bytes = match read_head(&path, SCAN_LIMIT) {
        Ok(bytes) => bytes,
        Err(error) => return ToolResult::fail(error),
    };

    let mut hits: Vec<Value> = Vec::new();
    for (name, pattern, note, confidence) in SIGNATURES {
        if pattern.is_empty() {
            continue;
        }
        let (found, _) = find_all(&bytes, pattern, 8);
        for offset in found {
            hits.push(json!({
                "algorithm": name,
                "offset": format!("0x{offset:x}"),
                "note": note,
                "confidence": confidence,
            }));
        }
    }

    // PEM 头是文本，单独搜：出现在二进制里通常意味着内嵌证书或私钥。
    for marker in ["-----BEGIN PUBLIC KEY-----", "-----BEGIN RSA PRIVATE KEY-----"] {
        let (found, _) = find_all(&bytes, marker.as_bytes(), 4);
        for offset in found {
            hits.push(json!({
                "algorithm": "pem",
                "offset": format!("0x{offset:x}"),
                "note": format!("内嵌 PEM：{marker}"),
                "confidence": "high",
            }));
        }
    }

    // RC4 KSA 的 0..255 递增表。误报率高（很多查表都是递增的），
    // 所以只在前 32 字节完全对得上时报，并且标 low。
    let ramp: Vec<u8> = (0..=255u8).collect();
    let (ramp_hits, _) = find_all(&bytes, &ramp[..32], 4);
    for offset in ramp_hits {
        hits.push(json!({
            "algorithm": "rc4-ksa",
            "offset": format!("0x{offset:x}"),
            "note": "0..255 递增表，可能是 RC4 KSA 或普通查表",
            "confidence": "low",
        }));
    }

    let mut payload = json!({
        "path": path.display().to_string(),
        "size": size,
        "scanned": bytes.len(),
        "hits": hits.len(),
        "matches": hits,
    });

    if let Some(probe) = args.get("xorProbe").and_then(Value::as_str).filter(|value| !value.is_empty()) {
        let plain = probe.as_bytes();
        let mut tried = Vec::new();
        for key in 1u16..=255 {
            let key = key as u8;
            let needle: Vec<u8> = plain.iter().map(|byte| byte ^ key).collect();
            let (found, _) = find_all(&bytes, &needle, 4);
            for offset in found {
                tried.push(json!({
                    "key": key,
                    "offset": format!("0x{offset:x}"),
                    "sample": probe,
                }));
            }
        }
        payload["xor"] = json!({
            "probe": probe,
            "keyCount": tried.len(),
            "matches": tried,
            "note": if tried.is_empty() { "没有单字节 XOR 命中" } else { "命中多把密钥时，取命中位置集中的那一把" },
        });
    }

    respond(ctx, "crypto", payload)
}

// ---------------------------------------------------------------- 证书与密钥
//
// 手写 DER TLV 递归下降，不引 x509-parser / ring / rsa。只读结构，不做密码学
// 运算：不验签、不解密、不导出私钥字段。识别不出就报 unknown + 裸 OID。

/// 顶层 / 内层的 DER TLV。
///
/// `class` 与 `constructed` 保留首字节的高位：`[0] EXPLICIT` 这类上下文标签
/// 光看 `tag` 会和 universal 的 SEQUENCE 混在一起。
struct Tlv<'a> {
    class: u8,
    tag: u32,
    value: &'a [u8],
}

impl Tlv<'_> {
    fn is_universal(&self, tag: u32) -> bool {
        self.class == 0 && self.tag == tag
    }

    fn is_context(&self, tag: u32) -> bool {
        self.class == 2 && self.tag == tag
    }
}

/// 读一个 TLV，返回它和下一个字节的位置。
///
/// 长 tag 号与 0x81..0x84 长长度都支持；不定长（0x80）在 DER 里非法，直接报错。
/// 所有越界都走 `Err`，畸形输入不会 panic。
fn read_tlv(bytes: &[u8], at: usize) -> Result<(Tlv<'_>, usize), String> {
    let first = *bytes.get(at).ok_or_else(|| format!("DER 在 0x{at:x} 处截断"))?;
    let class = first >> 6;
    let mut cursor = at + 1;
    let mut tag = (first & 0x1f) as u32;
    if tag == 0x1f {
        tag = 0;
        let mut used = 0;
        loop {
            let byte = *bytes
                .get(cursor)
                .ok_or_else(|| "DER 高 tag 号截断".to_string())?;
            cursor += 1;
            used += 1;
            tag = (tag << 7) | (byte & 0x7f) as u32;
            if byte & 0x80 == 0 {
                break;
            }
            if used >= 4 {
                return Err("DER 高 tag 号过长".to_string());
            }
        }
    }
    let length_byte = *bytes
        .get(cursor)
        .ok_or_else(|| "DER 长度字段截断".to_string())?;
    cursor += 1;
    let length = if length_byte & 0x80 == 0 {
        length_byte as usize
    } else {
        let count = (length_byte & 0x7f) as usize;
        if count == 0 {
            return Err("DER 不允许不定长编码".to_string());
        }
        if count > 4 {
            return Err("DER 长度字段超过 4 字节".to_string());
        }
        let mut length = 0usize;
        for _ in 0..count {
            let byte = *bytes
                .get(cursor)
                .ok_or_else(|| "DER 长度字段截断".to_string())?;
            cursor += 1;
            length = (length << 8) | byte as usize;
        }
        length
    };
    let end = cursor
        .checked_add(length)
        .ok_or_else(|| "DER 长度溢出".to_string())?;
    if end > bytes.len() {
        return Err(format!(
            "DER 值截断：声明 {length} 字节，实际只剩 {}",
            bytes.len() - cursor
        ));
    }
    Ok((
        Tlv {
            class,
            tag,
            value: &bytes[cursor..end],
        },
        end,
    ))
}

fn children<'a>(tlv: &Tlv<'a>) -> Result<Vec<Tlv<'a>>, String> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < tlv.value.len() {
        let (child, next) = read_tlv(tlv.value, at)?;
        out.push(child);
        at = next;
    }
    Ok(out)
}

/// 常用 OID 的名字。只收证书 / 密钥里真会遇到的，未命中就返回裸 OID。
const OID_NAMES: &[(&str, &str)] = &[
    ("1.2.840.113549.1.1.1", "rsaEncryption"),
    ("1.2.840.113549.1.1.2", "md2WithRSAEncryption"),
    ("1.2.840.113549.1.1.4", "md5WithRSAEncryption"),
    ("1.2.840.113549.1.1.5", "sha1WithRSAEncryption"),
    ("1.2.840.113549.1.1.10", "rsassaPss"),
    ("1.2.840.113549.1.1.11", "sha256WithRSAEncryption"),
    ("1.2.840.113549.1.1.12", "sha384WithRSAEncryption"),
    ("1.2.840.113549.1.1.13", "sha512WithRSAEncryption"),
    ("1.2.840.113549.1.1.14", "sha224WithRSAEncryption"),
    ("1.2.840.10045.2.1", "id-ecPublicKey"),
    ("1.2.840.10045.4.1", "ecdsa-with-SHA1"),
    ("1.2.840.10045.4.3.2", "ecdsa-with-SHA256"),
    ("1.2.840.10045.4.3.3", "ecdsa-with-SHA384"),
    ("1.2.840.10045.4.3.4", "ecdsa-with-SHA512"),
    ("1.2.840.10045.3.1.7", "prime256v1 (P-256)"),
    ("1.3.132.0.34", "secp384r1 (P-384)"),
    ("1.3.132.0.35", "secp521r1 (P-521)"),
    ("1.3.132.0.10", "secp256k1"),
    ("1.3.101.112", "Ed25519"),
    ("1.3.101.113", "Ed448"),
    ("1.2.840.113549.1.7.2", "signedData"),
    ("1.2.840.113549.1.7.1", "data"),
    ("1.3.14.3.2.26", "sha1"),
    ("2.16.840.1.101.3.4.2.1", "sha256"),
    ("2.16.840.1.101.3.4.2.2", "sha384"),
    ("2.16.840.1.101.3.4.2.3", "sha512"),
];

/// DN 里属性的短名（`C=CN, O=...` 这种写法）。
const DN_LABELS: &[(&str, &str)] = &[
    ("2.5.4.3", "CN"),
    ("2.5.4.4", "SN"),
    ("2.5.4.5", "serialNumber"),
    ("2.5.4.6", "C"),
    ("2.5.4.7", "L"),
    ("2.5.4.8", "ST"),
    ("2.5.4.10", "O"),
    ("2.5.4.11", "OU"),
    ("2.5.4.42", "GN"),
    ("1.2.840.113549.1.9.1", "E"),
];

fn lookup(table: &'static [(&'static str, &'static str)], key: &str) -> Option<&'static str> {
    table
        .iter()
        .find(|(oid, _)| *oid == key)
        .map(|(_, name)| *name)
}

fn oid_name(oid: &str) -> Option<&'static str> {
    lookup(OID_NAMES, oid)
}

/// OID 内容字节 → 点分字符串。首字节是 40*X+Y，之后是 base-128 变长。
fn oid_text(value: &[u8]) -> Result<String, String> {
    let first = *value.first().ok_or_else(|| "OID 为空".to_string())?;
    let (root, second) = if first < 40 {
        (0u32, first as u32)
    } else if first < 80 {
        (1u32, first as u32 - 40)
    } else {
        (2u32, first as u32 - 80)
    };
    let mut parts = vec![root.to_string(), second.to_string()];
    let mut current = 0u64;
    let mut open = false;
    for byte in &value[1..] {
        current = (current << 7) | (byte & 0x7f) as u64;
        open = true;
        if byte & 0x80 == 0 {
            parts.push(current.to_string());
            current = 0;
            open = false;
        }
    }
    if open {
        return Err("OID 末尾截断".to_string());
    }
    Ok(parts.join("."))
}

/// 无符号大端整数 → 去掉前导 0 的幅值。DER INTEGER 有符号，正数最多带一个 0x00。
fn integer_bytes(value: &[u8]) -> &[u8] {
    match value.iter().position(|byte| *byte != 0) {
        Some(start) => &value[start..],
        None => &value[value.len()..],
    }
}

/// 精确位长：去掉前导 0 后，按最高有效字节的实际位数算。
fn bit_len(value: &[u8]) -> usize {
    let magnitude = integer_bytes(value);
    match magnitude.first() {
        Some(first) => (magnitude.len() - 1) * 8 + (8 - first.leading_zeros() as usize),
        None => 0,
    }
}

/// 无符号整数 → 十六进制串（全零给 `00`，不输出空的 `0x`）。
fn hex_uint(value: &[u8]) -> String {
    let magnitude = integer_bytes(value);
    if magnitude.is_empty() {
        "00".to_string()
    } else {
        hex(magnitude)
    }
}

fn small_uint(value: &[u8]) -> Option<u64> {
    if value.len() > 8 {
        return None;
    }
    Some(
        value
            .iter()
            .fold(0u64, |acc, byte| (acc << 8) | *byte as u64),
    )
}

fn key_algorithm(oid: &str) -> &'static str {
    match oid {
        "1.2.840.113549.1.1.1" => "rsa",
        "1.2.840.10045.2.1" => "ec",
        "1.3.101.112" => "ed25519",
        "1.3.101.113" => "ed448",
        _ => "unknown",
    }
}

fn curve_bits(oid: &str) -> Option<u32> {
    match oid {
        "1.2.840.10045.3.1.7" => Some(256),
        "1.3.132.0.34" => Some(384),
        "1.3.132.0.35" => Some(521),
        "1.3.132.0.10" => Some(256),
        _ => None,
    }
}

/// AlgorithmIdentifier ::= SEQUENCE { OID, 参数 OPTIONAL }。未命中给裸 OID。
fn algorithm_name(tlv: &Tlv<'_>) -> String {
    let oid = children(tlv)
        .ok()
        .and_then(|parts| parts.first().and_then(|item| oid_text(item.value).ok()));
    match oid {
        Some(oid) => match oid_name(&oid) {
            Some(name) => name.to_string(),
            None => oid,
        },
        None => "unknown".to_string(),
    }
}

fn time_text(tlv: &Tlv<'_>) -> String {
    if let Some(text) = parse_der_time(tlv) {
        return text;
    }
    String::from_utf8_lossy(tlv.value).trim().to_string()
}

/// UTCTime（YYMMDDHHMM[SS]Z）与 GeneralizedTime（YYYYMMDDHHMM[SS][.f]Z）→ ISO 8601。
/// 解析不了返回 None，调用方保留原始字符串——不猜时区。
fn parse_der_time(tlv: &Tlv<'_>) -> Option<String> {
    let generalized = match tlv.tag {
        0x17 => false,
        0x18 => true,
        _ => return None,
    };
    let raw = String::from_utf8_lossy(tlv.value);
    let digits: String = raw.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    let base = if generalized { 4 } else { 2 };
    if digits.len() < base + 8 {
        return None;
    }
    let year: u32 = digits[..base].parse().ok()?;
    let year = if generalized {
        year
    } else if year >= 50 {
        1900 + year
    } else {
        2000 + year
    };
    let month = &digits[base..base + 2];
    let day = &digits[base + 2..base + 4];
    let hour = &digits[base + 4..base + 6];
    let minute = &digits[base + 6..base + 8];
    let second = if digits.len() >= base + 10 {
        &digits[base + 8..base + 10]
    } else {
        "00"
    };
    Some(format!("{year:04}-{month}-{day}T{hour}:{minute}:{second}Z"))
}

/// Name ::= SEQUENCE OF SET OF SEQUENCE { OID, 值 }。
fn decode_name(tlv: &Tlv<'_>) -> Option<String> {
    let rdns = children(tlv).ok()?;
    let mut parts = Vec::new();
    for set in rdns {
        for attribute in children(&set).unwrap_or_default() {
            let pair = children(&attribute).unwrap_or_default();
            if pair.len() < 2 {
                continue;
            }
            let oid = match oid_text(pair[0].value) {
                Ok(oid) => oid,
                Err(_) => continue,
            };
            let label = lookup(DN_LABELS, &oid).unwrap_or(&oid);
            let value = string_of(&pair[1]).unwrap_or_else(|| hex(pair[1].value));
            parts.push(format!("{label}={value}"));
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

fn string_of(tlv: &Tlv<'_>) -> Option<String> {
    match tlv.tag {
        0x0c | 0x13 | 0x16 | 0x14 | 0x12 => Some(String::from_utf8_lossy(tlv.value).to_string()),
        0x1e => {
            // BMPString 是 UTF-16BE。
            if tlv.value.len() % 2 != 0 {
                return None;
            }
            let units: Vec<u16> = tlv
                .value
                .chunks(2)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                .collect();
            Some(String::from_utf16_lossy(&units))
        }
        _ => None,
    }
}

/// SubjectPublicKeyInfo ::= SEQUENCE { AlgorithmIdentifier, BIT STRING }。
fn public_key(tlv: &Tlv<'_>) -> Option<Value> {
    let parts = children(tlv).ok()?;
    let alg = parts.first()?;
    let alg_fields = children(alg).ok()?;
    let oid = alg_fields
        .first()
        .and_then(|item| oid_text(item.value).ok())?;
    let bits = parts.get(1)?;
    if !bits.is_universal(0x03) {
        return None;
    }
    // BIT STRING 首字节是「未使用位数」，密钥本体从第二字节起。
    let payload = bits.value.get(1..)?;
    let kind = key_algorithm(&oid);
    let mut out = serde_json::Map::new();
    out.insert("algorithm".to_string(), json!(kind));
    match kind {
        "rsa" => {
            if let Ok((rsa, _)) = read_tlv(payload, 0) {
                if let Ok(fields) = children(&rsa) {
                    if let Some(modulus) = fields.first() {
                        out.insert("bits".to_string(), json!(bit_len(modulus.value)));
                    }
                    if let Some(e) = fields.get(1).and_then(|item| small_uint(item.value)) {
                        out.insert("e".to_string(), json!(e));
                    }
                }
            }
        }
        "ec" => {
            let curve = alg_fields
                .get(1)
                .filter(|item| item.is_universal(0x06))
                .and_then(|item| oid_text(item.value).ok());
            match curve.as_deref() {
                Some(oid) => match oid_name(oid) {
                    Some(name) => {
                        out.insert("curve".to_string(), json!(name));
                    }
                    None => {
                        out.insert("curve".to_string(), json!(oid));
                    }
                },
                None => {}
            }
            // 未压缩点是 0x04 || X || Y，位长 = 坐标字节数 × 8。
            let point_bits = if payload.first() == Some(&0x04)
                && payload.len() > 1
                && (payload.len() - 1) % 2 == 0
            {
                Some(((payload.len() - 1) * 4) as u32)
            } else {
                None
            };
            let bits = curve.as_deref().and_then(curve_bits).or(point_bits);
            if let Some(bits) = bits {
                out.insert("bits".to_string(), json!(bits));
            }
        }
        "ed25519" => {
            out.insert("bits".to_string(), json!(256));
        }
        "ed448" => {
            out.insert("bits".to_string(), json!(456));
        }
        _ => {
            out.insert("oid".to_string(), json!(oid));
        }
    }
    Some(Value::Object(out))
}

fn json_algorithm(oid: &str) -> Value {
    let kind = key_algorithm(oid);
    if kind == "unknown" {
        json!({ "algorithm": kind, "oid": oid })
    } else {
        json!({ "algorithm": kind })
    }
}

fn private_note() -> &'static str {
    "私钥内容不导出、不进上下文"
}

fn rsa_public(items: &[Tlv<'_>]) -> Value {
    let mut out = json!({
        "type": "rsa-public",
        "algorithm": "rsa",
        "bits": bit_len(items[0].value),
    });
    if let Some(e) = items.get(1).and_then(|item| small_uint(item.value)) {
        out["e"] = json!(e);
    }
    out
}

fn rsa_private(modulus: &Tlv<'_>, exponent: Option<&Tlv<'_>>) -> Value {
    let mut out = json!({
        "type": "rsa-private",
        "algorithm": "rsa",
        "bits": bit_len(modulus.value),
        "note": private_note(),
    });
    if let Some(e) = exponent.and_then(|item| small_uint(item.value)) {
        out["e"] = json!(e);
    }
    out
}

fn ec_private(curve: Option<String>) -> Value {
    let mut out = json!({
        "type": "ec-private",
        "algorithm": "ec",
        "note": private_note(),
    });
    if let Some(oid) = curve {
        out["curve"] = json!(oid_name(&oid).unwrap_or(&oid));
        if let Some(bits) = curve_bits(&oid) {
            out["bits"] = json!(bits);
        }
    }
    out
}

/// SEC1 ECPrivateKey ::= SEQUENCE { version INTEGER, privateKey OCTET STRING,
/// [0] 曲线 OID OPTIONAL, [1] 公钥 BIT STRING OPTIONAL }。
fn sec1_curve(items: &[Tlv<'_>]) -> Option<String> {
    items
        .iter()
        .find(|item| item.is_context(0))
        .and_then(|item| read_tlv(item.value, 0).ok())
        .and_then(|(inner, _)| {
            if inner.is_universal(0x06) {
                oid_text(inner.value).ok()
            } else {
                None
            }
        })
}

fn pkcs8_private(items: &[Tlv<'_>]) -> Result<Value, String> {
    let alg = children(&items[1])?;
    let oid = alg
        .first()
        .and_then(|item| oid_text(item.value).ok())
        .unwrap_or_default();
    let inner = items[2].value;
    match oid.as_str() {
        "1.2.840.113549.1.1.1" => {
            let (pkcs1, _) =
                read_tlv(inner, 0).map_err(|_| "PKCS#8 里的 RSA 私钥解不开".to_string())?;
            let fields = children(&pkcs1)?;
            let modulus = fields
                .get(1)
                .ok_or_else(|| "PKCS#8 里的 RSA 私钥缺模数".to_string())?;
            Ok(rsa_private(modulus, fields.get(2)))
        }
        "1.2.840.10045.2.1" => {
            let from_params = alg
                .get(1)
                .filter(|item| item.is_universal(0x06))
                .and_then(|item| oid_text(item.value).ok());
            let from_inner = read_tlv(inner, 0)
                .ok()
                .and_then(|(sec1, _)| children(&sec1).ok())
                .and_then(|fields| sec1_curve(&fields));
            Ok(ec_private(from_params.or(from_inner)))
        }
        "1.3.101.112" => Ok(json!({
            "type": "ed25519-private",
            "algorithm": "ed25519",
            "bits": 256,
            "note": private_note(),
        })),
        "1.3.101.113" => Ok(json!({
            "type": "ed448-private",
            "algorithm": "ed448",
            "bits": 456,
            "note": private_note(),
        })),
        other => {
            let mut out = json_algorithm(other);
            out["type"] = json!("pkcs8-private");
            out["note"] = json!(private_note());
            Ok(out)
        }
    }
}

/// 证书主体：TBSCertificate + 外层签名算法 + 签名值。
///
/// `items` 是外层 SEQUENCE 的三个子项，`der` 是这份证书的完整 DER（算指纹用）。
fn describe_certificate(items: &[Tlv<'_>], der: &[u8]) -> Result<Value, String> {
    if items.len() < 3 {
        return Err("证书条目不足".to_string());
    }
    let tbs = children(&items[0])?;
    let mut at = 0usize;
    let version = if tbs.first().map(|item| item.is_context(0)).unwrap_or(false) {
        at = 1;
        match tbs
            .first()
            .and_then(|item| children(item).ok())
            .and_then(|inner| inner.first().and_then(|item| small_uint(item.value)))
        {
            Some(0) => "v1",
            Some(1) => "v2",
            Some(2) => "v3",
            _ => "unknown",
        }
    } else {
        "v1"
    };
    let serial = tbs.get(at).ok_or_else(|| "证书缺少序列号".to_string())?;
    let signature = tbs
        .get(at + 1)
        .ok_or_else(|| "证书缺少签名算法".to_string())?;
    let issuer = tbs.get(at + 2).ok_or_else(|| "证书缺少签发者".to_string())?;
    let validity = tbs
        .get(at + 3)
        .ok_or_else(|| "证书缺少有效期".to_string())?;
    let subject = tbs.get(at + 4).ok_or_else(|| "证书缺少主体".to_string())?;
    let spki = tbs.get(at + 5).ok_or_else(|| "证书缺少公钥".to_string())?;
    if !serial.is_universal(0x02)
        || !signature.is_universal(0x10)
        || !issuer.is_universal(0x10)
        || !validity.is_universal(0x10)
        || !subject.is_universal(0x10)
        || !spki.is_universal(0x10)
    {
        return Err("TBSCertificate 字段顺序与预期不符".to_string());
    }

    let mut out = serde_json::Map::new();
    out.insert("type".to_string(), json!("x509"));
    out.insert("version".to_string(), json!(version));
    out.insert(
        "serial".to_string(),
        json!(format!("0x{}", hex_uint(serial.value))),
    );
    if let Some(name) = decode_name(issuer) {
        out.insert("issuer".to_string(), json!(name));
    }
    if let Some(name) = decode_name(subject) {
        out.insert("subject".to_string(), json!(name));
    }
    if let Ok(fields) = children(validity) {
        if let Some(not_before) = fields.first() {
            out.insert("validNotBefore".to_string(), json!(time_text(not_before)));
        }
        if let Some(not_after) = fields.get(1) {
            out.insert("validNotAfter".to_string(), json!(time_text(not_after)));
        }
    }
    out.insert(
        "signatureAlgorithm".to_string(),
        json!(algorithm_name(&items[1])),
    );
    if let Some(key) = public_key(spki) {
        out.insert("publicKey".to_string(), key);
    }
    out.insert(
        "fingerprintSha256".to_string(),
        json!(colon_hex(&sha256_of(der))),
    );
    Ok(Value::Object(out))
}

fn describe_pkcs7(explicit: &Tlv<'_>) -> Result<Value, String> {
    if !explicit.is_context(0) {
        return Err("signedData 不是 [0] EXPLICIT 包裹".to_string());
    }
    let (signed, _) = read_tlv(explicit.value, 0)?;
    let fields = children(&signed)?;
    let mut certificates = Vec::new();
    for field in &fields {
        if !field.is_context(0) {
            continue;
        }
        // [0] IMPLICIT SET OF Certificate：内容直接是证书的 SEQUENCE。
        let mut at = 0usize;
        while at < field.value.len() {
            let (cert, next) = read_tlv(field.value, at)?;
            if cert.is_universal(0x10) {
                if let Ok(cert_items) = children(&cert) {
                    if let Ok(described) =
                        describe_certificate(&cert_items, &field.value[at..next])
                    {
                        certificates.push(described);
                    }
                }
            }
            at = next;
        }
    }
    Ok(json!({
        "type": "pkcs7",
        "certificateCount": certificates.len(),
        "certificates": certificates,
        "note": if certificates.is_empty() {
            "signedData 里没有可解析的证书"
        } else {
            "APK 的 CERT.RSA / CERT.EC 通常就是这层 PKCS#7，证书在 certificates 里"
        },
    }))
}

/// 按最外层 SEQUENCE 的结构判别类型。判不出来就 `unknown`，不编名字。
fn describe_der(der: &[u8]) -> Result<Value, String> {
    let (top, top_end) = read_tlv(der, 0)?;
    if !top.is_universal(0x10) {
        return Err(format!("顶层不是 SEQUENCE（tag=0x{:x}）", top.tag));
    }
    let items = children(&top)?;
    let first = match items.first() {
        Some(item) => item,
        None => return Err("DER SEQUENCE 为空".to_string()),
    };

    // X.509：SEQUENCE(tbs) / SEQUENCE(sigAlg) / BIT STRING(sig)
    if items.len() == 3
        && first.is_universal(0x10)
        && items[1].is_universal(0x10)
        && items[2].is_universal(0x03)
    {
        if let Ok(cert) = describe_certificate(&items, &der[..top_end]) {
            return Ok(cert);
        }
    }

    // PKCS#7 ContentInfo ::= SEQUENCE { contentType OID, [0] EXPLICIT 内容 }
    if items.len() >= 2 && first.is_universal(0x06) {
        if let Ok(oid) = oid_text(first.value) {
            if oid == "1.2.840.113549.1.7.2" {
                return describe_pkcs7(&items[1]);
            }
        }
    }

    // PKCS#1 RSAPublicKey ::= SEQUENCE { n INTEGER, e INTEGER }
    if items.len() == 2 && first.is_universal(0x02) && items[1].is_universal(0x02) {
        return Ok(rsa_public(&items));
    }

    // PKCS#1 RSAPrivateKey：version + 至少八个 INTEGER（两素数版本是九个）
    if items.len() >= 9
        && first.is_universal(0x02)
        && items[1].is_universal(0x02)
        && items[2].is_universal(0x02)
    {
        return Ok(rsa_private(&items[1], Some(&items[2])));
    }

    // PKCS#8 PrivateKeyInfo ::= SEQUENCE { version, AlgorithmIdentifier, OCTET STRING, ... }
    if (items.len() == 3 || items.len() == 4)
        && first.is_universal(0x02)
        && items[1].is_universal(0x10)
        && items[2].is_universal(0x04)
    {
        return pkcs8_private(&items);
    }

    // SubjectPublicKeyInfo ::= SEQUENCE { AlgorithmIdentifier, BIT STRING }
    if items.len() == 2 && first.is_universal(0x10) && items[1].is_universal(0x03) {
        let key = public_key(&top).ok_or_else(|| "SPKI 公钥解析失败".to_string())?;
        return Ok(json!({ "type": "public-key", "publicKey": key }));
    }

    // SEC1 ECPrivateKey ::= SEQUENCE { version=1, OCTET STRING, [0] 曲线 OID? }
    if items.len() >= 2
        && first.is_universal(0x02)
        && small_uint(first.value) == Some(1)
        && items[1].is_universal(0x04)
    {
        return Ok(ec_private(sec1_curve(&items)));
    }

    Ok(json!({
        "type": "unknown",
        "topLevelTags": items
            .iter()
            .map(|item| format!("{}-{}", item.class, item.tag))
            .collect::<Vec<_>>(),
        "note": "结构没有匹配到已知的证书/密钥模板，不猜类型",
    }))
}

fn colon_hex(text: &str) -> String {
    text.as_bytes()
        .chunks(2)
        .map(|pair| std::str::from_utf8(pair).unwrap_or(""))
        .collect::<Vec<&str>>()
        .join(":")
}

fn parse_hex_offset(raw: &str) -> Result<usize, String> {
    let text = raw.trim();
    let digits = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .unwrap_or(text);
    usize::from_str_radix(digits, 16).map_err(|_| format!("offset 不是十六进制：{raw}"))
}

/// PEM 只可能在文件前面（或用户给的 offset 处）；窗口卡在 4 KiB 内，
/// 免得在一份二进制里随手找到一段 PEM 就当成本次输入。
fn sniff_pem(bytes: &[u8]) -> Option<Result<(String, Vec<u8>), String>> {
    const HEAD_WINDOW: usize = 4096;
    /// 一张证书 / 一把私钥的 PEM 远小于这个数；卡住是为了不在大二进制上
    /// 把整个尾部都做一遍 UTF-8 转换。
    const PEM_LIMIT: usize = 1024 * 1024;
    let window = &bytes[..bytes.len().min(HEAD_WINDOW + 11)];
    let (found, _) = find_all(window, b"-----BEGIN ", 1);
    let start = *found.first()?;
    let end = (start + PEM_LIMIT).min(bytes.len());
    Some(decode_pem(&bytes[start..end]))
}

fn decode_pem(bytes: &[u8]) -> Result<(String, Vec<u8>), String> {
    let text = String::from_utf8_lossy(bytes);
    let mut lines = text.lines();
    let header = lines.next().unwrap_or("").trim().to_string();
    let label = header
        .strip_prefix("-----BEGIN ")
        .and_then(|rest| rest.strip_suffix("-----"))
        .ok_or_else(|| format!("PEM 头格式不对：{header}"))?
        .to_string();
    let mut body = String::new();
    let mut encrypted = false;
    let mut closed = false;
    for line in lines {
        let line = line.trim();
        if line.starts_with("-----END ") {
            closed = true;
            break;
        }
        if line.is_empty() {
            continue;
        }
        if line.starts_with("Proc-Type:") && line.contains("ENCRYPTED") {
            encrypted = true;
            continue;
        }
        if line.contains(':') {
            // DEK-Info 之类的 PEM 头，不是 base64 主体。
            continue;
        }
        body.push_str(line);
    }
    if encrypted {
        return Err("这是需要口令的加密 PEM，不支持".to_string());
    }
    if !closed {
        return Err("PEM 缺少 -----END 行（可能被截断）".to_string());
    }
    let der = base64::engine::general_purpose::STANDARD
        .decode(body.as_bytes())
        .map_err(|error| format!("PEM base64 解不开：{error}"))?;
    Ok((label, der))
}

fn run_asn1(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let (path, size) = match target_of(ctx, args) {
        Ok(found) => found,
        Err(failure) => return failure,
    };
    let bytes = match read_head(&path, SCAN_LIMIT) {
        Ok(bytes) => bytes,
        Err(error) => return ToolResult::fail(error),
    };
    let offset = match args
        .get("offset")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(raw) => match parse_hex_offset(raw) {
            Ok(offset) => offset,
            Err(error) => return ToolResult::fail(error),
        },
        None => 0,
    };
    if offset >= bytes.len() {
        return ToolResult::fail(format!(
            "offset 0x{offset:x} 超出文件（共 {} 字节）",
            bytes.len()
        ));
    }
    let slice = &bytes[offset..];
    let (container, label, der): (&str, Option<String>, std::borrow::Cow<'_, [u8]>) =
        match sniff_pem(slice) {
            Some(Ok((label, body))) => ("pem", Some(label), std::borrow::Cow::Owned(body)),
            Some(Err(error)) => return ToolResult::fail(error),
            None => ("der", None, std::borrow::Cow::Borrowed(slice)),
        };
    let mut payload = match describe_der(&der) {
        Ok(value) => value,
        Err(error) => {
            return ToolResult::fail(format!(
                "不是可解析的证书/密钥 DER：{error}。若证书内嵌在二进制里，用 offset 指到起点"
            ))
        }
    };
    payload["path"] = json!(path.display().to_string());
    payload["size"] = json!(size);
    payload["offset"] = json!(format!("0x{offset:x}"));
    payload["container"] = json!(container);
    if let Some(label) = label {
        payload["pemLabel"] = json!(label);
    }
    if size > bytes.len() as u64 {
        payload["truncated"] = json!(true);
    }
    respond(ctx, "crypto", payload)
}

// ---------------------------------------------------------------- 多字节 XOR

/// 密钥上限：AES-128 的 16 字节恰好压线，再长就没有实用价值了。
const XOR_KEY_MAX: u32 = 16;
/// 输出条目 / 单把密钥的偏移上限。
const XOR_MATCH_MAX: usize = 64;
const XOR_OFFSET_MAX: usize = 32;
/// 未验证候选（probe 不比密钥长）只留重复出现的，且最多收集这么多种，
/// 否则随机数据在每个偏移都会产出一个"候选"，输出会失去意义。
const XOR_UNVERIFIED_MAX: usize = 4096;

struct XorHit {
    key: Vec<u8>,
    offsets: Vec<usize>,
    confidence: &'static str,
    verified: bool,
    partial: bool,
    dropped: bool,
}

fn confidence_rank(value: &str) -> u8 {
    match value {
        "certain" => 2,
        "high" => 1,
        _ => 0,
    }
}

fn run_xor_brute(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let (path, size) = match target_of(ctx, args) {
        Ok(found) => found,
        Err(failure) => return failure,
    };
    let probe = match args
        .get("probe")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(probe) => probe,
        None => return ToolResult::fail("需要 probe：一段已知明文，如 http:// 或 android.intent"),
    };
    let plain = probe.as_bytes();
    let min_len = args
        .get("minLen")
        .and_then(Value::as_u64)
        .map(|value| (value as u32).clamp(1, XOR_KEY_MAX))
        .unwrap_or(1);
    let max_len = args
        .get("maxLen")
        .and_then(Value::as_u64)
        .map(|value| (value as u32).clamp(1, XOR_KEY_MAX))
        .unwrap_or(8);
    if min_len > max_len {
        return ToolResult::fail(format!("minLen({min_len}) 不能大于 maxLen({max_len})"));
    }
    let bytes = match read_head(&path, SCAN_LIMIT) {
        Ok(bytes) => bytes,
        Err(error) => return ToolResult::fail(error),
    };
    if bytes.len() < plain.len() {
        return ToolResult::fail(format!(
            "样本只有 {} 字节，比 probe（{} 字节）还短",
            bytes.len(),
            plain.len()
        ));
    }

    let mut hits: Vec<XorHit> = Vec::new();
    let mut index: HashMap<Vec<u8>, usize> = HashMap::new();
    let mut capped = false;

    for key_len in min_len..=max_len {
        let key_len = key_len as usize;
        // probe 比密钥短时，只能恢复密钥的前 plain.len() 个字节。
        let known = key_len.min(plain.len());
        if known == 0 || known > bytes.len() {
            continue;
        }
        let mut unverified: HashMap<Vec<u8>, Vec<usize>> = HashMap::new();
        let mut overflow = false;
        for at in 0..=bytes.len() - known {
            let check_len = plain.len().min(bytes.len() - at);
            if check_len > key_len {
                // 先用第 key_len 个字节做一次 O(1) 过滤，只有极少数位置需要
                // 真正推导完整密钥。密钥是周期性的，key[key_len] = key[0]。
                if bytes[at + key_len] ^ bytes[at] ^ plain[0] != plain[key_len] {
                    continue;
                }
                let key: Vec<u8> = (0..key_len)
                    .map(|i| bytes[at + i] ^ plain[i])
                    .collect();
                let matched = (key_len..check_len)
                    .all(|i| bytes[at + i] ^ key[i % key_len] == plain[i]);
                if !matched {
                    continue;
                }
                let confidence = if check_len == plain.len() {
                    "certain"
                } else {
                    "high"
                };
                record_hit(
                    &mut hits,
                    &mut index,
                    key,
                    at,
                    confidence,
                    true,
                    false,
                    &mut capped,
                );
            } else {
                // 没有多余字节可验：随机数据在这里个个都「命中」，只保留
                // 在同一段数据里重复出现的候选，并标 low。
                let key: Vec<u8> = (0..known).map(|i| bytes[at + i] ^ plain[i]).collect();
                if !unverified.contains_key(&key) {
                    if unverified.len() >= XOR_UNVERIFIED_MAX {
                        overflow = true;
                        break;
                    }
                }
                let entry = unverified.entry(key).or_default();
                if entry.len() < XOR_OFFSET_MAX {
                    entry.push(at);
                }
            }
        }
        if overflow {
            capped = true;
        }
        let mut repeated: Vec<(Vec<u8>, Vec<usize>)> = unverified
            .into_iter()
            .filter(|(_, offsets)| offsets.len() >= 2)
            .collect();
        repeated.sort_by_key(|(_, offsets)| offsets[0]);
        for (key, offsets) in repeated {
            let partial = plain.len() < key_len;
            for at in offsets {
                record_hit(
                    &mut hits,
                    &mut index,
                    key.clone(),
                    at,
                    "low",
                    false,
                    partial,
                    &mut capped,
                );
            }
        }
    }

    hits.sort_by(|a, b| {
        confidence_rank(b.confidence)
            .cmp(&confidence_rank(a.confidence))
            .then(b.offsets.len().cmp(&a.offsets.len()))
            .then(a.partial.cmp(&b.partial))
            .then(a.key.len().cmp(&b.key.len()))
            .then(a.offsets.first().cmp(&b.offsets.first()))
    });

    let rendered: Vec<Value> = hits
        .iter()
        .map(|hit| {
            let printable = hit
                .key
                .iter()
                .all(|byte| byte.is_ascii_graphic() || *byte == b' ' || *byte == b'\t');
            let mut item = json!({
                "key": hit.key,
                "keyLen": hit.key.len(),
                "keyStr": if printable { Some(String::from_utf8_lossy(&hit.key).to_string()) } else { None },
                "offset": format!("0x{:x}", hit.offsets[0]),
                "offsets": hit.offsets.iter().map(|at| format!("0x{at:x}")).collect::<Vec<String>>(),
                "offsetCount": hit.offsets.len(),
                "confidence": hit.confidence,
                "verified": hit.verified,
                "partial": hit.partial,
            });
            if hit.dropped {
                item["offsetsTruncated"] = json!(true);
            }
            if hit.partial {
                item["note"] = json!(format!(
                    "probe 只有 {} 字节，密钥更长，只能恢复前缀",
                    hit.key.len()
                ));
            } else if !hit.verified {
                item["note"] = json!("没有多余字节可验证，只按重复出现筛出候选");
            }
            item
        })
        .collect();

    let note = if rendered.is_empty() {
        "没有命中：probe 在这些密钥长度下都不是明文".to_string()
    } else if hits.iter().any(|hit| !hit.verified) {
        "多字节密钥可能不止一把；优先取命中位置最集中的那把。confidence=low 的是未验证候选（probe 不够长或落在文件尾），要人工确认，不能直接下结论".to_string()
    } else {
        "多字节密钥可能不止一把；优先取命中位置最集中的那把".to_string()
    };

    respond(
        ctx,
        "crypto",
        json!({
            "path": path.display().to_string(),
            "size": size,
            "scanned": bytes.len(),
            "probe": probe,
            "minLen": min_len,
            "maxLen": max_len,
            "matchCount": rendered.len(),
            "matches": rendered,
            "truncated": size > bytes.len() as u64 || capped,
            "matchesCapped": capped,
            "note": note,
        }),
    )
}

#[allow(clippy::too_many_arguments)]
fn record_hit(
    hits: &mut Vec<XorHit>,
    index: &mut HashMap<Vec<u8>, usize>,
    key: Vec<u8>,
    at: usize,
    confidence: &'static str,
    verified: bool,
    partial: bool,
    capped: &mut bool,
) {
    match index.get(&key) {
        Some(&slot) => {
            let hit = &mut hits[slot];
            if hit.offsets.len() < XOR_OFFSET_MAX {
                hit.offsets.push(at);
            } else {
                hit.dropped = true;
            }
            if confidence_rank(confidence) > confidence_rank(hit.confidence) {
                hit.confidence = confidence;
            }
            hit.verified |= verified;
        }
        None => {
            if hits.len() >= XOR_MATCH_MAX {
                *capped = true;
                return;
            }
            index.insert(key.clone(), hits.len());
            hits.push(XorHit {
                key,
                offsets: vec![at],
                confidence,
                verified,
                partial,
                dropped: false,
            });
        }
    }
}

// ---------------------------------------------------------------- 差异

fn load_source(ctx: &ToolContext<'_>, raw: &str) -> Result<(Vec<u8>, String, bool), String> {
    if let Some(id) = raw.strip_prefix("artifact:") {
        let scope = ctx
            .dag
            .ok_or_else(|| "读制品需要 DAG 作用域；在聊天里请改用文件路径".to_string())?;
        let store = ArtifactStore::global()?;
        let (reference, bytes) = store.read(&scope.dag_id, id.trim())?;
        let label = format!("artifact:{} (key={})", reference.id, reference.key);
        return Ok((bytes, label, false));
    }
    let path = resolve_path(Path::new(ctx.directory), raw)?;
    let meta = std::fs::metadata(&path).map_err(|error| format!("读不到 {raw}：{error}"))?;
    if !meta.is_file() {
        return Err(format!("{raw} 不是普通文件"));
    }
    let truncated = meta.len() > SCAN_LIMIT;
    let bytes = read_head(&path, SCAN_LIMIT)?;
    Ok((bytes, path.display().to_string(), truncated))
}

/// 分块比较，把相邻的差异块并成区间。
fn changed_ranges(left: &[u8], right: &[u8], cap: usize) -> (Vec<(u64, u64)>, bool) {
    let mut ranges: Vec<(u64, u64)> = Vec::new();
    let longest = left.len().max(right.len());
    let mut offset = 0usize;
    while offset < longest {
        let end = (offset + DIFF_CHUNK).min(longest);
        let a = left.get(offset..end.min(left.len()));
        let b = right.get(offset..end.min(right.len()));
        let same = match (a, b) {
            (Some(a), Some(b)) => a == b,
            (None, None) => true,
            _ => false,
        };
        if !same {
            match ranges.last_mut() {
                Some(last) if last.0 + last.1 == offset as u64 => last.1 += (end - offset) as u64,
                _ => ranges.push((offset as u64, (end - offset) as u64)),
            }
        }
        offset = end;
    }
    let overflow = ranges.len() > cap;
    ranges.truncate(cap);
    (ranges, overflow)
}

fn run_diff(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let left_raw = args.get("a").and_then(Value::as_str).unwrap_or("").trim();
    let right_raw = args.get("b").and_then(Value::as_str).unwrap_or("").trim();
    if left_raw.is_empty() || right_raw.is_empty() {
        return ToolResult::fail("需要 a 与 b 两个来源");
    }
    let (left, left_label, left_cut) = match load_source(ctx, left_raw) {
        Ok(value) => value,
        Err(error) => return ToolResult::fail(format!("a：{error}")),
    };
    let (right, right_label, right_cut) = match load_source(ctx, right_raw) {
        Ok(value) => value,
        Err(error) => return ToolResult::fail(format!("b：{error}")),
    };

    let meta_a = identify(&left, left.len() as u64);
    let meta_b = identify(&right, right.len() as u64);
    let fields = ["format", "arch", "bits", "entry", "sections", "compileTime", "peKind"];
    let mut format_delta = serde_json::Map::new();
    for field in fields {
        let a = meta_a.get(field).cloned().unwrap_or(Value::Null);
        let b = meta_b.get(field).cloned().unwrap_or(Value::Null);
        if a != b {
            format_delta.insert(field.to_string(), json!({ "a": a, "b": b }));
        }
    }

    let prefix = left
        .iter()
        .zip(right.iter())
        .take_while(|(a, b)| a == b)
        .count();
    // 共同后缀不能和共同前缀重叠：两边全是同一份文件时，否则会被数两遍。
    let shorter = left.len().min(right.len());
    let suffix = left
        .iter()
        .rev()
        .zip(right.iter().rev())
        .take_while(|(a, b)| a == b)
        .count()
        .min(shorter - prefix.min(shorter));
    let (ranges, overflow) = changed_ranges(&left, &right, 200);

    respond(
        ctx,
        "diff",
        json!({
            "a": {
                "source": left_label,
                "size": left.len(),
                "sha256": sha256_of(&left),
                "format": meta_a.get("format").cloned().unwrap_or(Value::Null),
                "truncated": left_cut,
            },
            "b": {
                "source": right_label,
                "size": right.len(),
                "sha256": sha256_of(&right),
                "format": meta_b.get("format").cloned().unwrap_or(Value::Null),
                "truncated": right_cut,
            },
            "identical": left == right,
            "sizeDelta": right.len() as i64 - left.len() as i64,
            "commonPrefix": prefix,
            "commonSuffix": suffix,
            "formatDelta": format_delta,
            "changedRanges": ranges
                .iter()
                .map(|(offset, length)| json!({ "offset": format!("0x{offset:x}"), "length": length }))
                .collect::<Vec<Value>>(),
            "changedRangeCount": ranges.len(),
            "rangesTruncated": overflow,
            "chunkSize": DIFF_CHUNK,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn temp_file(tag: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prism-bin-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.bin");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(bytes).unwrap();
        path
    }

    fn context<'a>(directory: &'a str) -> ToolContext<'a> {
        static ENABLED: std::sync::OnceLock<super::super::tools::EnabledTools> =
            std::sync::OnceLock::new();
        ToolContext {
            app: None,
            session_id: "test",
            directory,
            call_id: "call",
            provider: None,
            model: None,
            permission: None,
            enabled: ENABLED.get_or_init(super::super::tools::EnabledTools::all),
            dag: None,
            should_stop: &|| false,
        }
    }

    /// 最小可解析的 PE32：一个 `.rdata` 节，导入 KERNEL32 的两个函数。
    /// 段表、RVA 换算、thunk 遍历三条路径都靠它覆盖。
    fn sample_pe() -> Vec<u8> {
        let mut bytes = vec![0u8; 0x700];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
        bytes[0x84..0x86].copy_from_slice(&0x014cu16.to_le_bytes());
        bytes[0x86..0x88].copy_from_slice(&1u16.to_le_bytes());
        bytes[0x94..0x96].copy_from_slice(&0xe0u16.to_le_bytes());
        bytes[0x98..0x9a].copy_from_slice(&0x10bu16.to_le_bytes());
        bytes[0xf8 + 8..0xf8 + 12].copy_from_slice(&0x1000u32.to_le_bytes());

        let table = 0x178;
        bytes[table..table + 6].copy_from_slice(b".rdata");
        bytes[table + 8..table + 12].copy_from_slice(&0x2000u32.to_le_bytes());
        bytes[table + 12..table + 16].copy_from_slice(&0x1000u32.to_le_bytes());
        bytes[table + 16..table + 20].copy_from_slice(&0x2000u32.to_le_bytes());
        bytes[table + 20..table + 24].copy_from_slice(&0x200u32.to_le_bytes());

        bytes[0x200 + 12..0x200 + 16].copy_from_slice(&0x1100u32.to_le_bytes());
        bytes[0x200 + 16..0x200 + 20].copy_from_slice(&0x1200u32.to_le_bytes());
        bytes[0x300..0x30c].copy_from_slice(b"KERNEL32.dll");
        bytes[0x400..0x404].copy_from_slice(&0x1300u32.to_le_bytes());
        bytes[0x404..0x408].copy_from_slice(&0x1320u32.to_le_bytes());
        bytes[0x502..0x502 + 12].copy_from_slice(b"VirtualAlloc");
        bytes[0x522..0x522 + 5].copy_from_slice(b"Sleep");
        bytes
    }

    #[test]
    fn query_parses_paths_indexes_and_expansion() {
        // .agents / [0] / .tools / [] 四步
        let steps = parse_query(".agents[0].tools[]").unwrap();
        assert_eq!(steps.len(), 4);
        let nested = parse_query("items[-1]").unwrap();
        assert!(matches!(nested[1], Step::Index(-1)));
        assert!(parse_query("").unwrap().is_empty());
        assert!(parse_query(".").unwrap().is_empty());
        assert!(parse_query(".a.").is_err(), "尾点应报错");
        assert!(parse_query(".a[0").is_err(), "未闭合方括号应报错");
    }

    #[test]
    fn query_walks_objects_and_arrays() {
        let root: Value = json!({
            "agents": [
                { "id": "static-analysis", "tools": ["bin.xref.search", "bin.crypto.scan"] },
                { "id": "unpacker", "tools": ["run_command"] }
            ]
        });
        let steps = parse_query(".agents[0].tools[1]").unwrap();
        assert_eq!(apply_query(&root, &steps).unwrap(), vec![json!("bin.crypto.scan")]);

        let last = parse_query(".agents[-1].id").unwrap();
        assert_eq!(apply_query(&root, &last).unwrap(), vec![json!("unpacker")]);

        let flat = parse_query(".agents[].id").unwrap();
        assert_eq!(
            apply_query(&root, &flat).unwrap(),
            vec![json!("static-analysis"), json!("unpacker")]
        );
    }

    #[test]
    fn query_reports_where_it_broke() {
        let root: Value = json!({ "a": { "b": 1 } });
        let error = apply_query(&root, &parse_query(".a.missing").unwrap()).unwrap_err();
        assert!(error.contains(".a.missing"), "错误要带完整路径：{error}");
    }

    #[test]
    fn json_tool_reads_inline_documents() {
        let ctx = context("/tmp");
        let out = run_json(
            &ctx,
            &json!({ "json": r#"{"hits":[{"off":16},{"off":32}]}"#, "query": ".hits[1].off" }),
        );
        assert!(out.outcome.error.is_none(), "{}", out.outcome.text);
        assert!(out.outcome.text.contains("\"value\": 32"), "{}", out.outcome.text);
    }

    #[test]
    fn json_tool_rejects_broken_input() {
        let ctx = context("/tmp");
        let out = run_json(&ctx, &json!({ "json": "{not json", "query": "." }));
        assert!(out.outcome.error.is_some());
        assert!(out.outcome.text.contains("不是合法 JSON"));
    }

    #[test]
    fn needle_encodes_three_ways() {
        assert_eq!(encode_needle("MZ", "ascii").unwrap(), b"MZ");
        assert_eq!(
            encode_needle("AB", "utf16").unwrap(),
            vec![0x41, 0x00, 0x42, 0x00]
        );
        assert_eq!(encode_needle("4d 5a", "hex").unwrap(), vec![0x4d, 0x5a]);
        assert!(encode_needle("4d5", "hex").is_err());
        assert!(encode_needle("zz", "hex").is_err());
    }

    #[test]
    fn find_all_reports_offsets_and_truncation() {
        let hay = b"..MZ..MZ..MZ";
        let (hits, cut) = find_all(hay, b"MZ", 10);
        assert_eq!(hits, vec![2, 6, 10]);
        assert!(!cut);
        let (hits, cut) = find_all(hay, b"MZ", 2);
        assert_eq!(hits.len(), 2);
        assert!(cut, "达到上限要标记截断");
        assert!(find_all(hay, b"", 5).0.is_empty());
    }

    #[test]
    fn pe_regions_map_offsets_to_virtual_addresses() {
        let bytes = sample_pe();
        let regions = regions(&bytes);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].name, ".rdata");
        let (section, address) = locate(&regions, 0x300);
        assert_eq!(section, Some(".rdata"));
        assert_eq!(address, Some(0x1100));
        assert_eq!(locate(&regions, 0x10).0, None, "节外不该编一个段名");
    }

    #[test]
    fn xref_finds_string_and_reports_section() {
        let bytes = sample_pe();
        let path = temp_file("xref", &bytes);
        let ctx = context(path.parent().unwrap().to_str().unwrap());
        let out = run_xref(&ctx, &json!({ "path": "sample.bin", "needle": "KERNEL32.dll" }));
        assert!(out.outcome.error.is_none(), "{}", out.outcome.text);
        let payload: Value = serde_json::from_str(&out.outcome.text).unwrap();
        assert_eq!(payload["hits"], json!(1));
        assert_eq!(payload["matches"][0]["offset"], "0x300");
        assert_eq!(payload["matches"][0]["va"], "0x1100");
        assert_eq!(payload["matches"][0]["section"], ".rdata");
    }

    #[test]
    fn import_table_is_walked_with_sensitive_flags() {
        let bytes = sample_pe();
        let path = temp_file("imports", &bytes);
        let ctx = context(path.parent().unwrap().to_str().unwrap());
        let out = run_import_export(&ctx, &json!({ "path": "sample.bin" }));
        assert!(out.outcome.error.is_none(), "{}", out.outcome.text);
        let payload: Value = serde_json::from_str(&out.outcome.text).unwrap();
        let imports = payload["imports"].as_array().unwrap();
        assert_eq!(imports.len(), 1, "{}", out.outcome.text);
        assert_eq!(imports[0]["library"], "KERNEL32.dll");
        assert_eq!(imports[0]["functions"], json!(2));
        assert_eq!(imports[0]["detail"][0]["name"], "VirtualAlloc");
        assert_eq!(imports[0]["detail"][0]["sensitive"], "内存分配");
        assert!(imports[0]["detail"][1]["sensitive"].is_null(), "Sleep 不该被标高危");
        assert_eq!(payload["riskHints"], json!(["内存分配"]));
    }

    #[test]
    fn sensitive_table_matches_case_insensitively() {
        assert_eq!(sensitive_of("VirtualAllocEx"), Some("内存分配"));
        assert_eq!(sensitive_of("createprocessw"), Some("执行"));
        assert_eq!(sensitive_of("memcpy"), None);
    }

    #[test]
    fn crypto_scan_reports_known_constant_with_offset() {
        let mut bytes = vec![0u8; 0x200];
        bytes[0x80..0x90].copy_from_slice(&[
            0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7,
            0xab, 0x76,
        ]);
        // 0..255 递增表：RC4 KSA 的典型特征。
        for index in 0..256usize {
            bytes[0x100 + index] = index as u8;
        }
        let path = temp_file("crypto", &bytes);
        let ctx = context(path.parent().unwrap().to_str().unwrap());
        let out = run_crypto(&ctx, &json!({ "path": "sample.bin" }));
        assert!(out.outcome.error.is_none(), "{}", out.outcome.text);
        let payload: Value = serde_json::from_str(&out.outcome.text).unwrap();
        let hits = payload["matches"].as_array().unwrap();
        let aes = hits
            .iter()
            .find(|hit| hit["algorithm"] == "aes-sbox")
            .expect("应命中 AES S 盒");
        assert_eq!(aes["offset"], "0x80");
        assert_eq!(aes["confidence"], "high");
        assert!(hits.iter().any(|hit| hit["algorithm"] == "rc4-ksa"));
    }

    #[test]
    fn crypto_scan_brutes_single_byte_xor() {
        let plain = b"http://evil.example";
        let key = 0x5a;
        let encoded: Vec<u8> = plain.iter().map(|byte| byte ^ key).collect();
        let mut bytes = vec![0u8; 0x40];
        bytes[0x10..0x10 + encoded.len()].copy_from_slice(&encoded);
        let path = temp_file("xor", &bytes);
        let ctx = context(path.parent().unwrap().to_str().unwrap());
        let out = run_crypto(&ctx, &json!({ "path": "sample.bin", "xorProbe": "http://" }));
        assert!(out.outcome.error.is_none(), "{}", out.outcome.text);
        let payload: Value = serde_json::from_str(&out.outcome.text).unwrap();
        let matches = payload["xor"]["matches"].as_array().unwrap();
        assert!(
            matches.iter().any(|hit| hit["key"] == json!(key)),
            "应试出密钥 {key}：{}",
            out.outcome.text
        );
    }

    #[test]
    fn changed_ranges_merge_adjacent_blocks() {
        let left = vec![0u8; DIFF_CHUNK * 3];
        let mut right = left.clone();
        right[DIFF_CHUNK..DIFF_CHUNK + 8].fill(0xff);
        right[DIFF_CHUNK * 2..DIFF_CHUNK * 2 + 8].fill(0xff);
        let (ranges, cut) = changed_ranges(&left, &right, 10);
        assert_eq!(ranges, vec![(DIFF_CHUNK as u64, (DIFF_CHUNK * 2) as u64)]);
        assert!(!cut);
    }

    #[test]
    fn diff_reports_size_hash_and_ranges() {
        let left = vec![0u8; 0x200];
        let mut right = left.clone();
        right.extend_from_slice(b"appended");
        let left_path = temp_file("diff-a", &left);
        let right_path = left_path.with_file_name("right.bin");
        std::fs::write(&right_path, &right).unwrap();
        let ctx = context(left_path.parent().unwrap().to_str().unwrap());

        let out = run_diff(&ctx, &json!({ "a": "sample.bin", "b": "right.bin" }));
        assert!(out.outcome.error.is_none(), "{}", out.outcome.text);
        let payload: Value = serde_json::from_str(&out.outcome.text).unwrap();
        assert_eq!(payload["identical"], json!(false));
        assert_eq!(payload["sizeDelta"], json!(8));
        assert_eq!(payload["commonPrefix"], json!(0x200));
        assert_ne!(payload["a"]["sha256"], payload["b"]["sha256"]);
    }

    #[test]
    fn diff_of_identical_files_is_empty() {
        let bytes = vec![7u8; 1024];
        let left_path = temp_file("same-a", &bytes);
        let right_path = left_path.with_file_name("same-b.bin");
        std::fs::write(&right_path, &bytes).unwrap();
        let ctx = context(left_path.parent().unwrap().to_str().unwrap());
        let out = run_diff(&ctx, &json!({ "a": "sample.bin", "b": "same-b.bin" }));
        let payload: Value = serde_json::from_str(&out.outcome.text).unwrap();
        assert_eq!(payload["identical"], json!(true));
        assert_eq!(payload["changedRangeCount"], json!(0));
        // 共同前缀已经覆盖全文，后缀不再重复计数（否则「完全一样」会被读成
        // 「前缀 1024 + 后缀 1024」，比实际长度还大）。
        assert_eq!(payload["commonSuffix"], json!(0));
    }

    #[test]
    fn diff_rejects_missing_side() {
        let ctx = context("/tmp");
        let out = run_diff(&ctx, &json!({ "a": "x" }));
        assert!(out.outcome.error.is_some());
        assert!(out.outcome.text.contains("需要 a 与 b"));
    }

    // ------------------------------------------------ 证书 / 密钥（asn1.parse）

    fn der(tag: u8, value: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        let len = value.len();
        if len < 0x80 {
            out.push(len as u8);
        } else if len < 0x100 {
            out.push(0x81);
            out.push(len as u8);
        } else {
            out.push(0x82);
            out.push((len >> 8) as u8);
            out.push(len as u8);
        }
        out.extend_from_slice(value);
        out
    }

    fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    fn oid_der(dotted: &str) -> Vec<u8> {
        let parts: Vec<u64> = dotted.split('.').map(|part| part.parse().unwrap()).collect();
        let mut body = vec![(parts[0] * 40 + parts[1]) as u8];
        for part in &parts[2..] {
            let mut chunk = vec![(*part & 0x7f) as u8];
            let mut rest = *part >> 7;
            while rest > 0 {
                chunk.insert(0, (rest & 0x7f) as u8 | 0x80);
                rest >>= 7;
            }
            body.extend_from_slice(&chunk);
        }
        der(0x06, &body)
    }

    fn alg_id(oid: &str) -> Vec<u8> {
        der(0x30, &cat(&[oid_der(oid), der(0x05, &[])]))
    }

    /// 2048 位模数：DER INTEGER 带一个 0x00 符号位，幅值首字节 0xc1。
    fn rsa_modulus() -> Vec<u8> {
        let mut body = vec![0x00, 0xc1];
        body.extend(std::iter::repeat(0x5a).take(255));
        der(0x02, &body)
    }

    fn rsa_exponent() -> Vec<u8> {
        der(0x02, &[0x01, 0x00, 0x01])
    }

    fn dn(cn: &str) -> Vec<u8> {
        der(
            0x30,
            &der(
                0x31,
                &der(0x30, &cat(&[oid_der("2.5.4.3"), der(0x13, cn.as_bytes())])),
            ),
        )
    }

    fn rsa_spki() -> Vec<u8> {
        let key = der(0x30, &cat(&[rsa_modulus(), rsa_exponent()]));
        let mut bits = vec![0x00];
        bits.extend_from_slice(&key);
        der(
            0x30,
            &cat(&[alg_id("1.2.840.113549.1.1.1"), der(0x03, &bits)]),
        )
    }

    fn sample_cert() -> Vec<u8> {
        let tbs = der(
            0x30,
            &cat(&[
                der(0xa0, &der(0x02, &[0x02])),
                der(0x02, &[0x12, 0x34]),
                alg_id("1.2.840.113549.1.1.11"),
                dn("Test CA"),
                der(
                    0x30,
                    &cat(&[der(0x17, b"230101000000Z"), der(0x17, b"330101000000Z")]),
                ),
                dn("Test Leaf"),
                rsa_spki(),
            ]),
        );
        let mut signature = vec![0x00];
        signature.extend_from_slice(&[0x30, 0x03, 0x02, 0x01, 0x01]);
        der(
            0x30,
            &cat(&[
                tbs,
                alg_id("1.2.840.113549.1.1.11"),
                der(0x03, &signature),
            ]),
        )
    }

    #[test]
    fn der_tlv_handles_short_long_and_high_tags() {
        let short = der(0x04, b"abc");
        let (tlv, next) = read_tlv(&short, 0).unwrap();
        assert_eq!(tlv.value, b"abc");
        assert_eq!(next, short.len());

        let long = der(0x04, &vec![7u8; 300]);
        let (tlv, next) = read_tlv(&long, 0).unwrap();
        assert_eq!(tlv.value.len(), 300);
        assert_eq!(next, long.len());

        // 高 tag 号：0x1f 0x1f 表示 tag=31。
        let high = vec![0x1f, 0x1f, 0x01, 0xaa];
        let (tlv, next) = read_tlv(&high, 0).unwrap();
        assert_eq!(tlv.tag, 31);
        assert_eq!(tlv.value, &[0xaa]);
        assert_eq!(next, 4);
    }

    #[test]
    fn der_tlv_rejects_malformed_input_without_panicking() {
        assert!(read_tlv(&[0x30], 0).is_err(), "缺长度字段");
        assert!(read_tlv(&[0x30, 0x05, 0x00], 0).is_err(), "值被截断");
        assert!(read_tlv(&[0x30, 0x80, 0x00], 0).is_err(), "不定长不是 DER");
        assert!(read_tlv(&[0x30, 0x85, 0, 0, 0, 0, 1], 0).is_err(), "长度字段过长");
        assert!(describe_der(&[0x02, 0x01, 0x01]).is_err(), "顶层不是 SEQUENCE");
        assert!(describe_der(&[0x30, 0x82, 0xff, 0xff]).is_err(), "长度超界");
    }

    #[test]
    fn integer_bit_length_uses_the_leading_bit() {
        assert_eq!(bit_len(&[0x00, 0xc1, 0x00]), 16);
        assert_eq!(bit_len(&[0x00, 0x80]), 8);
        assert_eq!(bit_len(&[0x7f]), 7);
        assert_eq!(bit_len(&[0x00, 0x01]), 1);
        assert_eq!(bit_len(&[0x00]), 0);
    }

    #[test]
    fn der_time_formats_seconds_and_generalized_time() {
        let utc = der(0x17, b"230102030405Z");
        let (tlv, _) = read_tlv(&utc, 0).unwrap();
        assert_eq!(time_text(&tlv), "2023-01-02T03:04:05Z");

        let generalized = der(0x18, b"20500102030405Z");
        let (tlv, _) = read_tlv(&generalized, 0).unwrap();
        assert_eq!(time_text(&tlv), "2050-01-02T03:04:05Z");

        // 不带秒的写法：秒补 00，不把分钟读成秒。
        let short = der(0x17, b"2301020304Z");
        let (tlv, _) = read_tlv(&short, 0).unwrap();
        assert_eq!(time_text(&tlv), "2023-01-02T03:04:00Z");
    }

    #[test]
    fn oid_decodes_to_dotted_text() {
        assert_eq!(
            oid_text(&[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b]).unwrap(),
            "1.2.840.113549.1.1.11"
        );
        assert_eq!(oid_text(&[0x55, 0x1d, 0x0e]).unwrap(), "2.5.29.14");
        assert!(oid_text(&[]).is_err());
        assert!(oid_text(&[0x2a, 0x86]).is_err(), "变长段没写完要报错");
    }

    #[test]
    fn asn1_describes_x509_certificate() {
        let value = describe_der(&sample_cert()).unwrap();
        assert_eq!(value["type"], "x509");
        assert_eq!(value["version"], "v3");
        assert_eq!(value["serial"], "0x1234");
        assert_eq!(value["issuer"], "CN=Test CA");
        assert_eq!(value["subject"], "CN=Test Leaf");
        assert_eq!(value["validNotBefore"], "2023-01-01T00:00:00Z");
        assert_eq!(value["validNotAfter"], "2033-01-01T00:00:00Z");
        assert_eq!(value["signatureAlgorithm"], "sha256WithRSAEncryption");
        assert_eq!(value["publicKey"]["algorithm"], "rsa");
        assert_eq!(value["publicKey"]["bits"], 2048);
        assert_eq!(value["publicKey"]["e"], 65537);
        let fingerprint = value["fingerprintSha256"].as_str().unwrap();
        assert_eq!(fingerprint.len(), 95, "32 字节十六进制 + 31 个冒号");
        assert_eq!(fingerprint.matches(':').count(), 31);
    }

    #[test]
    fn asn1_reads_pem_and_matches_bare_der() {
        let cert = sample_cert();
        let text = format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            base64::engine::general_purpose::STANDARD.encode(&cert)
        );
        let path = temp_file("pem-cert", text.as_bytes());
        let ctx = context(path.parent().unwrap().to_str().unwrap());
        let out = run_asn1(&ctx, &json!({ "path": "sample.bin" }));
        assert!(out.outcome.error.is_none(), "{}", out.outcome.text);
        let payload: Value = serde_json::from_str(&out.outcome.text).unwrap();
        assert_eq!(payload["container"], "pem");
        assert_eq!(payload["pemLabel"], "CERTIFICATE");
        assert_eq!(payload["type"], "x509");
        assert_eq!(payload["publicKey"]["bits"], 2048);
        let bare = describe_der(&cert).unwrap();
        assert_eq!(payload["fingerprintSha256"], bare["fingerprintSha256"]);
    }

    #[test]
    fn asn1_parses_certificate_at_embedded_offset() {
        let mut bytes = vec![0xa5u8; 0x40];
        bytes.extend_from_slice(&sample_cert());
        let path = temp_file("embedded-cert", &bytes);
        let ctx = context(path.parent().unwrap().to_str().unwrap());
        let out = run_asn1(&ctx, &json!({ "path": "sample.bin", "offset": "0x40" }));
        assert!(out.outcome.error.is_none(), "{}", out.outcome.text);
        let payload: Value = serde_json::from_str(&out.outcome.text).unwrap();
        assert_eq!(payload["offset"], "0x40");
        assert_eq!(payload["type"], "x509");
    }

    #[test]
    fn asn1_recognises_pkcs7_signed_data() {
        let cert = sample_cert();
        let signed = der(
            0x30,
            &cat(&[
                der(0x02, &[0x01]),
                der(0x31, &[]),
                der(0x30, &oid_der("1.2.840.113549.1.7.1")),
                der(0xa0, &cert),
            ]),
        );
        let content_info = der(
            0x30,
            &cat(&[
                oid_der("1.2.840.113549.1.7.2"),
                der(0xa0, &signed),
            ]),
        );
        let value = describe_der(&content_info).unwrap();
        assert_eq!(value["type"], "pkcs7");
        assert_eq!(value["certificateCount"], 1);
        assert_eq!(value["certificates"][0]["type"], "x509");
        assert_eq!(value["certificates"][0]["subject"], "CN=Test Leaf");
    }

    #[test]
    fn asn1_reads_pkcs1_and_pkcs8_keys() {
        let public = der(0x30, &cat(&[rsa_modulus(), rsa_exponent()]));
        let value = describe_der(&public).unwrap();
        assert_eq!(value["type"], "rsa-public");
        assert_eq!(value["bits"], 2048);
        assert_eq!(value["e"], 65537);

        let private = der(
            0x30,
            &cat(&[
                der(0x02, &[0x00]),
                rsa_modulus(),
                rsa_exponent(),
                der(0x02, &[0x11]),
                der(0x02, &[0x03]),
                der(0x02, &[0x05]),
                der(0x02, &[0x07]),
                der(0x02, &[0x09]),
                der(0x02, &[0x0b]),
            ]),
        );
        let pkcs8 = der(
            0x30,
            &cat(&[
                der(0x02, &[0x00]),
                alg_id("1.2.840.113549.1.1.1"),
                der(0x04, &private),
            ]),
        );
        let value = describe_der(&pkcs8).unwrap();
        assert_eq!(value["type"], "rsa-private");
        assert_eq!(value["bits"], 2048);
        let text = value.to_string();
        for secret in ["\"d\"", "\"p\"", "\"q\"", "\"dp\"", "\"dq\"", "\"qinv\""] {
            assert!(!text.contains(secret), "私钥字段 {secret} 不该导出：{text}");
        }
        assert!(value["note"].as_str().unwrap().contains("不导出"));
    }

    #[test]
    fn asn1_reads_ec_spki_with_curve_name() {
        let mut point = vec![0x04];
        point.extend(std::iter::repeat(0x11).take(64));
        let spki = der(
            0x30,
            &cat(&[
                der(
                    0x30,
                    &cat(&[
                        oid_der("1.2.840.10045.2.1"),
                        oid_der("1.2.840.10045.3.1.7"),
                    ]),
                ),
                der(0x03, &cat(&[vec![0x00], point])),
            ]),
        );
        let value = describe_der(&spki).unwrap();
        assert_eq!(value["type"], "public-key");
        assert_eq!(value["publicKey"]["algorithm"], "ec");
        assert_eq!(value["publicKey"]["curve"], "prime256v1 (P-256)");
        assert_eq!(value["publicKey"]["bits"], 256);
    }

    #[test]
    fn asn1_keeps_unknown_oids_verbatim() {
        let alg = alg_id("1.2.3.4.5");
        let (tlv, _) = read_tlv(&alg, 0).unwrap();
        assert_eq!(algorithm_name(&tlv), "1.2.3.4.5");
        assert_eq!(
            algorithm_name(&read_tlv(&alg_id("1.2.840.113549.1.1.11"), 0).unwrap().0),
            "sha256WithRSAEncryption"
        );
    }

    // ------------------------------------------------ 多字节 XOR（xor_brute）

    fn xor_encode(plain: &[u8], key: &[u8], at: usize, bytes: &mut [u8]) {
        for (index, byte) in plain.iter().enumerate() {
            bytes[at + index] = byte ^ key[index % key.len()];
        }
    }

    #[test]
    fn xor_brute_recovers_single_byte_key() {
        let plain = b"http://evil.example";
        let mut bytes = vec![0u8; 0x80];
        xor_encode(plain, &[0x5a], 0x10, &mut bytes);
        let path = temp_file("xor-single", &bytes);
        let ctx = context(path.parent().unwrap().to_str().unwrap());
        let out = run_xor_brute(&ctx, &json!({ "path": "sample.bin", "probe": "http://" }));
        assert!(out.outcome.error.is_none(), "{}", out.outcome.text);
        let payload: Value = serde_json::from_str(&out.outcome.text).unwrap();
        let hit = payload["matches"]
            .as_array()
            .unwrap()
            .iter()
            .find(|hit| hit["key"] == json!([0x5a]))
            .unwrap_or_else(|| panic!("应命中单字节密钥：{}", out.outcome.text));
        assert_eq!(hit["confidence"], "certain");
        assert_eq!(hit["offset"], "0x10");
        assert_eq!(hit["keyStr"], "Z");
    }

    #[test]
    fn xor_brute_recovers_multi_byte_key_over_long_plaintext() {
        let key = b"abc";
        let plain = b"http://example.com/api/v1";
        let mut bytes = vec![0u8; 0x80];
        xor_encode(plain, key, 0x20, &mut bytes);
        let path = temp_file("xor-multi", &bytes);
        let ctx = context(path.parent().unwrap().to_str().unwrap());
        let out = run_xor_brute(
            &ctx,
            &json!({ "path": "sample.bin", "probe": "http://example.com/api/v1" }),
        );
        assert!(out.outcome.error.is_none(), "{}", out.outcome.text);
        let payload: Value = serde_json::from_str(&out.outcome.text).unwrap();
        let hit = payload["matches"]
            .as_array()
            .unwrap()
            .iter()
            .find(|hit| hit["key"] == json!([0x61, 0x62, 0x63]))
            .unwrap_or_else(|| panic!("应试出多字节密钥 abc：{}", out.outcome.text));
        assert_eq!(hit["confidence"], "certain");
        assert_eq!(hit["offset"], "0x20");
        assert_eq!(hit["partial"], false);
    }

    #[test]
    fn xor_brute_merges_offsets_for_the_same_key() {
        let key = b"xy";
        let plain = b"http://dup.example";
        let mut bytes = vec![0u8; 0x80];
        xor_encode(plain, key, 0x10, &mut bytes);
        xor_encode(plain, key, 0x40, &mut bytes);
        let path = temp_file("xor-dup", &bytes);
        let ctx = context(path.parent().unwrap().to_str().unwrap());
        let out = run_xor_brute(&ctx, &json!({ "path": "sample.bin", "probe": "http://dup.example" }));
        assert!(out.outcome.error.is_none(), "{}", out.outcome.text);
        let payload: Value = serde_json::from_str(&out.outcome.text).unwrap();
        let hits: Vec<&Value> = payload["matches"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|hit| hit["key"] == json!([0x78, 0x79]))
            .collect();
        assert_eq!(hits.len(), 1, "同一把密钥应合并成一条：{}", out.outcome.text);
        assert_eq!(hits[0]["offsetCount"], 2);
        assert_eq!(hits[0]["offsets"], json!(["0x10", "0x40"]));
    }

    #[test]
    fn xor_brute_marks_partial_prefix_as_low_confidence() {
        // probe 只有 4 字节、密钥 8 字节：只能恢复前缀，且没有可验证的冗余。
        // 只保留在同一段数据里重复出现的候选，避免随机数据刷屏。
        let window = [0x3a, 0x91, 0x0c, 0xfe];
        let mut bytes = vec![0u8; 0x60];
        bytes[0x10..0x14].copy_from_slice(&window);
        bytes[0x30..0x34].copy_from_slice(&window);
        let path = temp_file("xor-partial", &bytes);
        let ctx = context(path.parent().unwrap().to_str().unwrap());
        let out = run_xor_brute(
            &ctx,
            &json!({ "path": "sample.bin", "probe": "http", "minLen": 8, "maxLen": 8 }),
        );
        assert!(out.outcome.error.is_none(), "{}", out.outcome.text);
        let payload: Value = serde_json::from_str(&out.outcome.text).unwrap();
        let expected: Vec<u8> = window
            .iter()
            .zip(b"http")
            .map(|(cipher, plain)| cipher ^ plain)
            .collect();
        let hit = payload["matches"]
            .as_array()
            .unwrap()
            .iter()
            .find(|hit| hit["key"] == json!(expected))
            .unwrap_or_else(|| panic!("应试出密钥前缀：{}", out.outcome.text));
        assert_eq!(hit["partial"], true);
        assert_eq!(hit["verified"], false);
        assert_eq!(hit["confidence"], "low");
        assert_eq!(hit["offsetCount"], 2);
    }

    #[test]
    fn xor_brute_reports_no_match() {
        let bytes: Vec<u8> = (0..512).map(|index| (index * 37 % 251) as u8).collect();
        let path = temp_file("xor-none", &bytes);
        let ctx = context(path.parent().unwrap().to_str().unwrap());
        let out = run_xor_brute(
            &ctx,
            &json!({ "path": "sample.bin", "probe": "http://", "maxLen": 4 }),
        );
        assert!(out.outcome.error.is_none(), "{}", out.outcome.text);
        let payload: Value = serde_json::from_str(&out.outcome.text).unwrap();
        assert_eq!(payload["matchCount"], 0, "{}", out.outcome.text);
        assert!(payload["note"].as_str().unwrap().contains("没有命中"));
    }

    #[test]
    fn every_tool_declares_static_read_metadata() {
        for tool in tools() {
            assert_eq!(tool.meta.risk, Risk::Read, "{} 不该写盘", tool.name);
            assert_eq!(tool.meta.scope, ToolScope::Any, "{} 应可在聊天里用", tool.name);
            assert_eq!(tool.meta.tags, vec!["bin:static"], "{} 标签不对", tool.name);
            assert_eq!(tool.permission, Permission::ReadOnly);
        }
    }
}
