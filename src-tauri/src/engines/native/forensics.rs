//! 安全研究线的本机工具：`bin.meta.identify` / `bin.hash.compute` / `bin.strings.extract`。
//!
//! 三个工具都只读文件、都不联网、都不需要外部依赖——装好就能用。
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
        builtin_full(
            "bin.meta.identify",
            "样本格式识别",
            meta_schema,
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
            run_meta,
        ),
        builtin_full(
            "bin.hash.compute",
            "三哈希",
            hash_schema,
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
            run_hash,
        ),
        builtin_full(
            "bin.strings.extract",
            "字符串提取",
            strings_schema,
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
            run_strings,
        ),
    ]
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
    if head.windows(4).any(|chunk| chunk == b"UPX!") {
        out["packerHint"] = json!("upx");
    }
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
}
