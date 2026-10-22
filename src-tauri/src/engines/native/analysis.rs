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

use std::path::Path;
use std::sync::Arc;

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

/// 按首字节过滤的全量搜索。比逐窗口 `position` 快，也不引新依赖。
fn find_all(haystack: &[u8], needle: &[u8], limit: usize) -> (Vec<usize>, bool) {
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
const SIGNATURES: &[(&str, &[u8], &str, &str)] = &[
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
