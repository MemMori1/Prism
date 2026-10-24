//! 反汇编 IR（`ProgramIndex`）：把 Ghidra / radare2 / DEX 三种来源映射成同一形状。
//!
//! 下游（`cfg.extract` / `hash_identify` / `vm.detect`）只认这一份结构，不认
//! 各家后端的 JSON。这样换后端不用改下游，也不会出现"Ghidra 有 blocks、
//! radare2 没有"两套分支散在各处。
//!
//! 三条规矩：
//!
//! 1. **`va` 是不透明 id**：DEX 没有虚拟地址，用 `code_off` 顶替。下游只能
//!    比较相等、画图，不能假设可重定位；
//! 2. **缓存键是 `sha256 + backend + backend_version`**：三者任一变化就重分析。
//!    机器上换了 radare2 版本，旧 IR 里的函数边界不能当事实继续用；
//! 3. **截断要标**：函数数超过 [`MAX_FUNCTIONS`] 时只保留前 N 个并置
//!    `truncated=true`，不假装"全都在"。
//!
//! 磁盘布局见 `docs/BIN_TOOLCHAIN.md` §3.5：
//! `~/.prism/cache/programs/<sha256>/index.json`。

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// IR 版本。字段语义变了就加一，旧缓存自然失效。
pub const SCHEMA: u32 = 1;

/// 单个样本最多保留多少个函数。
///
/// `aflj` 给 20001 个函数不是"分析得更全"，是模型上下文和 JSON 都撑不住。
/// 超了标 `truncated`，让人知道结论只覆盖前 N 个。
pub const MAX_FUNCTIONS: usize = 20_000;

/// 导入表上限：几千条已经足够定位 API，再多只会把上下文撑爆。
pub const MAX_IMPORTS: usize = 5_000;

/// 缓存总上限：8 GiB（Ghidra 项目目录也在这个额度里）。
pub const CACHE_LIMIT: u64 = 8 * 1024 * 1024 * 1024;

/// 一份样本的反汇编索引。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgramIndex {
    pub schema: u32,
    /// `ghidra` / `radare2` / `dex`。
    pub backend: String,
    /// 后端版本；DEX 内置解析器用 `builtin-<应用版本>`。
    pub backend_version: String,
    pub sample: SampleRef,
    #[serde(default)]
    pub functions: Vec<FunctionEntry>,
    /// 后端顺手给就带，不强制（`aflj` 不给导入表）。
    #[serde(default)]
    pub imports: Vec<ImportEntry>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// 被分析的样本（缓存命中时也要能核对"是不是同一份"）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SampleRef {
    pub sha256: String,
    pub size: u64,
    pub format: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bits: Option<u32>,
    /// 映像基址；DEX 与 `aflj` 不提供时为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_base: Option<u64>,
}

/// 一个函数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionEntry {
    pub va: u64,
    pub name: String,
    pub size: u32,
    /// `normal` / `thunk` / `import` / `plt` / `entry`。
    pub kind: String,
    /// 按需 detail pass 才有；`aflj` 这一段不产出基本块。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks: Option<Vec<BlockEntry>>,
}

/// 一个基本块。`succ` 是直接后继（fallthrough + jump），已换算成 `va`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockEntry {
    pub va: u64,
    pub size: u32,
    #[serde(default)]
    pub succ: Vec<u64>,
}

/// 一个外部符号（导入）。现在只有 Ghidra / DEX 会填。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportEntry {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library: Option<String>,
}

// ---------------------------------------------------------------- aflj 映射

/// `aflj` 里我们真正用的字段。
///
/// r2 的 JSON 还带 `nbbs` / `edges` / `callrefs` 等几十个字段，这里不声明就会
/// 被 serde 忽略——IR 只需要边界，别的等 detail pass 再说。
#[derive(Debug, Deserialize)]
struct AfljFunction {
    #[serde(default)]
    offset: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    size: u64,
    #[serde(default, rename = "type")]
    kind: String,
}

/// 从 `r2 -c 'aaa; aflj'` 的 stdout 里抠出 JSON 数组。
///
/// r2 有时会在 JSON 前后打警告；先找第一个 `[`、最后一个 `]`，再交给 serde。
/// 解析失败就是失败，不做"猜着修 JSON"。
pub fn json_slice(text: &str) -> Option<&str> {
    let start = text.find('[')?;
    let end = text.rfind(']')?;
    if end < start {
        return None;
    }
    Some(&text[start..=end])
}

/// 把 `aflj` 的 JSON 映射成 IR。
pub fn from_aflj(
    text: &str,
    sample: SampleRef,
    backend_version: &str,
) -> Result<ProgramIndex, String> {
    let slice = json_slice(text).ok_or_else(|| "输出里没有 JSON 数组".to_string())?;
    let entries: Vec<AfljFunction> =
        serde_json::from_str(slice).map_err(|error| format!("JSON 解析失败：{error}"))?;

    let truncated = entries.len() > MAX_FUNCTIONS;
    let mut functions = Vec::with_capacity(entries.len().min(MAX_FUNCTIONS));
    for entry in entries.into_iter().take(MAX_FUNCTIONS) {
        let name = if entry.name.trim().is_empty() {
            format!("sub_{:x}", entry.offset)
        } else {
            entry.name
        };
        functions.push(FunctionEntry {
            va: entry.offset,
            kind: kind_of(&entry.kind, &name).to_string(),
            name,
            size: entry.size.min(u32::MAX as u64) as u32,
            blocks: None,
        });
    }

    let note = if truncated {
        Some(format!(
            "函数数超过 {MAX_FUNCTIONS}，只保留前 {MAX_FUNCTIONS} 个（aflj 原始顺序）。"
        ))
    } else if functions.is_empty() {
        Some("radare2 没有识别出任何函数：可能是纯数据文件，或架构不在 aaa 的支持范围里。".to_string())
    } else {
        None
    };

    Ok(ProgramIndex {
        schema: SCHEMA,
        backend: "radare2".to_string(),
        backend_version: backend_version.to_string(),
        sample,
        functions,
        imports: Vec::new(),
        truncated,
        note,
    })
}

/// r2 的 `type` → IR 的 `kind`。
fn kind_of(raw: &str, name: &str) -> &'static str {
    match raw {
        "plt" => "plt",
        "import" | "imp" => "import",
        "thunk" => "thunk",
        _ if name.starts_with("entry") => "entry",
        _ => "normal",
    }
}

// ---------------------------------------------------------------- Ghidra 映射

/// `PrismExport.java` 的输出形状。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhidraExport {
    #[serde(default)]
    functions: Vec<GhidraFunction>,
    #[serde(default)]
    imports: Vec<GhidraImport>,
    #[serde(default)]
    truncated: bool,
    #[serde(default)]
    image_base: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhidraFunction {
    va: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    blocks: Vec<GhidraBlock>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhidraBlock {
    va: u64,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    succ: Vec<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhidraImport {
    #[serde(default)]
    name: String,
    #[serde(default)]
    library: Option<String>,
}

/// 把 `PrismExport.java` 的 JSON 映射成 IR。
pub fn from_ghidra(
    text: &str,
    mut sample: SampleRef,
    backend_version: &str,
) -> Result<ProgramIndex, String> {
    let mut export: GhidraExport =
        serde_json::from_str(text).map_err(|error| format!("JSON 解析失败：{error}"))?;

    if sample.image_base.is_none() {
        sample.image_base = export.image_base;
    }
    let over_limit = export.functions.len() > MAX_FUNCTIONS;
    let imports_over = export.imports.len() > MAX_IMPORTS;
    let truncated = export.truncated || over_limit || imports_over;
    let mut functions = Vec::with_capacity(export.functions.len().min(MAX_FUNCTIONS));
    for function in export.functions.drain(..).take(MAX_FUNCTIONS) {
        let blocks = if function.blocks.is_empty() {
            None
        } else {
            Some(
                function
                    .blocks
                    .into_iter()
                    .map(|block| BlockEntry {
                        va: block.va,
                        size: block.size.min(u32::MAX as u64) as u32,
                        succ: block.succ,
                    })
                    .collect(),
            )
        };
        functions.push(FunctionEntry {
            va: function.va,
            name: if function.name.trim().is_empty() {
                format!("sub_{:x}", function.va)
            } else {
                function.name
            },
            size: function.size.min(u32::MAX as u64) as u32,
            kind: if function.kind.trim().is_empty() {
                "normal".to_string()
            } else {
                function.kind
            },
            blocks,
        });
    }

    let mut imports: Vec<ImportEntry> = export
        .imports
        .into_iter()
        .filter(|item| !item.name.trim().is_empty())
        .take(MAX_IMPORTS)
        .map(|item| ImportEntry {
            name: item.name,
            library: item.library,
        })
        .collect();
    imports.shrink_to_fit();

    let note = if truncated {
        Some(format!(
            "Ghidra 导出被截断：最多保留 {MAX_FUNCTIONS} 个函数 / {MAX_IMPORTS} 个导入。"
        ))
    } else {
        None
    };

    Ok(ProgramIndex {
        schema: SCHEMA,
        backend: "ghidra".to_string(),
        backend_version: backend_version.to_string(),
        sample,
        functions,
        imports,
        truncated,
        note,
    })
}

// ---------------------------------------------------------------- DEX 映射

/// DEX 内置解析器的版本号（跟着应用版本走，升级后旧缓存失效）。
///
/// P2.4 的 `cfg.extract` 接进来后，这里的 `allow` 就能删掉。
#[cfg_attr(not(test), allow(dead_code))]
pub fn dex_backend_version() -> String {
    format!("builtin-{}", env!("CARGO_PKG_VERSION"))
}

/// 从已经解析好的方法列表构造 IR（单测与 `cfg.extract` 复用）。
#[cfg_attr(not(test), allow(dead_code))]
pub fn from_dex_methods(
    methods: Vec<crate::engines::native::dex::DexMethod>,
    sample: SampleRef,
    skipped_classes: usize,
) -> ProgramIndex {
    let truncated = methods.len() > MAX_FUNCTIONS;
    let functions: Vec<FunctionEntry> = methods
        .into_iter()
        .take(MAX_FUNCTIONS)
        .map(|method| FunctionEntry {
            va: method.va,
            name: method.name,
            size: method.size,
            kind: "normal".to_string(),
            blocks: None,
        })
        .collect();

    let mut note = String::from(
        "DEX 没有虚拟地址：va 是 code_off，只作不透明 id 用；blocks 未生成（基本块留给 detail pass）。",
    );
    if truncated {
        note.push_str(&format!(
            " 函数数超过 {MAX_FUNCTIONS}，只保留前 {MAX_FUNCTIONS} 个。"
        ));
    }
    if skipped_classes > 0 {
        note.push_str(&format!(
            " {skipped_classes} 个类的 class_data 解析失败，已跳过。"
        ));
    }

    ProgramIndex {
        schema: SCHEMA,
        backend: "dex".to_string(),
        backend_version: dex_backend_version(),
        sample,
        functions,
        imports: Vec::new(),
        truncated,
        note: Some(note),
    }
}

// ---------------------------------------------------------------- 缓存

/// `~/.prism/cache/programs`。
pub fn cache_root() -> Option<PathBuf> {
    crate::engines::opencode::search::prism_dir().map(|dir| dir.join("cache/programs"))
}

/// 缓存目录名就是 sha256：必须是 64 位十六进制，防止有人拿 `../` 当键。
fn valid_sha(sha: &str) -> bool {
    sha.len() == 64 && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// 读缓存。键不匹配（后端 / 版本 / schema / sha）一律当没有。
pub fn load(sha256: &str, backend: &str, backend_version: &str) -> Option<ProgramIndex> {
    load_at(&cache_root()?, sha256, backend, backend_version)
}

/// 同 [`load`]，根目录可指定（单测用）。
pub fn load_at(
    root: &Path,
    sha256: &str,
    backend: &str,
    backend_version: &str,
) -> Option<ProgramIndex> {
    if !valid_sha(sha256) {
        return None;
    }
    let text = std::fs::read_to_string(root.join(sha256).join("index.json")).ok()?;
    let index: ProgramIndex = serde_json::from_str(&text).ok()?;
    if index.schema != SCHEMA
        || index.backend != backend
        || index.backend_version != backend_version
        || index.sample.sha256 != sha256
    {
        return None;
    }
    Some(index)
}

/// 写缓存（临时文件 + rename，进程被杀不会留下半个 JSON）。
pub fn save(index: &ProgramIndex) -> Result<PathBuf, String> {
    let root = cache_root().ok_or_else(|| "找不到 ~/.prism 目录".to_string())?;
    save_at(&root, index)
}

/// 同 [`save`]，根目录可指定（单测用）。
pub fn save_at(root: &Path, index: &ProgramIndex) -> Result<PathBuf, String> {
    let sha = index.sample.sha256.as_str();
    if !valid_sha(sha) {
        return Err(format!("样本哈希不像 sha256：{sha}"));
    }
    let dir = root.join(sha);
    std::fs::create_dir_all(&dir).map_err(|error| format!("建缓存目录失败：{error}"))?;
    let text =
        serde_json::to_string(index).map_err(|error| format!("序列化 IR 失败：{error}"))?;
    let path = dir.join("index.json");
    let tmp = dir.join("index.json.tmp");
    std::fs::write(&tmp, text.as_bytes()).map_err(|error| format!("写缓存失败：{error}"))?;
    std::fs::rename(&tmp, &path).map_err(|error| format!("替换缓存失败：{error}"))?;
    Ok(path)
}

/// 后端输出解析失败时把原文留下来（`functions_raw.txt`）。
pub fn write_raw(sha256: &str, text: &str) -> Result<PathBuf, String> {
    if !valid_sha(sha256) {
        return Err(format!("样本哈希不像 sha256：{sha256}"));
    }
    let root = cache_root().ok_or_else(|| "找不到 ~/.prism 目录".to_string())?;
    let dir = root.join(sha256);
    std::fs::create_dir_all(&dir).map_err(|error| format!("建缓存目录失败：{error}"))?;
    let path = dir.join("functions_raw.txt");
    std::fs::write(&path, text.as_bytes()).map_err(|error| format!("写原文失败：{error}"))?;
    Ok(path)
}

/// 淘汰结果（设置页/日志用得上，单测也靠它断言）。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PruneStats {
    pub removed: usize,
    pub freed: u64,
    pub kept_bytes: u64,
}

/// 把缓存压到 `limit` 字节以内，按 `index.json` 的 mtime 从旧到新删。
///
/// 失败一律忽略：清理是维护动作，不能因为它失败就把这次分析搞挂。
pub fn prune_cache(limit: u64) -> PruneStats {
    match cache_root() {
        Some(root) => prune_cache_at(&root, limit),
        None => PruneStats::default(),
    }
}

/// 同 [`prune_cache`]，根目录可指定（单测用）。
pub fn prune_cache_at(root: &Path, limit: u64) -> PruneStats {
    let Ok(entries) = std::fs::read_dir(root) else {
        return PruneStats::default();
    };
    let mut items: Vec<(SystemTime, PathBuf, u64)> = Vec::new();
    let mut total = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let size = dir_size(&path);
        total += size;
        // index.json 的 mtime 才是"最近分析时间"；目录 mtime 会被子目录写入改掉。
        let stamp = std::fs::metadata(path.join("index.json"))
            .and_then(|meta| meta.modified())
            .or_else(|_| meta.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        items.push((stamp, path, size));
    }
    if total <= limit {
        return PruneStats {
            removed: 0,
            freed: 0,
            kept_bytes: total,
        };
    }

    items.sort_by_key(|(stamp, _, _)| *stamp);
    let mut removed = 0usize;
    let mut freed = 0u64;
    for (_, path, size) in items {
        if total <= limit {
            break;
        }
        if std::fs::remove_dir_all(&path).is_ok() {
            total = total.saturating_sub(size);
            freed += size;
            removed += 1;
        }
    }
    PruneStats {
        removed,
        freed,
        kept_bytes: total,
    }
}

fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        // symlink_metadata：不跟随符号链接，避免缓存里被塞一个环。
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            total += dir_size(&entry.path());
        } else if meta.is_file() {
            total += meta.len();
        }
    }
    total
}

/// 单测用：把 mtime 拨到指定秒数，避免同一秒内建三个目录分不出新旧。
#[cfg(test)]
fn touch_at(path: &Path, seconds: u64) {
    let stamp = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds);
    let Ok(file) = std::fs::OpenOptions::new().write(true).open(path) else {
        return;
    };
    let _ = file.set_times(std::fs::FileTimes::new().set_modified(stamp));
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "prism-index-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建测试目录");
        dir
    }

    fn sample(sha: &str) -> SampleRef {
        SampleRef {
            sha256: sha.to_string(),
            size: 4096,
            format: "mach-o".to_string(),
            arch: Some("arm64".to_string()),
            bits: Some(64),
            image_base: Some(0x1000_0000),
        }
    }

    fn index_for(sha: &str) -> ProgramIndex {
        ProgramIndex {
            schema: SCHEMA,
            backend: "radare2".to_string(),
            backend_version: "5.9.8".to_string(),
            sample: sample(sha),
            functions: vec![FunctionEntry {
                va: 0x1000_0400,
                name: "main".to_string(),
                size: 32,
                kind: "normal".to_string(),
                blocks: None,
            }],
            imports: Vec::new(),
            truncated: false,
            note: None,
        }
    }

    #[test]
    fn aflj_maps_offset_name_size_and_kind() {
        let text = r#"
            WARN: something
            [
              {"offset": 4294971392, "name": "entry0", "size": 32, "type": "sym"},
              {"offset": 4294971424, "name": "sub_100001080", "size": 96, "type": "fcn"},
              {"offset": 4294971520, "name": "sym.imp.printf", "size": 16, "type": "plt"},
              {"offset": 4294971536, "name": "", "size": 8, "type": "thunk"}
            ]
        "#;
        let index = from_aflj(text, sample(&"a".repeat(64)), "5.9.8").expect("应能解析");
        assert_eq!(index.schema, 1);
        assert_eq!(index.backend, "radare2");
        assert_eq!(index.backend_version, "5.9.8");
        assert!(!index.truncated);
        assert_eq!(index.functions.len(), 4);
        assert_eq!(index.functions[0].va, 4294971392);
        assert_eq!(index.functions[0].name, "entry0");
        assert_eq!(index.functions[0].kind, "entry");
        assert_eq!(index.functions[1].size, 96);
        assert_eq!(index.functions[2].kind, "plt");
        // 空名字用地址补，不留空串给下游。
        assert_eq!(index.functions[3].name, "sub_100001090");
        assert_eq!(index.functions[3].kind, "thunk");
        assert!(index.functions.iter().all(|item| item.blocks.is_none()));
    }

    #[test]
    fn aflj_truncates_at_the_limit() {
        let entries: Vec<serde_json::Value> = (0..(MAX_FUNCTIONS + 1))
            .map(|index| serde_json::json!({ "offset": index as u64, "name": format!("f{index}"), "size": 4, "type": "fcn" }))
            .collect();
        let text = serde_json::to_string(&entries).expect("序列化 fixture");
        let index = from_aflj(&text, sample(&"b".repeat(64)), "5.9.8").expect("应能解析");
        assert!(index.truncated);
        assert_eq!(index.functions.len(), MAX_FUNCTIONS);
        assert!(index.note.unwrap_or_default().contains("20000"));
    }

    #[test]
    fn bad_output_is_an_error_not_a_guess() {
        assert!(from_aflj("not json at all", sample(&"c".repeat(64)), "5.9.8").is_err());
        assert!(json_slice("no array here").is_none());
        assert_eq!(json_slice("noise [1,2] noise"), Some("[1,2]"));
    }

    #[test]
    fn ghidra_export_maps_blocks_imports_and_image_base() {
        let text = r#"{
          "functions": [
            {"va": 4198400, "name": "entry", "size": 64, "kind": "entry", "blocks": [
              {"va": 4198400, "size": 24, "succ": [4198424]},
              {"va": 4198424, "size": 8, "succ": []}
            ]},
            {"va": 4198432, "name": "thunk_FUN_1", "size": 5, "kind": "thunk", "blocks": []}
          ],
          "imports": [{"name": "printf", "library": "libc"}],
          "truncated": false,
          "imageBase": 4294967296
        }"#;
        let mut sample = sample(&"9".repeat(64));
        sample.image_base = None;
        let index = from_ghidra(text, sample, "11.2.1").expect("应能解析");
        assert_eq!(index.backend, "ghidra");
        assert_eq!(index.sample.image_base, Some(0x1_0000_0000));
        assert_eq!(index.functions.len(), 2);
        assert_eq!(index.functions[0].kind, "entry");
        let blocks = index.functions[0].blocks.as_ref().expect("应有基本块");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].succ, vec![4198424]);
        assert!(index.functions[1].blocks.is_none());
        assert_eq!(index.imports[0].name, "printf");
        assert_eq!(index.imports[0].library.as_deref(), Some("libc"));
    }

    #[test]
    fn ghidra_export_truncation_is_reported() {
        let entries: Vec<serde_json::Value> = (0..(MAX_FUNCTIONS + 1))
            .map(|index| serde_json::json!({ "va": index as u64, "name": format!("f{index}"), "size": 4, "kind": "normal" }))
            .collect();
        let text = serde_json::json!({ "functions": entries, "imports": [] }).to_string();
        let index = from_ghidra(&text, sample(&"8".repeat(64)), "11.2.1").expect("应能解析");
        assert!(index.truncated);
        assert_eq!(index.functions.len(), MAX_FUNCTIONS);
    }

    #[test]
    fn dex_methods_use_code_off_as_va() {
        use crate::engines::native::dex::DexMethod;
        let methods = vec![
            DexMethod {
                name: "Lapp/Main;->onCreate".to_string(),
                va: 0x1234,
                size: 42,
                access: 1,
            },
            DexMethod {
                name: "Lapp/Main;->run".to_string(),
                va: 0x2000,
                size: 8,
                access: 1,
            },
        ];
        let index = from_dex_methods(methods, sample(&"d".repeat(64)), 2);
        assert_eq!(index.backend, "dex");
        assert_eq!(index.functions[0].va, 0x1234);
        assert_eq!(index.functions[0].size, 42);
        assert_eq!(index.functions[0].kind, "normal");
        let note = index.note.unwrap_or_default();
        assert!(note.contains("code_off"), "{note}");
        assert!(note.contains("2 个类"), "{note}");
    }

    #[test]
    fn cache_roundtrip_checks_backend_and_version() {
        let root = scratch("cache");
        let sha = "e".repeat(64);
        let index = index_for(&sha);
        let path = save_at(&root, &index).expect("写缓存");
        assert!(path.is_file());

        assert!(load_at(&root, &sha, "radare2", "5.9.8").is_some());
        // 换后端 / 换版本 / 换样本：都不该命中。
        assert!(load_at(&root, &sha, "ghidra", "5.9.8").is_none());
        assert!(load_at(&root, &sha, "radare2", "5.9.9").is_none());
        assert!(load_at(&root, &"f".repeat(64), "radare2", "5.9.8").is_none());
        // 路径穿越不该被当成缓存键。
        assert!(load_at(&root, "../../etc", "radare2", "5.9.8").is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prune_removes_the_oldest_until_under_limit() {
        let root = scratch("prune");
        // 三个缓存目录，每个 index.json 100 字节。
        for (index, seconds) in [("1", 1_600_000_100u64), ("2", 1_600_000_200), ("3", 1_600_000_300)] {
            let sha = index.repeat(64);
            let dir = root.join(&sha);
            std::fs::create_dir_all(&dir).expect("建缓存目录");
            let file = dir.join("index.json");
            std::fs::write(&file, vec![b'x'; 100]).expect("写 index");
            touch_at(&file, seconds);
        }
        // 留 250 字节：必须删掉最旧的那一个。
        let stats = prune_cache_at(&root, 250);
        assert_eq!(stats.removed, 1, "{stats:?}");
        assert!(stats.kept_bytes <= 250, "{stats:?}");
        assert!(!root.join("1".repeat(64)).exists(), "最旧的没删");
        assert!(root.join("2".repeat(64)).exists());
        assert!(root.join("3".repeat(64)).exists());

        // 已经在上限内：不动。
        let again = prune_cache_at(&root, 250);
        assert_eq!(again.removed, 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn save_rejects_a_non_sha_key() {
        let root = scratch("bad-key");
        let mut index = index_for(&"0".repeat(64));
        index.sample.sha256 = "../escape".to_string();
        assert!(save_at(&root, &index).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
