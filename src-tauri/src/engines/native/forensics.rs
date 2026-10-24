//! 安全研究线的本机工具：`bin.meta.identify` / `bin.hash.compute` /
//! `bin.strings.extract` / `bin.pack.detect`。
//!
//! 四个工具都只读文件、都不联网、都不需要外部依赖——装好就能用。
//! 这是"工具矩阵"的第一批真实现：先把标签体系、制品落盘、参数上限跑通，
//! 再接 Ghidra / 沙箱这类要外部环境的工具。
//!
//! 一条纪律：解析不出来的字段**直接省略**，不猜。安全分析里一个编出来的
//! 架构名会让人做出错误的判断，比"未知"有害得多。

use std::io::Read;
use std::path::{Path, PathBuf};

use md5::Md5;
use serde_json::{json, Value};
use sha1::Sha1;
use sha2::{Digest, Sha256};

use super::tools::{
    builtin_full, resolve_path, Network, Permission, Risk, ToolContext, ToolDef, ToolOutcome,
    ToolMeta, ToolResult, ToolScope,
};
use crate::dag::artifact::ArtifactStore;

/// 元信息只读文件头部：4 MiB 够装下 Mach-O 的全部 load commands。
const META_HEAD: u64 = 4 * 1024 * 1024;
/// 哈希上限：再大就不是"样本"而是数据集，硬算会卡住执行线程。
const HASH_LIMIT: u64 = 512 * 1024 * 1024;
/// 字符串扫描上限。
const STRINGS_SCAN: u64 = 32 * 1024 * 1024;
/// 字符串条目默认 / 上限。
const STRINGS_DEFAULT: usize = 200;
const STRINGS_MAX: usize = 2000;

pub fn tools() -> Vec<std::sync::Arc<ToolDef>> {
    vec![
        static_tool("bin.meta.identify", "样本格式识别", meta_schema, run_meta),
        static_tool("bin.hash.compute", "三哈希", hash_schema, run_hash),
        static_tool("bin.strings.extract", "字符串提取", strings_schema, run_strings),
        static_tool("bin.pack.detect", "加壳识别", pack_schema, run_pack),
    ]
}

/// 四个工具共用同一份元信息：静态、只读、不出网、不限作用域。
fn static_tool(
    name: &'static str,
    blurb: &'static str,
    schema: fn() -> Value,
    run: super::tools::ToolRun,
) -> std::sync::Arc<ToolDef> {
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

fn meta_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bin.meta.identify",
            "description": "识别样本格式与架构：PE / ELF / Mach-O / DEX / APK，给出位数、端序、入口、段数、编译时间等。只读文件头，不执行样本。",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "相对工作目录的样本路径" }
                },
                "required": ["path"]
            }
        }
    })
}

fn hash_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bin.hash.compute",
            "description": "算样本的 md5 / sha1 / sha256（流式，支持大文件）。摘要用于比对威胁情报与去重。",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "相对工作目录的样本路径" },
                    "algorithms": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["md5", "sha1", "sha256"] },
                        "description": "要哪些，默认三个都算"
                    }
                },
                "required": ["path"]
            }
        }
    })
}

fn strings_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bin.strings.extract",
            "description": "提取 ASCII 与 UTF-16LE 字符串，自动标记 URL / 域名 / IP / 路径 / 命令 / 密钥。按内容去重，带偏移。",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "相对工作目录的样本路径" },
                    "minLen": { "type": "integer", "description": "最短长度，默认 4" },
                    "limit": { "type": "integer", "description": "最多返回多少条，默认 200，上限 2000" },
                    "encoding": { "type": "string", "enum": ["both", "ascii", "utf16"], "description": "默认 both" }
                },
                "required": ["path"]
            }
        }
    })
}

// ---------------------------------------------------------------- 公共

pub(crate) fn target_of(ctx: &ToolContext<'_>, args: &Value) -> Result<(PathBuf, u64), ToolResult> {
    let raw = args
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ToolResult::fail("缺少 path"))?;
    let path = resolve_path(Path::new(ctx.directory), raw).map_err(ToolResult::fail)?;
    let meta = std::fs::metadata(&path)
        .map_err(|error| ToolResult::fail(format!("读不到 {raw}：{error}")))?;
    if !meta.is_file() {
        return Err(ToolResult::fail(format!("{raw} 不是普通文件")));
    }
    Ok((path, meta.len()))
}

pub(crate) fn read_head(path: &Path, cap: u64) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|error| format!("打开文件失败：{error}"))?;
    let mut buffer = Vec::new();
    file.take(cap)
        .read_to_end(&mut buffer)
        .map_err(|error| format!("读取文件失败：{error}"))?;
    Ok(buffer)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(TABLE[(byte >> 4) as usize] as char);
        out.push(TABLE[(byte & 0x0f) as usize] as char);
    }
    out
}

/// 有 DAG 作用域就顺手落制品，返回制品 id。
///
/// 失败不影响工具结果：结果本身已经在返回值里，制品只是"更好的传递方式"。
pub(crate) fn publish(
    ctx: &ToolContext<'_>,
    key: &str,
    text: &str,
) -> Option<Result<String, String>> {
    let scope = ctx.dag?;
    match ArtifactStore::global() {
        Ok(store) => Some(
            store
                .put_text(&scope.dag_id, &scope.node_id, key, text, None)
                .map(|reference| reference.id),
        ),
        Err(error) => Some(Err(error)),
    }
}

/// 统一出口：序列化 + （必要时）落制品。
pub(crate) fn respond(ctx: &ToolContext<'_>, key: &str, mut payload: Value) -> ToolResult {
    if let Some(result) = publish(ctx, key, &payload.to_string()) {
        match result {
            Ok(id) => payload["artifact"] = json!(id),
            Err(error) => payload["artifactError"] = json!(error),
        }
    }
    ToolResult::plain(ToolOutcome::ok(
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()),
    ))
}

// ---------------------------------------------------------------- 元信息

fn run_meta(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let (path, size) = match target_of(ctx, args) {
        Ok(found) => found,
        Err(failure) => return failure,
    };
    let head = match read_head(&path, META_HEAD) {
        Ok(bytes) => bytes,
        Err(error) => return ToolResult::fail(error),
    };
    let mut payload = identify(&head, size);
    payload["path"] = json!(path.display().to_string());
    payload["size"] = json!(size);
    respond(ctx, "meta", payload)
}

/// 纯函数：从头部字节判断格式。`size` 只用于展示与截断提示。
pub fn identify(head: &[u8], size: u64) -> Value {
    let mut out = json!({ "format": "unknown" });
    out["headerBytes"] = json!(head.len().min(size as usize));

    if head.len() >= 64 && &head[..4] == b"\x7fELF" {
        return elf(head, out);
    }
    if head.len() >= 4 && &head[..4] == b"dex\n" {
        out["format"] = json!("dex");
        out["arch"] = json!("dalvik");
        out["bits"] = json!(32);
        out["version"] = json!(String::from_utf8_lossy(&head[4..7]).to_string());
        if let Some(total) = read_u32(head, 32, true) {
            out["size"] = json!(total);
        }
        return out;
    }
    if head.len() >= 2 && &head[..2] == b"MZ" {
        if let Some(value) = pe(head, out.clone()) {
            return value;
        }
    }
    if head.len() >= 4 {
        let magic = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
        if matches!(magic, 0xfeed_face | 0xfeed_facf) {
            return macho(head, out, u32::from_le_bytes([head[0], head[1], head[2], head[3]]) == 0xfeed_facf, true);
        }
        if matches!(magic, 0xcefa_edfe | 0xcffa_edfe) {
            return macho(head, out, magic == 0xcffa_edfe, false);
        }
        // fat：大端读出来的 0xcafebabe。
        if u32::from_be_bytes([head[0], head[1], head[2], head[3]]) == 0xcafe_babe {
            let count = read_u32(head, 4, false).unwrap_or(0);
            out["format"] = json!("macho");
            out["fat"] = json!(true);
            out["archCount"] = json!(count);
            let mut arches: Vec<String> = Vec::new();
            for index in 0..count.min(8) as usize {
                let base = 8 + index * 20;
                if let Some(cpu) = read_u32(head, base, false) {
                    if let Some(name) = cpu_arch(cpu as i32) {
                        arches.push(name.to_string());
                    }
                }
            }
            if !arches.is_empty() {
                out["arches"] = json!(arches);
                out["arch"] = json!(arches.join(" + "));
            }
            return out;
        }
    }
    if head.len() >= 4 && &head[..4] == b"PK\x03\x04" {
        // APK 就是 zip：认得 AndroidManifest.xml 就当 APK，否则只是 zip。
        let window = &head[..head.len().min(256 * 1024)];
        let apk = window
            .windows(20)
            .any(|chunk| chunk == b"AndroidManifest.xml\x00" || chunk == b"AndroidManifest.xml");
        out["format"] = json!(if apk { "apk" } else { "zip" });
        if apk {
            out["arch"] = json!("dalvik");
        }
        return out;
    }
    out
}

pub(crate) fn read_u16(bytes: &[u8], at: usize, little: bool) -> Option<u16> {
    let slice = bytes.get(at..at + 2)?;
    let raw = [slice[0], slice[1]];
    Some(if little {
        u16::from_le_bytes(raw)
    } else {
        u16::from_be_bytes(raw)
    })
}

pub(crate) fn read_u32(bytes: &[u8], at: usize, little: bool) -> Option<u32> {
    let slice = bytes.get(at..at + 4)?;
    let raw = [slice[0], slice[1], slice[2], slice[3]];
    Some(if little {
        u32::from_le_bytes(raw)
    } else {
        u32::from_be_bytes(raw)
    })
}

pub(crate) fn read_u64(bytes: &[u8], at: usize, little: bool) -> Option<u64> {
    let slice = bytes.get(at..at + 8)?;
    let mut raw = [0u8; 8];
    raw.copy_from_slice(slice);
    Some(if little {
        u64::from_le_bytes(raw)
    } else {
        u64::from_be_bytes(raw)
    })
}

fn elf(head: &[u8], mut out: Value) -> Value {
    let class = head[4];
    let little = head[5] != 2;
    let bits = if class == 2 { 64 } else { 32 };
    out["format"] = json!("elf");
    out["bits"] = json!(bits);
    out["endian"] = json!(if little { "little" } else { "big" });

    if let Some(kind) = read_u16(head, 16, little) {
        out["elfType"] = json!(match kind {
            1 => "rel",
            2 => "exec",
            3 => "dyn",
            4 => "core",
            _ => "other",
        });
    }
    if let Some(machine) = read_u16(head, 18, little) {
        out["machine"] = json!(machine);
        let arch = match machine {
            3 => Some("x86"),
            62 => Some("x86_64"),
            40 => Some("arm"),
            183 => Some("aarch64"),
            243 => Some("riscv"),
            8 => Some("mips"),
            21 => Some("ppc64"),
            _ => None,
        };
        if let Some(arch) = arch {
            out["arch"] = json!(arch);
        }
    }
    let entry = if bits == 64 {
        read_u64(head, 24, little)
    } else {
        read_u32(head, 24, little).map(u64::from)
    };
    if let Some(entry) = entry {
        out["entry"] = json!(format!("0x{entry:x}"));
    }
    // 段 / 节数量在 32 位与 64 位头里偏移不同。
    let (shnum_at, phnum_at, shoff_at) = if bits == 64 {
        (60, 56, 40)
    } else {
        (48, 44, 32)
    };
    if let Some(count) = read_u16(head, shnum_at, little) {
        out["sections"] = json!(count);
    }
    if let Some(count) = read_u16(head, phnum_at, little) {
        out["segments"] = json!(count);
    }
    let shoff = if bits == 64 {
        read_u64(head, shoff_at, little)
    } else {
        read_u32(head, shoff_at, little).map(u64::from)
    };
    if let Some(offset) = shoff {
        out["sectionHeaderOffset"] = json!(format!("0x{offset:x}"));
    }
    // 壳判断不在这里：`bin.pack.detect` 才是唯一口径（节表 + 特征串 + 结构信号）。
    // 早先这里有一个只认 ELF `UPX!` 的 packerHint，PE 样本永远拿不到，
    // 两个入口给同一件事两套答案，比没有更糟。
    out
}

fn pe(head: &[u8], mut out: Value) -> Option<Value> {
    let pe_offset = read_u32(head, 0x3c, true)? as usize;
    if head.get(pe_offset..pe_offset + 4)? != b"PE\0\0" {
        return None;
    }
    let coff = pe_offset + 4;
    let machine = read_u16(head, coff, true)?;
    let sections = read_u16(head, coff + 2, true)?;
    let stamp = read_u32(head, coff + 4, true)?;

    out["format"] = json!("pe");
    out["sections"] = json!(sections);
    out["machine"] = json!(machine);
    let (arch, bits) = match machine {
        0x014c => (Some("x86"), Some(32)),
        0x8664 => (Some("x86_64"), Some(64)),
        0x01c0 | 0x01c4 => (Some("arm"), Some(32)),
        0xaa64 => (Some("arm64"), Some(64)),
        _ => (None, None),
    };
    if let Some(arch) = arch {
        out["arch"] = json!(arch);
    }
    if let Some(bits) = bits {
        out["bits"] = json!(bits);
    }
    if stamp > 0 {
        if let Some(time) = chrono::DateTime::from_timestamp(stamp as i64, 0) {
            out["compileTime"] = json!(time.to_rfc3339());
        }
    }

    let optional = coff + 20;
    let magic = read_u16(head, optional, true)?;
    out["peKind"] = json!(match magic {
        0x10b => "pe32",
        0x20b => "pe32+",
        0x107 => "rom",
        _ => "unknown",
    });
    if let Some(entry) = read_u32(head, optional + 16, true) {
        out["entry"] = json!(format!("0x{entry:x}"));
    }
    if let Some(subsystem) = read_u16(head, optional + 68, true) {
        out["subsystem"] = json!(match subsystem {
            1 => "native",
            2 => "windows-gui",
            3 => "windows-console",
            9 => "windows-ce",
            10 => "efi",
            _ => "other",
        });
    }
    Some(out)
}

fn cpu_arch(cpu: i32) -> Option<&'static str> {
    match cpu {
        7 => Some("i386"),
        0x0100_0007 => Some("x86_64"),
        12 => Some("arm"),
        0x0100_000c => Some("arm64"),
        _ => None,
    }
}

fn macho(head: &[u8], mut out: Value, is_64: bool, little: bool) -> Value {
    out["format"] = json!("macho");
    out["bits"] = json!(if is_64 { 64 } else { 32 });
    out["endian"] = json!(if little { "little" } else { "big" });
    if let Some(cpu) = read_u32(head, 4, little) {
        if let Some(arch) = cpu_arch(cpu as i32) {
            out["arch"] = json!(arch);
        }
        out["cpuType"] = json!(cpu);
    }
    if let Some(file_type) = read_u32(head, 12, little) {
        out["machoType"] = json!(match file_type {
            1 => "object",
            2 => "execute",
            6 => "dylib",
            8 => "bundle",
            _ => "other",
        });
    }
    if let Some(commands) = read_u32(head, 16, little) {
        out["loadCommands"] = json!(commands);
    }
    out
}

// ---------------------------------------------------------------- 哈希

fn run_hash(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let (path, size) = match target_of(ctx, args) {
        Ok(found) => found,
        Err(failure) => return failure,
    };
    if size > HASH_LIMIT {
        return ToolResult::fail(format!(
            "文件 {} 超过哈希上限（{} MiB）：先切出需要分析的部分再算",
            size,
            HASH_LIMIT / (1024 * 1024)
        ));
    }
    let wanted: Vec<String> = args
        .get("algorithms")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|item| item.to_ascii_lowercase())
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();
    let all = wanted.is_empty();
    let want = |name: &str| all || wanted.iter().any(|item| item == name);

    let mut md5 = Md5::new();
    let mut sha1 = Sha1::new();
    let mut sha256 = Sha256::new();
    let mut file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) => return ToolResult::fail(format!("打开文件失败：{error}")),
    };
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => return ToolResult::fail(format!("读取文件失败：{error}")),
        };
        let chunk = &buffer[..read];
        md5.update(chunk);
        sha1.update(chunk);
        sha256.update(chunk);
    }

    let mut payload = json!({
        "path": path.display().to_string(),
        "size": size,
    });
    if want("md5") {
        payload["md5"] = json!(hex(&md5.finalize()));
    }
    if want("sha1") {
        payload["sha1"] = json!(hex(&sha1.finalize()));
    }
    if want("sha256") {
        payload["sha256"] = json!(hex(&sha256.finalize()));
    }
    respond(ctx, "hashes", payload)
}

// ---------------------------------------------------------------- 字符串

fn run_strings(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let (path, size) = match target_of(ctx, args) {
        Ok(found) => found,
        Err(failure) => return failure,
    };
    let min_len = args
        .get("minLen")
        .and_then(Value::as_u64)
        .map(|value| value.clamp(3, 64) as usize)
        .unwrap_or(4);
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map(|value| (value as usize).clamp(1, STRINGS_MAX))
        .unwrap_or(STRINGS_DEFAULT);
    let encoding = args
        .get("encoding")
        .and_then(Value::as_str)
        .unwrap_or("both");
    let ascii = encoding != "utf16";
    let utf16 = encoding != "ascii";

    let head = match read_head(&path, STRINGS_SCAN) {
        Ok(bytes) => bytes,
        Err(error) => return ToolResult::fail(error),
    };
    let (items, total) = collect_strings(&head, min_len, limit, ascii, utf16);

    let payload = json!({
        "path": path.display().to_string(),
        "size": size,
        "scanned": head.len().min(size as usize),
        "truncated": total > items.len() || (size as usize) > head.len(),
        "count": items.len(),
        "total": total,
        "strings": items,
    });
    respond(ctx, "strings", payload)
}

/// 提取并去重。返回（返回条目, 去重后的总数）。
pub fn collect_strings(
    bytes: &[u8],
    min_len: usize,
    limit: usize,
    ascii: bool,
    utf16: bool,
) -> (Vec<Value>, usize) {
    // 顺序扫描两种编码，命中后去重：同一条字符串在文件里出现几十次很常见，
    // 全列出来只会把上下文塞满。
    let mut seen: std::collections::HashMap<String, (usize, &'static str, usize)> =
        std::collections::HashMap::new();
    let mut order: Vec<String> = Vec::new();

    let mut push = |text: String, offset: usize, kind: &'static str| {
        if let Some(entry) = seen.get_mut(&text) {
            entry.2 += 1;
            return;
        }
        seen.insert(text.clone(), (offset, kind, 1));
        order.push(text);
    };

    if ascii {
        let mut start: Option<usize> = None;
        for (index, byte) in bytes.iter().enumerate() {
            if (0x20..=0x7e).contains(byte) {
                if start.is_none() {
                    start = Some(index);
                }
            } else if let Some(from) = start.take() {
                if index - from >= min_len {
                    push(
                        String::from_utf8_lossy(&bytes[from..index]).to_string(),
                        from,
                        "ascii",
                    );
                }
            }
        }
        // 文件以可打印字符结尾时，最后一段没有"终止字节"来触发，单独收尾。
        if let Some(from) = start {
            if bytes.len() - from >= min_len {
                push(
                    String::from_utf8_lossy(&bytes[from..]).to_string(),
                    from,
                    "ascii",
                );
            }
        }
    }

    if utf16 {
        let mut index = 0usize;
        while index + 1 < bytes.len() {
            let low = bytes[index];
            let high = bytes[index + 1];
            if high == 0 && (0x20..=0x7e).contains(&low) {
                let start = index;
                let mut end = index;
                while end + 1 < bytes.len()
                    && bytes[end + 1] == 0
                    && (0x20..=0x7e).contains(&bytes[end])
                {
                    end += 2;
                }
                if (end - start) / 2 >= min_len {
                    let text: String = bytes[start..end]
                        .chunks_exact(2)
                        .map(|pair| pair[0] as char)
                        .collect();
                    push(text, start, "utf16le");
                }
                index = end.max(index + 2);
            } else {
                index += 1;
            }
        }
    }

    let total = order.len();
    let items = order
        .into_iter()
        .filter_map(|text| {
            let (offset, kind, hits) = seen.get(&text)?.clone();
            let tags = classify(&text);
            Some(json!({
                "offset": offset,
                "text": text.chars().take(240).collect::<String>(),
                "encoding": kind,
                "hits": hits,
                "tags": tags,
            }))
        })
        .take(limit)
        .collect();
    (items, total)
}

/// 可疑标记。规则刻意宽松：宁可多标，让人自己判断。
pub fn classify(text: &str) -> Vec<&'static str> {
    let lower = text.to_ascii_lowercase();
    let mut tags: Vec<&'static str> = Vec::new();
    if lower.contains("http://") || lower.contains("https://") {
        tags.push("url");
    } else if looks_like_domain(&lower) {
        tags.push("domain");
    }
    if looks_like_ipv4(&lower) {
        tags.push("ipv4");
    }
    if lower.contains("cmd.exe")
        || lower.contains("powershell")
        || lower.contains("/bin/sh")
        || lower.contains("/bin/bash")
        || lower.starts_with("cmd ")
    {
        tags.push("command");
    }
    if lower.contains(":\\")
        || lower.contains("\\windows\\")
        || lower.starts_with("/etc/")
        || lower.starts_with("/usr/")
        || lower.starts_with("/tmp/")
    {
        tags.push("path");
    }
    if lower.contains("begin private key")
        || lower.contains("begin rsa private key")
        || lower.contains("begin certificate")
    {
        tags.push("key");
    }
    tags
}

fn looks_like_ipv4(text: &str) -> bool {
    let mut parts = text.split('.');
    let mut count = 0;
    for part in parts.by_ref() {
        count += 1;
        if count > 4 {
            return false;
        }
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
        if part.parse::<u16>().map(|value| value > 255).unwrap_or(true) {
            return false;
        }
    }
    count == 4
}

fn looks_like_domain(text: &str) -> bool {
    if !text.contains('.') || text.contains(' ') || text.len() > 120 {
        return false;
    }
    let tail = text.rsplit('.').next().unwrap_or("");
    matches!(
        tail,
        "com" | "net" | "org" | "cn" | "io" | "ru" | "xyz" | "top" | "info" | "biz" | "cc"
    )
}

// ---------------------------------------------------------------- 壳识别

/// 默认只读前 8 MiB：PE 节表、UPX stub、.NET 元数据都在这个范围里。
const PACK_HEAD: u64 = 8 * 1024 * 1024;
/// `deep` 模式的上限。再大就不是"看壳"而是全盘扫描了。
const PACK_SCAN: u64 = 64 * 1024 * 1024;
/// 熵超过这条线，基本可以认定内容被压缩或加密过（普通代码约 6.0–6.6）。
const ENTROPY_PACKED: f64 = 7.2;
/// 节区标志位：可执行 / 可写。
const SCN_MEM_EXECUTE: u32 = 0x2000_0000;
const SCN_MEM_WRITE: u32 = 0x8000_0000;

/// 特征强度。
///
/// `Low` 只说明"像"，`High` 是业界公认的独有标记，`Certain` 是专有节名——
/// 节名在 PE 头里，是结构事实；特征串可能只是编译器内嵌的字符串，永远到不了 Certain。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Strength {
    Low,
    High,
    Certain,
}

impl Strength {
    fn as_str(self) -> &'static str {
        match self {
            Strength::Certain => "certain",
            Strength::High => "high",
            Strength::Low => "low",
        }
    }
}

/// 一条可匹配的特征。
///
/// 统一用 `&'static str` 而不是字节串：表里全是 ASCII，扫描时再编码即可，
/// 宽字符标记当场插零字节——省掉一堆 `b"..."` 的类型转换噪音。
///
/// `Weak*` 是**共享标记**：`.adata` 这类节名好几个壳都在用，单独见到只能说"像"。
/// 不加这一档，`ASProtect` 就会因为一个 `.adata` 节被判成 high，那是误报。
enum Mark {
    /// 节名，忽略大小写。
    Section(&'static str),
    /// ASCII 特征串。
    Ascii(&'static str),
    /// UTF-16LE 特征串（.NET 与部分混淆器的字符串常量是宽字符）。
    Wide(&'static str),
    /// 共享节名：只把置信度抬到 low。
    WeakSection(&'static str),
}

struct Packer {
    name: &'static str,
    strength: Strength,
    /// 认出来之后该往哪走。写具体动作，不写"建议脱壳"这种废话。
    hint: &'static str,
    marks: &'static [Mark],
}

/// 壳 / 保护器 / 打包器特征表。
///
/// 顺序有意义：同级命中按表序返回，UPX 排第一是因为它最常见。
/// 表里只放**能落到证据上**的标记——不放"熵高"这类推测，那是结构信号的事。
const PACKERS: &[Packer] = &[
    Packer {
        name: "UPX",
        strength: Strength::Certain,
        hint: "先 `upx -d`：UPX 会自己重建 IAT 与节表，成功率远高于手工 dump；失败再找 OEP 落地后 dump + 修导入表。",
        marks: &[
            Mark::Section("UPX0"),
            Mark::Section("UPX1"),
            Mark::Section("UPX2"),
            Mark::Section(".UPX"),
            Mark::Ascii("UPX!"),
            Mark::Ascii("$Info: This file is packed with the UPX"),
        ],
    },
    Packer {
        name: "VMProtect",
        strength: Strength::Certain,
        hint: "VM 保护。别指望静态全量还原：先在 xref 里定位 handler 表与校验分支，只对关键判断做动态跟踪。",
        marks: &[
            Mark::Section(".vmp0"),
            Mark::Section(".vmp1"),
            Mark::Section(".vmp2"),
            Mark::Ascii("VMProtect"),
            Mark::Ascii(".vmp"),
        ],
    },
    Packer {
        name: "Themida / WinLicense",
        strength: Strength::Certain,
        hint: "带反调试与代码虚拟化。先确认有没有硬件断点/时序检测，静态侧只取字符串与资源证据，别急着跑。",
        marks: &[
            Mark::Section(".themida"),
            Mark::Section(".winlic"),
            Mark::Ascii("Themida"),
            Mark::Ascii("WinLicense"),
        ],
    },
    Packer {
        name: "ASProtect",
        strength: Strength::High,
        hint: "找 pushad/popad 对，单步到 OEP 后 dump；IAT 一般被加密，需要重建。",
        marks: &[
            Mark::Section(".aspr"),
            Mark::WeakSection(".adata"),
            Mark::Ascii("ASProtect"),
        ],
    },
    Packer {
        name: "ASPack",
        strength: Strength::High,
        hint: "入口在 .aspack 段。走到 OEP 再 dump，压缩段可以直接丢弃。",
        marks: &[Mark::Section(".aspack"), Mark::Ascii("ASPack")],
    },
    Packer {
        name: "PECompact",
        strength: Strength::High,
        hint: "入口跳板很浅，通常单步几十条指令就到 OEP；dump 后修 IAT。",
        marks: &[
            Mark::Ascii("PECompact2"),
            Mark::Ascii("PECompact"),
            Mark::Ascii("PEC2"),
        ],
    },
    Packer {
        name: "MPRESS",
        strength: Strength::High,
        hint: "LZMA 压缩壳。`mpress -d` 只对自家产物有效，其余情况走 OEP dump。",
        marks: &[
            Mark::Section(".MPRESS1"),
            Mark::Section(".MPRESS2"),
            Mark::Ascii("MPRESS"),
        ],
    },
    Packer {
        name: "FSG",
        strength: Strength::High,
        hint: "老式压缩壳，特征极短。走到 OEP 后 dump，注意原程序可能只有一两个节。",
        marks: &[Mark::Ascii("FSG!")],
    },
    Packer {
        name: "Petite",
        strength: Strength::High,
        hint: "入口在最后一个节。dump 时注意把原节表恢复回去。",
        marks: &[Mark::Section(".petite"), Mark::Ascii("petite")],
    },
    Packer {
        name: "Armadillo",
        strength: Strength::High,
        hint: "有 Nanomites 变种（int3 回填）。先判断版本，再决定是脱壳还是只在动态侧观察。",
        marks: &[
            Mark::Ascii("Armadillo"),
            Mark::WeakSection(".text1"),
        ],
    },
    Packer {
        name: "Obsidium",
        strength: Strength::High,
        hint: "多层代码变形。静态反汇编会被带偏，优先动态记录真实执行路径。",
        marks: &[Mark::Ascii("Obsidium")],
    },
    Packer {
        name: "Enigma Protector",
        strength: Strength::Certain,
        hint: "带授权与虚拟化。先看授权校验分支（通常是一个返回布尔的小函数），再谈脱壳。",
        marks: &[
            Mark::Section(".enigma1"),
            Mark::Section(".enigma2"),
            Mark::Ascii("EnigmaProtector"),
        ],
    },
    Packer {
        name: "EXECryptor",
        strength: Strength::High,
        hint: "入口被搬到壳段，IAT 加密。dump 后必须重建导入表才能静态读。",
        marks: &[Mark::Ascii("EXECryptor")],
    },
    Packer {
        name: "tElock",
        strength: Strength::High,
        hint: "反调试较重（多重 SEH）。先在受控环境里跑，再考虑脱壳。",
        marks: &[Mark::Ascii("tElock")],
    },
    Packer {
        name: "MoleBox",
        strength: Strength::High,
        hint: "单文件打包器，运行时释放到临时目录。别脱壳——直接监控临时目录里落地的文件。",
        marks: &[Mark::Ascii("MoleBox")],
    },
    Packer {
        name: "Xenocode",
        strength: Strength::High,
        hint: "文件与注册表都虚拟化。重点看虚拟文件系统描述表，而不是代码段。",
        marks: &[Mark::Ascii("Xenocode")],
    },
    Packer {
        name: "PELock",
        strength: Strength::High,
        hint: "授权 + 反调试。先定位授权检查点，再看壳本身。",
        marks: &[Mark::Ascii("PELock")],
    },
    Packer {
        name: "Yoda's Crypter",
        strength: Strength::High,
        hint: "老式加密壳，反调试靠 SEH 链。走 OEP 后 dump。",
        marks: &[Mark::Ascii("Yoda's Crypter"), Mark::Ascii("YodaCrypter")],
    },
    Packer {
        name: "WWPack32",
        strength: Strength::High,
        hint: "压缩壳，节名被抹掉。按入口点落段判断，走到 OEP 再 dump。",
        marks: &[Mark::Ascii("WWPACK32")],
    },
    Packer {
        name: "NeoLite",
        strength: Strength::High,
        hint: "老式压缩壳。dump 后检查重定位表是否被破坏。",
        marks: &[Mark::Ascii("NeoLite")],
    },
    Packer {
        name: "kkrunchy",
        strength: Strength::High,
        hint: "极小体积壳，常出现在恶意样本里。带 CRC 校验，改动样本会导致自毁。",
        marks: &[Mark::Ascii("kkrunchy")],
    },
    Packer {
        name: ".NET Reactor",
        strength: Strength::High,
        hint: "方法体加密 + 反调试。先用 de4dot 试一遍，剩余的按调用代理逐步还原。",
        marks: &[
            Mark::Ascii("NETReactor"),
            Mark::Ascii(".reactor"),
            Mark::Ascii("Reactored"),
        ],
    },
    Packer {
        name: "ConfuserEx",
        strength: Strength::High,
        hint: "常量与字符串加密，方法体替换成代理调用。先 de4dot，再针对残留的 switch 混淆还原。",
        marks: &[
            Mark::Ascii("ConfuserEx"),
            Mark::Wide("ConfuserEx"),
            Mark::Ascii("ConfusedByAttribute"),
            Mark::Ascii(".confuser"),
        ],
    },
    Packer {
        name: "SmartAssembly",
        strength: Strength::High,
        hint: "字符串压缩 + 控制流混淆。de4dot 能解大部分，剩下去看资源里的字符串解压器。",
        marks: &[Mark::Ascii("SmartAssembly")],
    },
    Packer {
        name: "Dotfuscator",
        strength: Strength::High,
        hint: "以重命名与字符串加密为主，方法体基本完整。还原符号比脱壳更有价值。",
        marks: &[Mark::Ascii("DotfuscatorAttribute"), Mark::Ascii("Dotfuscator")],
    },
    Packer {
        name: "Babel Obfuscator",
        strength: Strength::High,
        hint: "字符串加密 + 资源保护。de4dot 支持有限，重点找解密函数。",
        marks: &[Mark::Ascii("BabelObfuscatorAttribute")],
    },
    Packer {
        name: "Eazfuscator",
        strength: Strength::High,
        hint: "常量加密 + 控制流扁平化。找 <PrivateImplementationDetails> 里的解密桩。",
        marks: &[Mark::Ascii("Eazfuscator")],
    },
    Packer {
        name: "PyInstaller",
        strength: Strength::High,
        hint: "不是壳，是打包器。用 pyinstxtractor 拆出 PYZ，再按 pyc magic 对应的 Python 版本反编译。",
        marks: &[
            Mark::Ascii("MEI\u{000c}\u{000b}\u{000a}\u{000b}\u{000e}"),
            Mark::Ascii("PyInstaller"),
            Mark::Ascii("pyi-"),
        ],
    },
    Packer {
        name: "py2exe",
        strength: Strength::High,
        hint: "拆出 PYZ 后反编译；pyc magic 必须和 Python 版本对上，否则全是乱码。",
        marks: &[Mark::Ascii("PYTHONSCRIPT")],
    },
    Packer {
        name: "Nuitka",
        strength: Strength::High,
        hint: "编译成 C 再打包，没有字节码可反编译。只能按 native 二进制分析，别找 pyc。",
        marks: &[Mark::Ascii("Nuitka")],
    },
    Packer {
        name: "Enigma Virtual Box",
        strength: Strength::High,
        hint: "文件虚拟化，运行时把内嵌文件映射进进程。看 .enigma 目录与运行时释放路径。",
        marks: &[Mark::Ascii("Enigma Virtual Box")],
    },
    Packer {
        name: "360 加固",
        strength: Strength::High,
        hint: "libjiagu* 是特征。壳入口在 Application.attachBaseContext 之后，脱壳点通常在它调用完 dex 加载之后。",
        marks: &[
            Mark::Ascii("libjiagu"),
            Mark::Ascii("libjiagu_art"),
            Mark::Ascii("libjiagu_x86"),
        ],
    },
    Packer {
        name: "腾讯加固",
        strength: Strength::High,
        hint: "libshell* 是特征。先看壳的 dex 抽取时机，再找内存里完整的 dex。",
        marks: &[
            Mark::Ascii("libshella"),
            Mark::Ascii("libshellx"),
            Mark::Ascii("libshell-super"),
        ],
    },
    Packer {
        name: "梆梆加固",
        strength: Strength::High,
        hint: "libsec* 与 libDexHelper 是特征。脱壳点在 dex 被完整加载进内存之后。",
        marks: &[
            Mark::Ascii("libsecexe"),
            Mark::Ascii("libsecmain"),
            Mark::Ascii("libDexHelper"),
        ],
    },
    Packer {
        name: "阿里加固",
        strength: Strength::High,
        hint: "libmobisec 是特征。先确认是整体加固还是仅 dex 加密。",
        marks: &[Mark::Ascii("libmobisec")],
    },
    Packer {
        name: "百度加固",
        strength: Strength::High,
        hint: "libbaiduprotect 是特征。注意它有反调试与完整性校验，改动前先备份。",
        marks: &[Mark::Ascii("libbaiduprotect")],
    },
    Packer {
        name: "娜迦加固",
        strength: Strength::High,
        hint: "libchaosvmp 是特征。VM 保护，优先找 dex 完整落地的时机。",
        marks: &[Mark::Ascii("libchaosvmp")],
    },
    Packer {
        name: "爱加密",
        strength: Strength::High,
        hint: "libexecmain 是特征。先看 attachBaseContext 链，再找内存 dex。",
        marks: &[Mark::Ascii("libexecmain"), Mark::Ascii("ijiami")],
    },
];

// ---------------------------------------------------------------- Android 壳

/// 一个 Android 加固方案的识别规则。
///
/// 与上面的 `Packer`（PE/ELF 视角，靠节名 + 字节特征串）不同，APK 的壳
/// 证据几乎全在**文件名清单**上：`lib/` 下的加固 so、`assets/` 里的特征文件、
/// `classes.dex` 的数量与体积。这些文件名在 zip 中央目录里是明文存的，
/// 不依赖解压，扫一遍中央目录就能拿到结构事实——比「编译器内嵌字符串」硬得多。
struct AndroidPacker {
    name: &'static str,
    /// `lib/` 下出现的加固 so 文件名（不含路径前缀）。这是最硬的结构证据：
    /// 一个正常 app 不会带 `libjiagu.so`。
    libs: &'static [&'static str],
    /// `assets/` 或根目录下出现的特征文件（子串匹配，大小写敏感）。
    assets: &'static [&'static str],
    hint: &'static str,
}

/// Android 加固方案特征表。顺序有意义：同级命中按表序返回。
const ANDROID_PACKERS: &[AndroidPacker] = &[
    AndroidPacker {
        name: "360 加固",
        libs: &["libjiagu.so", "libjiagu_art.so", "libjiagu_x86.so", "libjiagu_a64.so"],
        assets: &["jiagu"],
        hint: "libjiagu* 是特征。壳入口在 Application.attachBaseContext 之后，脱壳点通常在它调用完 dex 加载之后。",
    },
    AndroidPacker {
        name: "腾讯加固（乐固）",
        libs: &["libshell.so", "libshella-", "libshellx-", "libtup.so", "libexec.so"],
        assets: &["libshell", "libshella", "tosprotection"],
        hint: "libshell* 是特征。先看壳的 dex 抽取时机，再找内存里完整的 dex。",
    },
    AndroidPacker {
        name: "梆梆加固",
        libs: &["libsecexe.so", "libsecmain.so", "libSecShell.so", "libDexHelper.so", "libDexHelper-x86.so"],
        assets: &["secData0.jar", "secData", "classes0.jar"],
        hint: "libsec* 与 libDexHelper 是特征。脱壳点在 dex 被完整加载进内存之后。",
    },
    AndroidPacker {
        name: "阿里加固（聚安全）",
        libs: &["libmobisec.so", "libmobisecz.so", "libmobisecy.so"],
        assets: &["mobisec"],
        hint: "libmobisec 是特征。先确认是整体加固还是仅 dex 加密。",
    },
    AndroidPacker {
        name: "百度加固",
        libs: &["libbaiduprotect.so", "libbaiduprotect_x86.so"],
        assets: &["baiduprotect", "libbaiduprotect"],
        hint: "libbaiduprotect 是特征。注意它有反调试与完整性校验，改动前先备份。",
    },
    AndroidPacker {
        name: "娜迦加固",
        libs: &["libchaosvmp.so", "libchaosvmp-x86.so", "libddog.so", "libfdog.so"],
        assets: &["naga", "chaosvmp"],
        hint: "libchaosvmp 是特征。VM 保护，优先找 dex 完整落地的时机。",
    },
    AndroidPacker {
        name: "爱加密",
        libs: &["libexecmain.so", "libexec.so", "ijm_lib.so"],
        assets: &["ijiami.dat", "ijiami", "libexecmain"],
        hint: "libexecmain 是特征。先看 attachBaseContext 链，再找内存 dex。",
    },
    AndroidPacker {
        name: "顶象加固",
        libs: &["libx3g.so", "libDxAnti.so", "libXposedCheck.so"],
        assets: &["dxsdk", "DxAnti"],
        hint: "libDxAnti / libx3g 是特征。顶象偏业务安全，先确认是不是纯壳还是带风控。",
    },
    AndroidPacker {
        name: "网易易盾加固",
        libs: &["libnesec.so", "libwbox.so", "libmoxiang.so"],
        assets: &["nesec", "yidun"],
        hint: "libnesec 是特征。网易易盾通常伴随风控 SDK，脱壳后还要看风控链路。",
    },
];

/// zip 中央目录里挑出来的一条记录，只保留壳检测关心的列。
struct ZipEntry {
    name: String,
    /// 未压缩大小（用于算 classes.dex 的真实体积）。
    uncompressed_size: u64,
}

/// 轻量 zip 中央目录解析：只读文件名清单，不碰本地文件头、不解压。
///
/// APK 的壳特征（so 文件名、assets 文件名）都在中央目录里明文存储，
/// 所以只需要定位 EOCD → 中央目录 → 逐条读文件名，就能拿到全部结构证据。
/// 对 zip64、加密、数据描述符这些不关心的字段直接跳过，解析失败就返回 None，
/// 由调用方按「无中央目录信息」降级——纪律是「解析不出就省略，不猜」。
fn apk_entries(blob: &[u8]) -> Option<Vec<ZipEntry>> {
    // EOCD 至少 22 字节，标志是 0x06054b50。从尾部往前扫，兼容带注释的 zip。
    if blob.len() < 22 {
        return None;
    }
    let mut eocd = None;
    let scan_from = blob.len().saturating_sub(22 + 65536);
    for at in (scan_from..=blob.len() - 22).rev() {
        if &blob[at..at + 4] == b"PK\x05\x06" {
            eocd = Some(at);
            break;
        }
    }
    let eocd = eocd?;
    let count = read_u16(blob, eocd + 10, true)? as usize;
    let cd_offset = read_u32(blob, eocd + 16, true)? as usize;
    if cd_offset >= blob.len() {
        return None;
    }

    let mut entries = Vec::new();
    let mut at = cd_offset;
    for _ in 0..count {
        // 中央目录文件头：0x02014b50，固定 46 字节。
        if &blob[at..at + 4] != b"PK\x01\x02" {
            break;
        }
        let name_len = read_u16(blob, at + 28, true)? as usize;
        let extra_len = read_u16(blob, at + 30, true)? as usize;
        let comment_len = read_u16(blob, at + 32, true)? as usize;
        let uncompressed = read_u32(blob, at + 24, true)? as u64;
        let name_start = at + 46;
        let name_end = name_start.checked_add(name_len)?;
        if name_end > blob.len() {
            break;
        }
        let name = String::from_utf8_lossy(&blob[name_start..name_end]).to_string();
        entries.push(ZipEntry {
            name,
            uncompressed_size: uncompressed,
        });
        at = name_end + extra_len + comment_len;
    }
    if entries.is_empty() {
        None
    } else {
        Some(entries)
    }
}

/// APK 壳检测：纯函数，输入整个（或头部的）APK 字节与真实文件大小。
///
/// 返回与 `detect_pack` 同构的 payload，`format` 固定 `apk`。识别依据：
/// 1. `lib/` 下的加固 so 文件名（最硬，结构事实）；
/// 2. `assets/` 或根目录的特征文件；
/// 3. `classes.dex` 的数量与体积异常（抽取壳会让 classes.dex 变成极小 stub）；
/// 4. `AndroidManifest.xml` 之外，是否有 `classes.dex` 被塞进 assets（整体加固）。
fn detect_apk_pack(blob: &[u8], size: u64) -> Value {
    let entries = apk_entries(blob);

    // ---- 从条目清单提炼结构信号
    let mut lib_files: Vec<&str> = Vec::new();
    let mut classes_dex: Vec<(usize, u64)> = Vec::new();
    let mut has_manifest = false;
    let mut suspicious_assets: Vec<&str> = Vec::new();
    if let Some(list) = &entries {
        for entry in list {
            let name = entry.name.as_str();
            if name == "AndroidManifest.xml" {
                has_manifest = true;
            }
            if let Some(base) = name.rsplit('/').next() {
                if name.starts_with("lib/") && base.ends_with(".so") {
                    lib_files.push(base);
                }
            }
            if name == "classes.dex" {
                classes_dex.push((0, entry.uncompressed_size));
            } else if let Some(suffix) = name.strip_prefix("classes") {
                if let Some(rest) = suffix.strip_suffix(".dex") {
                    if let Ok(index) = rest.parse::<usize>() {
                        classes_dex.push((index, entry.uncompressed_size));
                    }
                }
            }
        }
    }
    // classes.dex 若被挪进 assets/（整体加固常见），这里也能捞到。
    if let Some(list) = &entries {
        for entry in list {
            let name = entry.name.as_str();
            if name.starts_with("assets/") && (name.ends_with(".dex") || name.ends_with(".jar")) {
                suspicious_assets.push(name);
            }
        }
    }

    // ---- 按壳特征匹配
    let mut found: Vec<Value> = Vec::new();
    let mut top: Option<Strength> = None;
    if let Some(list) = &entries {
        for packer in ANDROID_PACKERS {
            let mut evidence: Vec<String> = Vec::new();
            for entry in list {
                let base = entry.name.rsplit('/').next().unwrap_or(entry.name.as_str());
                let lib_hit = entry.name.starts_with("lib/")
                    && packer.libs.iter().any(|lib| {
                        base == *lib || base.starts_with(&lib[..lib.len().saturating_sub(1)])
                    });
                if lib_hit {
                    evidence.push(format!("lib/ 出现 {}", base));
                }
                let asset_hit = packer.assets.iter().any(|asset| entry.name.contains(asset));
                if asset_hit && !lib_hit {
                    evidence.push(format!("assets/ 出现特征 {}", base));
                }
            }
            if evidence.is_empty() {
                continue;
            }
            // so 文件名是结构事实，直接定 high；只有 assets 特征时降为 low。
            let has_lib = evidence.iter().any(|item| item.starts_with("lib/"));
            let strength = if has_lib { Strength::High } else { Strength::Low };
            top = Some(top.map_or(strength, |current| current.max(strength)));
            found.push(json!({
                "name": packer.name,
                "confidence": strength.as_str(),
                "evidence": evidence,
                "hint": packer.hint,
            }));
        }
    }
    found.sort_by(|left, right| {
        let l = left["confidence"].as_str().unwrap_or("");
        let r = right["confidence"].as_str().unwrap_or("");
        strength_rank(r).cmp(&strength_rank(l))
    });

    // ---- 结构信号
    let manifest_missing = entries.is_some() && !has_manifest;
    let dex_count = classes_dex.len();
    let min_dex_size = classes_dex.iter().map(|(_, size)| *size).min().unwrap_or(0);
    // 抽取壳的典型形态：classes.dex 只有一个且极小（stub），真实代码在 so 里。
    let dex_stub = dex_count == 1 && min_dex_size > 0 && min_dex_size < 16 * 1024;

    // 强信号只放「结构异常」：dex 被抽取成 stub、assets 里塞了 dex/jar（整体加固）。
    let strong = usize::from(dex_stub) + usize::from(!suspicious_assets.is_empty());
    let weak = usize::from(manifest_missing);

    let packed = match top {
        Some(Strength::High) | Some(Strength::Certain) => true,
        Some(Strength::Low) => strong > 0,
        None => strong >= 1 || weak >= 1,
    };
    let confidence = match top {
        Some(strength) => strength.as_str(),
        None if packed => "low",
        None => "none",
    };
    let names: Vec<&str> = found
        .iter()
        .filter_map(|item| item["name"].as_str())
        .collect();
    let joined = names.join(" / ");
    let verdict = match top {
        Some(Strength::Certain) => format!("确认被 {joined} 加固"),
        Some(Strength::High) => format!("高度疑似 {joined}"),
        Some(Strength::Low) => format!("疑似 {joined}，但只有弱特征，需要人工确认"),
        None if packed => format!("没有匹配到已知壳，但发现 {strong} 项加固结构信号，可能是私有壳或抽取壳"),
        None => "未发现加壳特征".to_string(),
    };

    let mut advice: Vec<String> = Vec::new();
    if packed {
        advice.push(
            "先确认这是抽取壳还是整体加固：抽取壳的 classes.dex 是 stub，真代码在 lib/ 下的 so 里，脱壳要抓内存 dex；整体加固则要过 Application.attachBaseContext。"
                .to_string(),
        );
        for packer in &found {
            if let Some(hint) = packer["hint"].as_str() {
                advice.push(hint.to_string());
            }
        }
    } else {
        advice.push(
            "未发现加固特征，可以直接对 classes.dex 跑 bin.dex.disasm / bin.dex.xref 拿代码级证据。"
                .to_string(),
        );
    }
    if dex_stub {
        advice.push(format!(
            "classes.dex 只有 {min_dex_size} 字节，小于 16 KiB 的 stub 几乎肯定是抽取壳：真实 dex 在运行时从 so 里释放，静态反汇编拿到的是壳加载器而非业务代码。"
        ));
    }

    let mut payload = json!({
        "format": "apk",
        "packed": packed,
        "confidence": confidence,
        "verdict": verdict,
        "packers": found,
        "signals": {
            "entryCount": entries.as_ref().map(|list| list.len()),
            "manifestPresent": entries.as_ref().map(|_| has_manifest),
            "classesDexCount": dex_count,
            "minClassesDexSize": min_dex_size,
            "dexIsStub": dex_stub,
            "nativeLibCount": lib_files.len(),
            "nativeLibs": lib_files,
            "suspiciousAssets": suspicious_assets,
            "strongSignals": strong,
            "weakSignals": weak,
        },
        "advice": advice,
    });
    // 没解析出中央目录（截断 / zip64 / 损坏）时如实标注，不装知道。
    if entries.is_none() {
        payload["note"] = json!(
            "无法解析 zip 中央目录（文件可能被截断或用了 zip64）。以下结论只基于文件头部字节，可能漏掉加固 so 证据，建议用完整样本重跑。"
        );
    }
    if size > 0 && entries.as_ref().map(|list| list.is_empty()).unwrap_or(true) {
        // 中央目录为空但有体积：可能是 zip 头损坏，如实降级。
        payload["note"] = json!("中央目录为空，无法枚举条目名，加固判断不可靠。");
    }
    payload
}

fn strength_rank(value: &str) -> u8 {
    match value {
        "certain" => 3,
        "high" => 2,
        "low" => 1,
        _ => 0,
    }
}

fn pack_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bin.pack.detect",
            "description": "判断样本是否加壳/混淆：按节名、stub 特征串、入口点所在节、可写可执行节、导入表缺失、overlay、熵值综合打分，给出壳名、置信度、证据偏移与下一步动作。只读，不执行样本。",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "相对工作目录的样本路径" },
                    "deep": { "type": "boolean", "description": "扫描整个文件找特征串（默认只看前 8 MiB，deep 提到 64 MiB）" }
                },
                "required": ["path"]
            }
        }
    })
}

fn run_pack(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let (path, size) = match target_of(ctx, args) {
        Ok(found) => found,
        Err(failure) => return failure,
    };
    let deep = args.get("deep").and_then(Value::as_bool).unwrap_or(false);
    // zip / APK 的中央目录与 EOCD 在文件末尾，读头部拿不到；对这类文件读整包
    //（上限 PACK_SCAN），否则超过 8 MiB 的加固 APK 会漏掉壳 so 证据。
    let is_zip = read_head(&path, 4)
        .map(|bytes| bytes.as_slice() == b"PK\x03\x04")
        .unwrap_or(false);
    let blob = if is_zip {
        match read_head(&path, PACK_SCAN) {
            Ok(bytes) => bytes,
            Err(error) => return ToolResult::fail(error),
        }
    } else {
        match read_head(&path, if deep { PACK_SCAN } else { PACK_HEAD }) {
            Ok(bytes) => bytes,
            Err(error) => return ToolResult::fail(error),
        }
    };
    let mut payload = detect_pack(&blob, size);
    payload["path"] = json!(path.display().to_string());
    payload["size"] = json!(size);
    payload["scannedBytes"] = json!(blob.len());
    payload["truncated"] = json!((size as usize) > blob.len());
    respond(ctx, "pack", payload)
}

/// PE 节表里我们真正关心的几列。
struct PeSection {
    name: String,
    address: u64,
    virtual_size: u64,
    raw_size: u64,
    raw_offset: u64,
    characteristics: u32,
}

struct PeFacts {
    sections: Vec<PeSection>,
    entry_rva: u64,
    /// 为 0 表示没有导入表——这是加壳最硬的结构证据之一。
    import_rva: u64,
}

/// 只解析节表与两个地址。导入了什么由 `bin.importexport.list` 负责，不重复。
fn pe_facts(head: &[u8]) -> Option<PeFacts> {
    let pe_offset = read_u32(head, 0x3c, true)? as usize;
    if head.get(pe_offset..pe_offset + 4)? != b"PE\0\0" {
        return None;
    }
    let coff = pe_offset + 4;
    let count = read_u16(head, coff + 2, true)?;
    let optional_size = read_u16(head, coff + 16, true)?;
    let optional = coff + 20;
    let is_64 = read_u16(head, optional, true)? == 0x20b;
    let entry_rva = read_u32(head, optional + 16, true).unwrap_or(0) as u64;
    // DataDirectory 起点：PE32 在 +96，PE32+ 在 +112；[1] 是导入表。
    let directories = optional + if is_64 { 112 } else { 96 };
    let import_rva = read_u32(head, directories + 8, true).unwrap_or(0) as u64;

    let table = optional + optional_size as usize;
    let mut sections = Vec::new();
    for index in 0..count.min(96) as usize {
        let at = table + index * 40;
        let Some(raw) = head.get(at..at + 40) else {
            break;
        };
        sections.push(PeSection {
            name: String::from_utf8_lossy(&raw[..8])
                .trim_end_matches('\0')
                .to_string(),
            address: read_u32(raw, 12, true).unwrap_or(0) as u64,
            virtual_size: read_u32(raw, 8, true).unwrap_or(0) as u64,
            raw_size: read_u32(raw, 16, true).unwrap_or(0) as u64,
            raw_offset: read_u32(raw, 20, true).unwrap_or(0) as u64,
            characteristics: read_u32(raw, 36, true).unwrap_or(0),
        });
    }
    Some(PeFacts {
        sections,
        entry_rva,
        import_rva,
    })
}

/// 文件偏移落在哪个节的原始数据里。
fn section_at(sections: &[PeSection], offset: usize) -> Option<&str> {
    let offset = offset as u64;
    sections
        .iter()
        .find(|section| {
            offset >= section.raw_offset && offset < section.raw_offset + section.raw_size
        })
        .map(|section| section.name.as_str())
}

/// 预编码的特征串。
///
/// 宽字符在这里就展开成字节流，扫描循环里只剩一次 `==`。
struct Needle {
    packer: usize,
    mark: usize,
    bytes: Vec<u8>,
}

fn build_needles() -> Vec<Needle> {
    let mut out = Vec::new();
    for (packer, entry) in PACKERS.iter().enumerate() {
        for (mark, item) in entry.marks.iter().enumerate() {
            match item {
                Mark::Section(_) | Mark::WeakSection(_) => {}
                Mark::Ascii(text) => out.push(Needle {
                    packer,
                    mark,
                    bytes: text.as_bytes().to_vec(),
                }),
                Mark::Wide(text) => {
                    let mut encoded = Vec::with_capacity(text.len() * 2);
                    for byte in text.as_bytes() {
                        encoded.push(*byte);
                        encoded.push(0);
                    }
                    out.push(Needle {
                        packer,
                        mark,
                        bytes: encoded,
                    });
                }
            }
        }
    }
    out
}

/// 一遍过完全部特征串，返回 `hits[packer] = [(mark, offset)]`。
///
/// 逐个特征各扫一遍在 64 MiB 上要几十秒；这里按首字节建桶，单次遍历即可，
/// 命中的特征串只在首字节对得上时才做整段比较。
fn scan_needles(blob: &[u8], needles: &[Needle]) -> Vec<Vec<(usize, usize)>> {
    let mut hits: Vec<Vec<(usize, usize)>> = vec![Vec::new(); PACKERS.len()];
    let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); 256];
    for (index, needle) in needles.iter().enumerate() {
        if let Some(first) = needle.bytes.first() {
            buckets[*first as usize].push(index);
        }
    }
    for (offset, byte) in blob.iter().enumerate() {
        let bucket = &buckets[*byte as usize];
        if bucket.is_empty() {
            continue;
        }
        for &index in bucket {
            let needle = &needles[index];
            if blob.len() - offset < needle.bytes.len() {
                continue;
            }
            if &blob[offset..offset + needle.bytes.len()] != needle.bytes.as_slice() {
                continue;
            }
            let slot = &mut hits[needle.packer];
            if !slot.iter().any(|(mark, _)| *mark == needle.mark) {
                slot.push((needle.mark, offset));
            }
        }
    }
    hits
}

/// 香农熵（bit/byte）。全零得 0，均匀分布得 8。
fn entropy(bytes: &[u8]) -> f64 {
    if bytes.is_empty() {
        return 0.0;
    }
    let mut counts = [0u32; 256];
    for byte in bytes {
        counts[*byte as usize] += 1;
    }
    let total = bytes.len() as f64;
    counts
        .iter()
        .filter(|count| **count > 0)
        .fold(0.0, |sum, count| {
            let p = *count as f64 / total;
            sum - p * p.log2()
        })
}

/// 纯函数：给一段内容判壳。`size` 是文件真实大小，只在算 overlay 时用到。
pub fn detect_pack(blob: &[u8], size: u64) -> Value {
    // APK 的壳证据全在文件名清单上（加固 so / assets 特征 / classes.dex 结构），
    // 跟 PE 的「节名 + stub 特征串 + 结构信号」是两套逻辑，先按格式分派。
    // 判断依据是中央目录里有没有 `AndroidManifest.xml` 条目——这比 `identify`
    // 的「头部窗口里搜到文件名」可靠（真实 APK 的文件名在尾部中央目录，头部窗口搜不到）。
    let is_apk = apk_entries(blob)
        .map(|entries| entries.iter().any(|entry| entry.name == "AndroidManifest.xml"))
        .unwrap_or(false);
    if is_apk {
        return detect_apk_pack(blob, size);
    }
    let facts = pe_facts(blob);
    let empty = Vec::new();
    let sections = facts
        .as_ref()
        .map(|item| &item.sections)
        .unwrap_or(&empty);

    let needles = build_needles();
    let hits = scan_needles(blob, &needles);

    // ---- 特征串与节名
    let mut found: Vec<(Strength, Value)> = Vec::new();
    let mut top: Option<Strength> = None;
    // 至少有一个壳名只靠特征串命中——结论要附一句人工确认的提醒。
    let mut string_only = false;
    for (index, packer) in PACKERS.iter().enumerate() {
        let mut evidence: Vec<String> = Vec::new();
        // 只命中共用标记（`.adata` 之类）时，置信度只能算 low。
        let mut weak = true;
        // 节名是 PE 头里的结构事实；只命中特征串时最高判到 high。
        let mut structural = false;
        for mark in packer.marks {
            let name = match mark {
                Mark::Section(name) | Mark::WeakSection(name) => name,
                _ => continue,
            };
            if let Some(section) = sections
                .iter()
                .find(|item| item.name.eq_ignore_ascii_case(name))
            {
                evidence.push(format!("节名 {}", section.name));
                if let Mark::Section(_) = mark {
                    weak = false;
                    structural = true;
                }
            }
        }
        for &(mark, offset) in &hits[index] {
            // 证据要说清命中落在哪个节：字符串可能只是编译器内嵌的常量，
            // 见到 `.rdata` 里的 "VMProtect" 和见到入口节里的，判断完全不同。
            let at = match section_at(sections, offset) {
                Some(name) => format!("0x{offset:x}（{name} 节）"),
                None => format!("0x{offset:x}"),
            };
            match &packer.marks[mark] {
                Mark::Ascii(text) => {
                    evidence.push(format!("{at} 出现 \"{text}\""));
                    weak = false;
                }
                Mark::Wide(text) => {
                    evidence.push(format!("{at} 出现宽字符 \"{text}\""));
                    weak = false;
                }
                Mark::Section(_) | Mark::WeakSection(_) => {}
            }
        }
        if evidence.is_empty() {
            continue;
        }
        let strength = if weak {
            Strength::Low
        } else if structural {
            packer.strength
        } else {
            packer.strength.min(Strength::High)
        };
        if !structural {
            string_only = true;
        }
        top = Some(top.map_or(strength, |current| current.max(strength)));
        found.push((
            strength,
            json!({
                "name": packer.name,
                "confidence": strength.as_str(),
                "evidence": evidence,
                "hint": packer.hint,
            }),
        ));
    }
    found.sort_by(|left, right| right.0.cmp(&left.0));
    let packers: Vec<Value> = found.into_iter().map(|(_, value)| value).collect();

    // ---- 结构信号
    let mut rows: Vec<Value> = Vec::new();
    let mut writable_exec: Vec<String> = Vec::new();
    let mut virtual_gap: Vec<String> = Vec::new();
    let mut last_end = 0u64;
    for section in sections {
        let executable = section.characteristics & SCN_MEM_EXECUTE != 0;
        let writable = section.characteristics & SCN_MEM_WRITE != 0;
        if executable && writable {
            writable_exec.push(section.name.clone());
        }
        if section.virtual_size >= 0x1000
            && (section.raw_size == 0
                || section.virtual_size > section.raw_size.saturating_mul(4))
        {
            virtual_gap.push(section.name.clone());
        }
        last_end = last_end.max(section.raw_offset + section.raw_size);

        // 节内熵只在数据确实读进来了的时候算：截断的节算出来的熵是假的。
        let start = section.raw_offset as usize;
        let end = start.saturating_add(section.raw_size as usize).min(blob.len());
        let usable = section.raw_size >= 512 && start < end;
        let mut row = json!({
            "name": section.name,
            "writable": writable,
            "executable": executable,
            "virtualSize": section.virtual_size,
            "rawSize": section.raw_size,
        });
        if usable {
            let value = entropy(&blob[start..end]);
            row["entropy"] = json!((value * 100.0).round() / 100.0);
        }
        rows.push(row);
    }

    let entry_rva = facts.as_ref().map(|item| item.entry_rva).unwrap_or(0);
    let entry_section = facts.as_ref().and_then(|item| {
        item.sections.iter().find(|section| {
            let span = section.virtual_size.max(section.raw_size).max(1);
            entry_rva >= section.address && entry_rva < section.address + span
        })
    });
    let entry_writable_exec = entry_section
        .map(|section| {
            section.characteristics & (SCN_MEM_EXECUTE | SCN_MEM_WRITE)
                == (SCN_MEM_EXECUTE | SCN_MEM_WRITE)
        })
        .unwrap_or(false);
    let imports = facts.as_ref().map(|item| item.import_rva != 0);
    let overlay = if facts.is_some() && last_end > 0 {
        size.saturating_sub(last_end)
    } else {
        0
    };
    let file_entropy = entropy(blob);

    let strong = usize::from(!writable_exec.is_empty())
        + usize::from(entry_writable_exec)
        + usize::from(imports == Some(false))
        + usize::from(!virtual_gap.is_empty());
    let weak = usize::from(overlay > 0) + usize::from(file_entropy >= ENTROPY_PACKED);

    let packed = match top {
        Some(Strength::Certain) | Some(Strength::High) => true,
        Some(Strength::Low) => strong + weak > 0,
        None => strong >= 2 || (strong >= 1 && weak >= 2),
    };
    let confidence = match top {
        Some(strength) => strength.as_str(),
        None if packed => "low",
        None => "none",
    };
    let names: Vec<&str> = packers
        .iter()
        .filter_map(|item| item["name"].as_str())
        .collect();
    let joined = names.join(" / ");
    let verdict = match top {
        Some(Strength::Certain) => format!("确认被 {joined} 加壳"),
        Some(Strength::High) => format!("高度疑似 {joined}"),
        Some(Strength::Low) => format!("疑似 {joined}，但只有弱特征，需要人工确认"),
        None if packed => {
            format!("没有匹配到已知壳，但有 {strong} 项结构异常，可能是私有壳或保护器")
        }
        None => "未发现加壳特征".to_string(),
    };

    let mut advice: Vec<String> = Vec::new();
    if packed {
        advice.push(
            "先 cp 一份到 Scratch 工作区再动手：原始样本只读，任何 dump 或修补都写在新文件上。"
                .to_string(),
        );
        for packer in &packers {
            if let Some(hint) = packer["hint"].as_str() {
                advice.push(hint.to_string());
            }
        }
    } else {
        advice.push(
            "可以直接进 bin.importexport.list 与 bin.strings.extract：未加壳时这两步收益最高。"
                .to_string(),
        );
    }
    if file_entropy >= ENTROPY_PACKED && packers.is_empty() {
        advice.push(format!(
            "整体熵 {file_entropy:.2} 却没有已知壳特征：可能是加密数据段或私有壳，先确认这段数据是不是代码。"
        ));
    }
    if imports == Some(false) {
        advice.push(
            "导入表为空：正常程序至少要导入几个系统 DLL，这几乎一定是壳或自举代码。".to_string(),
        );
    }

    let mut payload = json!({
        "format": identify(blob, size)["format"],
        "packed": packed,
        "confidence": confidence,
        "verdict": verdict,
        "packers": packers,
        "signals": {
            "fileEntropy": (file_entropy * 100.0).round() / 100.0,
            "entropyThreshold": ENTROPY_PACKED,
            "sections": rows,
            "entrySection": entry_section.map(|section| section.name.clone()),
            "entryInWritableExecutable": entry_writable_exec,
            "writableExecutableSections": writable_exec,
            "virtualSizeOutweighsRaw": virtual_gap,
            "importTable": imports,
            "overlayBytes": overlay,
            "strongSignals": strong,
            "weakSignals": weak,
        },
        "advice": advice,
    });
    if string_only {
        payload["note"] = json!(
            "有壳名只靠特征串命中（偏移见 evidence）。编译器内嵌的字符串、其他工具留下的标记都会命中，定案前请按偏移人工确认。"
        );
    }
    payload
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file(tag: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prism-bin-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.bin");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(bytes).unwrap();
        path
    }

    fn context<'a>(directory: &'a str) -> ToolContext<'a> {
        // 开关集合要活得比 ctx 长：放静态里，不能引用函数栈上的临时值。
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

    #[test]
    fn identifies_elf64() {
        let mut bytes = vec![0u8; 64];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 2; // 64 位
        bytes[5] = 1; // 小端
        bytes[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
        bytes[18..20].copy_from_slice(&62u16.to_le_bytes()); // x86_64
        bytes[24..32].copy_from_slice(&0x401000u64.to_le_bytes());
        bytes[60..62].copy_from_slice(&5u16.to_le_bytes()); // shnum
        bytes[56..58].copy_from_slice(&2u16.to_le_bytes()); // phnum

        let info = identify(&bytes, 64);
        assert_eq!(info["format"], "elf");
        assert_eq!(info["arch"], "x86_64");
        assert_eq!(info["bits"], json!(64));
        assert_eq!(info["elfType"], "exec");
        assert_eq!(info["entry"], "0x401000");
        assert_eq!(info["sections"], json!(5));
        assert_eq!(info["segments"], json!(2));
    }

    #[test]
    fn identifies_pe64() {
        let mut bytes = vec![0u8; 0x100];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
        bytes[0x84..0x86].copy_from_slice(&0x8664u16.to_le_bytes());
        bytes[0x86..0x88].copy_from_slice(&3u16.to_le_bytes());
        bytes[0x88..0x8c].copy_from_slice(&0x6500_0000u32.to_le_bytes());
        bytes[0x98..0x9a].copy_from_slice(&0x20bu16.to_le_bytes());
        bytes[0xa8..0xac].copy_from_slice(&0x1000u32.to_le_bytes());
        bytes[0xdc..0xde].copy_from_slice(&3u16.to_le_bytes());

        let info = identify(&bytes, 0x100);
        assert_eq!(info["format"], "pe");
        assert_eq!(info["arch"], "x86_64");
        assert_eq!(info["peKind"], "pe32+");
        assert_eq!(info["subsystem"], "windows-console");
        assert_eq!(info["sections"], json!(3));
        assert!(info["compileTime"].as_str().unwrap().starts_with("2023-09"));
    }

    #[test]
    fn identifies_macho_and_apk() {
        let mut macho = vec![0u8; 32];
        macho[..4].copy_from_slice(&0xfeed_facfu32.to_le_bytes());
        macho[4..8].copy_from_slice(&0x0100_000cu32.to_le_bytes());
        macho[12..16].copy_from_slice(&2u32.to_le_bytes());
        let info = identify(&macho, 32);
        assert_eq!(info["format"], "macho");
        assert_eq!(info["arch"], "arm64");
        assert_eq!(info["machoType"], "execute");

        let mut apk = vec![0u8; 512];
        apk[..4].copy_from_slice(b"PK\x03\x04");
        apk[64..83].copy_from_slice(b"AndroidManifest.xml");
        assert_eq!(identify(&apk, 512)["format"].as_str(), Some("apk"));
    }

    #[test]
    fn hashes_match_known_vectors() {
        let path = temp_file("hash", b"abc");
        let directory = path.parent().unwrap().display().to_string();
        let result = run_hash(
            &context(&directory),
            &json!({ "path": "sample.bin" }),
        );
        let payload: Value = serde_json::from_str(&result.outcome.text).unwrap();
        assert_eq!(payload["md5"].as_str().unwrap(), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(payload["sha1"].as_str().unwrap(), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            payload["sha256"].as_str().unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(payload["size"].as_u64(), Some(3));
    }

    #[test]
    fn strings_are_extracted_deduped_and_tagged() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"\x00\x01GET /a HTTP/1.1\x00");
        // 同一条 URL 出现两次：验证按内容去重、命中次数累加。
        bytes.extend_from_slice(b"\x00https://evil.example.com/x\x00");
        bytes.extend_from_slice(b"\x00https://evil.example.com/x\x00");
        bytes.extend_from_slice("C:\\Windows\\Temp\\a.exe".as_bytes());
        bytes.extend_from_slice(&[0, 0]);
        for ch in "wide-string".encode_utf16() {
            bytes.extend_from_slice(&ch.to_le_bytes());
        }
        bytes.push(0);

        let (items, total) = collect_strings(&bytes, 4, 100, true, true);
        let texts: Vec<&str> = items
            .iter()
            .map(|item| item["text"].as_str().unwrap())
            .collect();
        assert!(texts.iter().any(|text| text.contains("https://evil.example.com")));
        assert!(texts.iter().any(|text| text.contains("C:\\Windows\\Temp")));
        assert!(texts.iter().any(|text| *text == "wide-string"));
        // 重复的 URL 只留一条，但命中次数记 2。
        let url = items
            .iter()
            .find(|item| item["text"].as_str().unwrap().contains("https://evil"))
            .unwrap();
        assert_eq!(url["hits"].as_u64(), Some(2));
        assert!(url["tags"].as_array().unwrap().iter().any(|tag| tag.as_str() == Some("url")));
        assert_eq!(total, items.len());
    }

    #[test]
    fn tags_cover_the_common_cases() {
        assert_eq!(classify("http://a.test/x"), vec!["url"]);
        assert!(classify("8.8.8.8").contains(&"ipv4"));
        assert!(classify("evil.example.cn").contains(&"domain"));
        assert!(classify("powershell -enc AAAA").contains(&"command"));
        assert!(classify("C:\\Users\\a").contains(&"path"));
        assert!(classify("-----BEGIN PRIVATE KEY-----").contains(&"key"));
        assert!(classify("ZZZZZZ").is_empty());
    }

    #[test]
    fn rejects_paths_outside_the_workspace() {
        let path = temp_file("escape", b"x");
        let directory = path.parent().unwrap().display().to_string();
        let result = run_meta(&context(&directory), &json!({ "path": "../../etc/passwd" }));
        assert!(result.outcome.error.is_some());
    }

    #[test]
    fn identifies_the_running_binary() {
        // 真文件：本测试进程自己的可执行文件。
        let exe = std::env::current_exe().unwrap();
        let bytes = read_head(&exe, META_HEAD).unwrap();
        let size = std::fs::metadata(&exe).unwrap().len();
        let info = identify(&bytes, size);
        assert!(
            matches!(info["format"].as_str(), Some("macho") | Some("elf") | Some("pe")),
            "{info}"
        );
        assert!(info["bits"].as_u64().is_some(), "{info}");
    }

    // ------------------------------------------------------------ 壳识别

    struct Spec {
        name: &'static str,
        address: u32,
        virtual_size: u32,
        raw_size: u32,
        raw_offset: u32,
        characteristics: u32,
    }

    /// 造一个 `pe_facts` 能解析的 PE32。
    /// 节表固定落在 0x178（SizeOfOptionalHeader = 0xE0），和真实布局一致。
    fn build_pe(sections: &[Spec], entry: u32, import_rva: u32, total: usize) -> Vec<u8> {
        let mut bytes = vec![0u8; total.max(0x400)];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
        bytes[0x84..0x86].copy_from_slice(&0x014cu16.to_le_bytes());
        bytes[0x86..0x88].copy_from_slice(&(sections.len() as u16).to_le_bytes());
        bytes[0x94..0x96].copy_from_slice(&0xe0u16.to_le_bytes());
        bytes[0x98..0x9a].copy_from_slice(&0x010bu16.to_le_bytes());
        bytes[0xa8..0xac].copy_from_slice(&entry.to_le_bytes());
        bytes[0x100..0x104].copy_from_slice(&import_rva.to_le_bytes());
        for (index, spec) in sections.iter().enumerate() {
            let at = 0x178 + index * 40;
            bytes[at..at + spec.name.len()].copy_from_slice(spec.name.as_bytes());
            bytes[at + 8..at + 12].copy_from_slice(&spec.virtual_size.to_le_bytes());
            bytes[at + 12..at + 16].copy_from_slice(&spec.address.to_le_bytes());
            bytes[at + 16..at + 20].copy_from_slice(&spec.raw_size.to_le_bytes());
            bytes[at + 20..at + 24].copy_from_slice(&spec.raw_offset.to_le_bytes());
            bytes[at + 36..at + 40].copy_from_slice(&spec.characteristics.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn pack_confirms_upx_by_section_names() {
        let bytes = build_pe(
            &[
                Spec {
                    name: "UPX0",
                    address: 0x1000,
                    virtual_size: 0x1_0000,
                    raw_size: 0,
                    raw_offset: 0,
                    characteristics: 0xe000_00c0,
                },
                Spec {
                    name: "UPX1",
                    address: 0x1_1000,
                    virtual_size: 0x2000,
                    raw_size: 0x800,
                    raw_offset: 0x400,
                    characteristics: 0xe000_00c0,
                },
            ],
            0x1_1100,
            0,
            0x1000,
        );
        let payload = detect_pack(&bytes, 0x1000);
        assert_eq!(payload["packed"], json!(true), "{payload}");
        assert_eq!(payload["confidence"], "certain");
        assert_eq!(payload["packers"][0]["name"], "UPX");
        assert_eq!(payload["signals"]["importTable"], json!(false));
        assert_eq!(payload["signals"]["entrySection"], "UPX1");
        assert_eq!(payload["signals"]["entryInWritableExecutable"], json!(true));
        assert_eq!(
            payload["signals"]["virtualSizeOutweighsRaw"],
            json!(["UPX0"])
        );
        let evidence = payload["packers"][0]["evidence"].as_array().unwrap();
        assert!(
            evidence
                .iter()
                .any(|item| item.as_str().unwrap().contains("UPX0")),
            "{payload}"
        );
        // 脱壳方向要落到具体动作上，不能只说"建议脱壳"。
        assert!(payload["advice"][1].as_str().unwrap().contains("upx -d"));
    }

    #[test]
    fn pack_clears_a_normal_pe() {
        let bytes = build_pe(
            &[Spec {
                name: ".text",
                address: 0x1000,
                virtual_size: 0x1000,
                raw_size: 0x800,
                raw_offset: 0x400,
                characteristics: 0x6000_0020,
            }],
            0x1000,
            0x2000,
            0x1000,
        );
        let payload = detect_pack(&bytes, 0x1000);
        assert_eq!(payload["packed"], json!(false), "{payload}");
        assert_eq!(payload["confidence"], "none");
        assert_eq!(payload["packers"], json!([]));
        assert_eq!(payload["verdict"], "未发现加壳特征");
    }

    #[test]
    fn pack_finds_stub_strings_without_section_names() {
        let mut bytes = build_pe(
            &[Spec {
                name: ".rdata",
                address: 0x1000,
                virtual_size: 0x1000,
                raw_size: 0x800,
                raw_offset: 0x400,
                characteristics: 0x4000_0040,
            }],
            0x1000,
            0x2000,
            0x1000,
        );
        bytes[0x600..0x60a].copy_from_slice(b"VMProtect\0");
        let payload = detect_pack(&bytes, 0x1000);
        assert_eq!(payload["packers"][0]["name"], "VMProtect", "{payload}");
        // 只有特征串，没有专有节名：最高判到 high。
        assert_eq!(payload["confidence"], "high");
        assert!(payload["note"].as_str().unwrap().contains("人工确认"));
        assert!(
            payload["packers"][0]["evidence"][0]
                .as_str()
                .unwrap()
                .contains("0x600"),
            "{payload}"
        );
        // 命中落在哪个节要写清楚：`.rdata` 里的字符串和入口节里的字符串不是一回事。
        assert!(
            payload["packers"][0]["evidence"][0]
                .as_str()
                .unwrap()
                .contains(".rdata"),
            "{payload}"
        );
    }

    /// 宽字符特征必须单独走一条路：ASCII 扫描看不见 `C\0o\0n\0f\0...`。
    #[test]
    fn pack_matches_wide_character_markers() {
        let mut bytes = build_pe(
            &[Spec {
                name: ".text",
                address: 0x1000,
                virtual_size: 0x1000,
                raw_size: 0x800,
                raw_offset: 0x400,
                characteristics: 0x6000_0020,
            }],
            0x1000,
            0x2000,
            0x1000,
        );
        for (index, byte) in b"ConfuserEx".iter().enumerate() {
            bytes[0x500 + index * 2] = *byte;
        }
        let payload = detect_pack(&bytes, 0x1000);
        assert_eq!(payload["packers"][0]["name"], "ConfuserEx", "{payload}");
        assert!(
            payload["packers"][0]["evidence"][0]
                .as_str()
                .unwrap()
                .contains("宽字符"),
            "{payload}"
        );
    }

    #[test]
    fn pack_flags_structure_without_a_known_name() {
        // 可写可执行 + 导入表为空 + 内容接近随机，但节名被抹掉。
        let mut bytes = build_pe(
            &[Spec {
                name: "seg0",
                address: 0x1000,
                virtual_size: 0x1000,
                raw_size: 0x800,
                raw_offset: 0x400,
                characteristics: 0xe000_0020,
            }],
            0x1000,
            0,
            0x1000,
        );
        let mut state = 0x1234_5678u32;
        for chunk in bytes[0x400..0xc00].iter_mut() {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *chunk = (state >> 24) as u8;
        }
        let payload = detect_pack(&bytes, 0x1000);
        assert_eq!(payload["packers"], json!([]));
        assert_eq!(payload["packed"], json!(true), "{payload}");
        assert_eq!(payload["confidence"], "low");
        let section_entropy = payload["signals"]["sections"][0]["entropy"].as_f64().unwrap();
        assert!(section_entropy > 7.0, "节内熵应接近 8：{payload}");
        assert!(payload["verdict"].as_str().unwrap().contains("结构异常"));
        assert_eq!(
            payload["signals"]["writableExecutableSections"],
            json!(["seg0"])
        );
    }

    /// 共用节名单独出现时只能算 low：`.adata` 同时被 ASProtect 与 ASPack 使用。
    #[test]
    fn shared_section_name_alone_is_only_a_weak_hit() {
        let bytes = build_pe(
            &[Spec {
                name: ".adata",
                address: 0x1000,
                virtual_size: 0x1000,
                raw_size: 0x800,
                raw_offset: 0x400,
                characteristics: 0x4000_0040,
            }],
            0x1000,
            0x2000,
            // 文件正好收在节尾，不留 overlay：这条用例只想看弱特征会不会被结构信号抬高。
            0xc00,
        );
        let payload = detect_pack(&bytes, 0xc00);
        assert_eq!(payload["packers"][0]["name"], "ASProtect", "{payload}");
        assert_eq!(payload["packers"][0]["confidence"], "low");
        // low 不足以单独定案：结构信号全干净，就不算加壳。
        assert_eq!(payload["packed"], json!(false));
        assert!(payload["verdict"].as_str().unwrap().contains("需要人工确认"));
    }

    /// 真文件回归：系统自带的 /bin/ls 不该被判成加壳。
    /// 默认忽略（不是每个环境都有这个路径），手动跑：
    /// `cargo test --lib -- --ignored pack_clears_a_real_executable`.
    #[test]
    #[ignore]
    fn pack_clears_a_real_executable() {
        let path = Path::new("/bin/ls");
        if !path.exists() {
            return;
        }
        let size = std::fs::metadata(path).unwrap().len();
        let bytes = read_head(path, PACK_SCAN).unwrap();
        let payload = detect_pack(&bytes, size);
        assert_eq!(payload["packed"], json!(false), "{payload}");
        assert_eq!(payload["confidence"], "none", "{payload}");
    }

    #[test]
    fn pack_reports_trailing_overlay() {
        let bytes = build_pe(
            &[Spec {
                name: ".text",
                address: 0x1000,
                virtual_size: 0x1000,
                raw_size: 0x200,
                raw_offset: 0x400,
                characteristics: 0x6000_0020,
            }],
            0x1000,
            0x2000,
            0x1000,
        );
        let payload = detect_pack(&bytes, 0x1000);
        // 最后一个节收在 0x600，后面 0xA00 字节是附加数据。
        assert_eq!(payload["signals"]["overlayBytes"], json!(0xa00), "{payload}");
    }

    #[test]
    fn entropy_reaches_both_ends() {
        assert_eq!(entropy(&[]), 0.0);
        assert_eq!(entropy(&[7u8; 4096]), 0.0);
        let spread: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
        assert!((entropy(&spread) - 8.0).abs() < 0.001);
    }

    #[test]
    fn pack_tool_is_registered_read_only() {
        let tool = tools()
            .into_iter()
            .find(|item| item.name == "bin.pack.detect")
            .expect("应注册 bin.pack.detect");
        assert_eq!(tool.meta.risk, Risk::Read);
        assert_eq!(tool.permission, Permission::ReadOnly);
        assert_eq!(tool.meta.network, Network::None);
        assert_eq!(tool.meta.scope, ToolScope::Any);
        assert_eq!(tool.meta.tags, vec!["bin:static"]);
    }

    #[test]
    fn pack_tool_reads_from_disk() {
        let bytes = build_pe(
            &[Spec {
                name: "UPX0",
                address: 0x1000,
                virtual_size: 0x1_0000,
                raw_size: 0,
                raw_offset: 0,
                characteristics: 0xe000_00c0,
            }],
            0x1000,
            0,
            0x1000,
        );
        let path = temp_file("pack", &bytes);
        let directory = path.parent().unwrap().display().to_string();
        let result = run_pack(
            &context(&directory),
            &json!({ "path": "sample.bin", "deep": true }),
        );
        assert!(result.outcome.error.is_none(), "{}", result.outcome.text);
        assert!(result.outcome.text.contains("\"packed\": true"));
        assert!(result.outcome.text.contains("\"scannedBytes\""));
    }

    // ------------------------------------------------------------ APK 壳识别

    /// 造一个能被 `apk_entries` 解析的最小 APK（zip）。
    /// 只写中央目录与 EOCD，不写本地文件头——壳检测只读中央目录的文件名清单。
    /// `entries` 是 `(文件名, 未压缩大小)`。
    fn build_apk_zip(entries: &[(&str, u64)]) -> Vec<u8> {
        // 文件头伪造出 `PK\x03\x04`，让 `identify` 能认成 apk（配合 AndroidManifest.xml 条目名）。
        let mut bytes = vec![0u8; 512];
        bytes[..4].copy_from_slice(b"PK\x03\x04");
        // 在头部窗口里塞一个 AndroidManifest.xml 字样，让 identify 判定为 apk 而非 zip。
        bytes[64..83].copy_from_slice(b"AndroidManifest.xml");

        // 中央目录从 512 之后开始。
        let mut cd = Vec::new();
        let mut local = 0u32; // 本地头偏移，壳检测不读，填 0 即可。
        for (name, size) in entries {
            let name_bytes = name.as_bytes();
            // 中央目录文件头：PK\x01\x02 + 46 字节结构。
            cd.extend_from_slice(b"PK\x01\x02");
            cd.extend_from_slice(&20u16.to_le_bytes()); // version made by
            cd.extend_from_slice(&20u16.to_le_bytes()); // version needed
            cd.extend_from_slice(&0u16.to_le_bytes()); // flags
            cd.extend_from_slice(&0u16.to_le_bytes()); // method (store)
            cd.extend_from_slice(&0u16.to_le_bytes()); // time
            cd.extend_from_slice(&0u16.to_le_bytes()); // date
            cd.extend_from_slice(&0u32.to_le_bytes()); // crc32
            cd.extend_from_slice(&(*size as u32).to_le_bytes()); // compressed size
            cd.extend_from_slice(&(*size as u32).to_le_bytes()); // uncompressed size
            cd.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes()); // name len
            cd.extend_from_slice(&0u16.to_le_bytes()); // extra len
            cd.extend_from_slice(&0u16.to_le_bytes()); // comment len
            cd.extend_from_slice(&0u16.to_le_bytes()); // disk number
            cd.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            cd.extend_from_slice(&0u32.to_le_bytes()); // external attrs
            cd.extend_from_slice(&local.to_le_bytes()); // local header offset
            cd.extend_from_slice(name_bytes);
            local += 1; // 占位推进，实际检测不读
        }
        let cd_offset = bytes.len() as u32;
        let cd_size = cd.len() as u32;
        bytes.extend_from_slice(&cd);

        // EOCD。
        let eocd_at = bytes.len();
        bytes.extend_from_slice(b"PK\x05\x06");
        bytes.extend_from_slice(&0u16.to_le_bytes()); // disk number
        bytes.extend_from_slice(&0u16.to_le_bytes()); // cd start disk
        bytes.extend_from_slice(&(entries.len() as u16).to_le_bytes()); // entries on disk
        bytes.extend_from_slice(&(entries.len() as u16).to_le_bytes()); // total entries
        bytes.extend_from_slice(&cd_size.to_le_bytes()); // cd size
        bytes.extend_from_slice(&cd_offset.to_le_bytes()); // cd offset
        bytes.extend_from_slice(&0u16.to_le_bytes()); // comment len
        debug_assert_eq!(eocd_at, cd_offset as usize + cd_size as usize);
        bytes
    }

    #[test]
    fn apk_entries_parses_central_directory() {
        let bytes = build_apk_zip(&[
            ("AndroidManifest.xml", 1024),
            ("classes.dex", 4096),
            ("lib/arm64-v8a/libjiagu.so", 2_000_000),
        ]);
        let entries = apk_entries(&bytes).unwrap();
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["AndroidManifest.xml", "classes.dex", "lib/arm64-v8a/libjiagu.so"]
        );
        assert_eq!(entries[2].uncompressed_size, 2_000_000);
    }

    #[test]
    fn apk_pack_detects_360_by_so_name() {
        let bytes = build_apk_zip(&[
            ("AndroidManifest.xml", 1024),
            ("classes.dex", 2048),
            ("lib/arm64-v8a/libjiagu.so", 2_000_000),
        ]);
        let payload = detect_apk_pack(&bytes, bytes.len() as u64);
        assert_eq!(payload["format"], "apk", "{payload}");
        assert_eq!(payload["packed"], json!(true), "{payload}");
        assert_eq!(payload["packers"][0]["name"], "360 加固", "{payload}");
        assert_eq!(payload["packers"][0]["confidence"], "high", "{payload}");
        assert_eq!(payload["signals"]["nativeLibCount"], json!(1));
    }

    #[test]
    fn apk_pack_flags_dex_stub() {
        // classes.dex 只有 4 KiB（stub），没有已知壳 so：走结构信号判私有/抽取壳。
        let bytes = build_apk_zip(&[
            ("AndroidManifest.xml", 1024),
            ("classes.dex", 4096),
            ("lib/arm64-v8a/libcustom.so", 1_500_000),
        ]);
        let payload = detect_apk_pack(&bytes, bytes.len() as u64);
        assert_eq!(payload["packed"], json!(true), "{payload}");
        assert_eq!(payload["signals"]["dexIsStub"], json!(true));
        assert!(payload["verdict"].as_str().unwrap().contains("结构信号"), "{payload}");
    }

    #[test]
    fn apk_pack_clears_a_normal_apk() {
        let bytes = build_apk_zip(&[
            ("AndroidManifest.xml", 4096),
            ("classes.dex", 3_000_000),
            ("classes2.dex", 1_000_000),
            ("lib/arm64-v8a/libnative.so", 200_000),
            ("resources.arsc", 50_000),
        ]);
        let payload = detect_apk_pack(&bytes, bytes.len() as u64);
        assert_eq!(payload["packed"], json!(false), "{payload}");
        assert_eq!(payload["confidence"], "none");
        assert_eq!(payload["verdict"], "未发现加壳特征");
        // 正常 app 的 classes.dex 不小，dexIsStub 应为 false。
        assert_eq!(payload["signals"]["dexIsStub"], json!(false));
    }

    /// 真文件回归：真实 APK（未加固的 aerial-guard）不该被判成加固。
    /// 手动跑：`cargo test --lib -- --ignored apk_pack_clears_a_real_apk`。
    #[test]
    #[ignore]
    fn apk_pack_clears_a_real_apk() {
        let path = Path::new("/Users/edwa2d4th/Desktop/未命名文件夹 7/aerial-guard.apk");
        if !path.exists() {
            return;
        }
        let size = std::fs::metadata(path).unwrap().len();
        let bytes = read_head(path, PACK_SCAN).unwrap();
        let payload = detect_pack(&bytes, size);
        assert_eq!(payload["format"], "apk", "{payload}");
        // aerial-guard 是普通 TV 管家，不应命中任何加固壳名。
        assert_eq!(payload["packers"], json!([]), "{payload}");
    }
}
