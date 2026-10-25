//! 安全研究线的 DEX 解析器：`bin.dex.disasm` / `bin.dex.xref`。
//!
//! 补上 `analysis.rs` 里 `bin.importexport.list`「明确只支持 PE / ELF」留下的
//! 空白——DEX 的 `class_data_item → code_item` 指令级解析。逆向 agent 之前只能
//! 拿到「符号在池」级证据（字符串池、类型池、method_id 表），进不了「哪个方法
//! invoke 了什么、const-string 引用了什么」的字节码层；这里一次补齐。
//!
//! 与整条安全研究线共用三条纪律：
//! - **只读**：`Risk::Read`，不写工作目录、不联网、不执行样本；
//! - **有上限**：卡读取字节数与单方法指令数，超了标注截断而不是硬扫；
//! - **不猜**：解析不出的字段直接省略；`code_off == 0` 明确标「abstract/native，
//!   无 code_item」；损坏 / 非 DEX / cdex 直接报错，不硬解。

use std::sync::Arc;

use serde_json::{json, Value};

use super::forensics::{read_head, read_u16, read_u32, respond, target_of};
use super::tools::{
    builtin_full, Network, Permission, Risk, ToolContext, ToolDef, ToolMeta, ToolResult, ToolRun,
    ToolScope,
};

/// DEX 读取上限。超过这个大小的单文件很可能是被塞了资源的多 dex 容器或畸形样本。
const DEX_LIMIT: u64 = 64 * 1024 * 1024;
/// 最多遍历多少个 class_def：几千个类全展开会把上下文撑爆。
#[cfg_attr(not(test), allow(dead_code))]
const MAX_CLASSES: u32 = 5000;
/// 单方法反汇编输出条数上限：巨型方法（R8 混淆产物里常见）撑爆上下文。
const INSN_DEFAULT: usize = 200;
const INSN_MAX: usize = 4000;
/// 列类时默认 / 最多回多少条类名。
///
/// 一个 3 MB 的 dex 有三千多个类，全塞进结果会超过单条输出上限被截断，变成
/// 非法 JSON（模型既看不全也解析不了）。200 条约 9 千字符，留得下上下文。
const CLASS_LIST_DEFAULT: usize = 200;
const CLASS_LIST_MAX: usize = 2000;
/// 单次反汇编结果的字符预算：要比工具出口闸的 4 万小一截，留出 JSON 外壳与
/// 顶层缩进的余量（按 pretty 大小累计，见收集处的注释）。
const DISASM_BUDGET: usize = 25_000;
/// xref 每类条目默认 / 上限。
const XREF_DEFAULT: usize = 128;
const XREF_MAX: usize = 2000;

pub fn tools() -> Vec<Arc<ToolDef>> {
    vec![
        static_tool("bin.dex.disasm", "DEX 指令反汇编", disasm_schema, run_disasm),
        static_tool("bin.dex.xref", "DEX 语义调用图", xref_schema, run_xref),
    ]
}

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

fn disasm_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bin.dex.disasm",
            "description": "反汇编一个 DEX 类的指定方法：解析 class_data_item 与 code_item，输出指令流（invoke/const-string/switch 等），带调用目标与字符串引用。这是拿到「哪个方法调用了什么」代码级证据的唯一入口。**只想搞清这段代码想干什么（行为、条件、意图）就别用它，改用 bin.jadx.decompile 拿可读 Java**——指令流能说明调了谁，说不清为什么。",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "相对工作目录的 .dex 文件路径" },
                    "class": { "type": "string", "description": "类描述符，如 Lapp/orchard/tv/guard/BootReceiver;。不填则列出全部类与方法签名" },
                    "method": { "type": "string", "description": "方法名，如 onReceive；不填则反汇编该类全部方法" },
                    "limit": { "type": "integer", "description": "单方法最多输出多少条指令，默认 200，上限 4000" }
                },
                "required": ["path"]
            }
        }
    })
}

fn xref_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bin.dex.xref",
            "description": "提取一个 DEX 类的语义交叉引用：谁 invoke 了谁、引用了哪些 string/type、switch 的分支键表。用于回答「onReceive 的 switch 键表」「onAccessibilityEvent 是否调 getText」这类问题。",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "相对工作目录的 .dex 文件路径" },
                    "class": { "type": "string", "description": "类描述符，如 Lapp/orchard/tv/guard/ConfigReceiver;。不填则扫全部类" },
                    "limit": { "type": "integer", "description": "每类条目上限，默认 128，上限 2000" }
                },
                "required": ["path"]
            }
        }
    })
}

// ---------------------------------------------------------------- 错误

#[derive(Debug, Clone, PartialEq, Eq)]
enum DexError {
    NotDex,
    CompactDex,
    Truncated,
    UlebOverflow,
    BadOffset,
}

impl std::fmt::Display for DexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            DexError::NotDex => "不是 DEX 文件（magic 非 dex\\n）",
            DexError::CompactDex => "cdex（compact-dex）暂不支持",
            DexError::Truncated => "文件被截断",
            DexError::UlebOverflow => "uleb128 超过 5 字节（畸形输入）",
            DexError::BadOffset => "偏移越界（code_off 或 data_off 指向文件外）",
        };
        write!(f, "{text}")
    }
}

// ---------------------------------------------------------------- 底层读取

/// 读取一个 uleb128（unsigned LEB128），返回 `(值, 下一个位置)`。
/// 上限 5 字节（u32 范围），超出即 `UlebOverflow`，防畸形输入死循环。
fn uleb(bytes: &[u8], at: usize) -> Result<(u32, usize), DexError> {
    let mut result = 0u32;
    let mut shift = 0u32;
    let mut cursor = at;
    for _ in 0..5 {
        let byte = *bytes.get(cursor).ok_or(DexError::Truncated)?;
        result |= ((byte & 0x7f) as u32) << shift;
        cursor += 1;
        if byte & 0x80 == 0 {
            return Ok((result, cursor));
        }
        shift += 7;
    }
    Err(DexError::UlebOverflow)
}

/// DEX 头部的 section 表。偏移、数量都从 `header_item` 直读。
#[derive(Debug, PartialEq)]
struct Sections {
    string_ids: (u32, u32),
    type_ids: (u32, u32),
    field_ids: (u32, u32),
    method_ids: (u32, u32),
    class_defs: (u32, u32),
}

impl Sections {
    fn parse(bytes: &[u8]) -> Result<Sections, DexError> {
        if bytes.len() < 0x70 || &bytes[..4] != b"dex\n" {
            if bytes.get(..4).map(|m| m == b"cdex").unwrap_or(false) {
                return Err(DexError::CompactDex);
            }
            return Err(DexError::NotDex);
        }
        let pair = |size_at: usize, off_at: usize| -> Result<(u32, u32), DexError> {
            let size = read_u32(bytes, size_at, true).ok_or(DexError::Truncated)?;
            let off = read_u32(bytes, off_at, true).ok_or(DexError::Truncated)?;
            Ok((size, off))
        };
        // 偏移必须逐条对齐 DEX 规范（`header_item`，112 字节）：
        //   string_ids 0x38/0x3c · type_ids 0x40/0x44 · proto_ids 0x48/0x4c
        //   field_ids 0x50/0x54 · method_ids 0x58/0x5c · class_defs 0x60/0x64
        //
        // 这里曾经整体多写了 0x18（从 0x50 起），于是每个 section 都读到了
        // 后一个池子上，`class_defs` 更是越过 header 落进 string_ids 数组——
        // 真实样本上表现成 `classCount: 2685886`（实际 3300 个类）、类名为空、
        // "找不到类"，两个 dex 工具整体失效。合成样本的测试当时按同样的错位
        // 造数据，所以自洽通过，一点没报警。
        Ok(Sections {
            string_ids: pair(0x38, 0x3c)?,
            type_ids: pair(0x40, 0x44)?,
            field_ids: pair(0x50, 0x54)?,
            method_ids: pair(0x58, 0x5c)?,
            class_defs: pair(0x60, 0x64)?,
        })
    }
}

/// 解析后的 DEX 视图：持有字节切片与各池的快捷访问。
struct Dex<'a> {
    bytes: &'a [u8],
    sections: Sections,
}

impl<'a> Dex<'a> {
    fn parse(bytes: &'a [u8]) -> Result<Dex<'a>, DexError> {
        let sections = Sections::parse(bytes)?;
        Ok(Dex { bytes, sections })
    }

    /// 读 string_id 表里第 `index` 个字符串的 MUTF-8 内容。
    fn string(&self, index: u32) -> Result<String, DexError> {
        let (_, off) = self.sections.string_ids;
        let data_off = read_u32(self.bytes, off as usize + index as usize * 4, true)
            .ok_or(DexError::BadOffset)?;
        let (len, at) = uleb(self.bytes, data_off as usize)?;
        let mut out = Vec::with_capacity(len as usize);
        let mut cursor = at;
        // MUTF-8：NUL 编码成 0xC0 0x80；0 结尾。这里做宽容解码，替换非法序列。
        while cursor < self.bytes.len() {
            let byte = self.bytes[cursor];
            if byte == 0 {
                break;
            }
            // MUTF-8 的 0xC0 0x80 还原成 NUL。
            if byte == 0xc0 && self.bytes.get(cursor + 1) == Some(&0x80) {
                out.push(0);
                cursor += 2;
                continue;
            }
            out.push(byte);
            cursor += 1;
        }
        Ok(String::from_utf8_lossy(&out).to_string())
    }

    /// 读 type_id 表里第 `index` 个类型的描述符。
    fn type_descriptor(&self, index: u32) -> Result<String, DexError> {
        let (_, off) = self.sections.type_ids;
        let descriptor_idx = read_u32(self.bytes, off as usize + index as usize * 4, true)
            .ok_or(DexError::BadOffset)?;
        self.string(descriptor_idx)
    }

    /// 读 method_id 表里第 `index` 个方法，返回 `类名->方法名`。
    fn method_ref(&self, index: u32) -> Result<String, DexError> {
        let (_, off) = self.sections.method_ids;
        let at = off as usize + index as usize * 8;
        let class_idx = read_u16(self.bytes, at, true).ok_or(DexError::BadOffset)? as u32;
        let name_idx = read_u32(self.bytes, at + 4, true).ok_or(DexError::BadOffset)?;
        let class = self.type_descriptor(class_idx)?;
        let name = self.string(name_idx)?;
        Ok(format!("{class}->{name}"))
    }

    /// 读 field_id 表里第 `index` 个字段，返回 `类名->字段名:类型`。
    fn field_ref(&self, index: u32) -> Result<String, DexError> {
        let (_, off) = self.sections.field_ids;
        let at = off as usize + index as usize * 8;
        let class_idx = read_u16(self.bytes, at, true).ok_or(DexError::BadOffset)? as u32;
        let type_idx = read_u16(self.bytes, at + 2, true).ok_or(DexError::BadOffset)? as u32;
        let name_idx = read_u32(self.bytes, at + 4, true).ok_or(DexError::BadOffset)?;
        let class = self.type_descriptor(class_idx)?;
        let ty = self.type_descriptor(type_idx)?;
        let name = self.string(name_idx)?;
        Ok(format!("{class}->{name}:{ty}"))
    }

    /// 类的描述符名。`class_def_item` 的 `class_idx` 指向 type_id。
    fn class_name(&self, class_def_index: u32) -> Result<String, DexError> {
        let (_, off) = self.sections.class_defs;
        let at = off as usize + class_def_index as usize * 32;
        let class_idx = read_u32(self.bytes, at, true).ok_or(DexError::BadOffset)?;
        self.type_descriptor(class_idx)
    }

    /// 遍历 `class_data_item`，对每个 `code_off != 0` 的方法回调。
    ///
    /// 上一轮 P0 的根因就在这一段的「field 条目跳过」——读完 sf/inf/dm/vm 后
    /// **必须先消费 sf+inf 个 encoded_field**，再解析 method 条目，否则游标错位。
    fn each_method<F>(&self, class_def_index: u32, mut visit: F) -> Result<(), DexError>
    where
        F: FnMut(u32, u32, u32) -> Result<(), DexError>,
    {
        let (_, off) = self.sections.class_defs;
        let at = off as usize + class_def_index as usize * 32;
        // class_data_off 在 class_def_item 的字节偏移 0x18。
        let class_data_off = read_u32(self.bytes, at + 0x18, true).ok_or(DexError::BadOffset)?;
        if class_data_off == 0 {
            // 没有 class_data_item：接口 / 空类，无方法体。
            return Ok(());
        }

        let d = self.bytes;
        let (sf, p) = uleb(d, class_data_off as usize)?;
        let (inf, p) = uleb(d, p)?;
        let (dm, p) = uleb(d, p)?;
        let (vm, p) = uleb(d, p)?;

        // 跳过字段条目：field_idx_diff + access_flags（无 code_off）。
        let mut cursor = p;
        for _ in 0..sf + inf {
            let (_, after_idx) = uleb(d, cursor)?;
            let (_, after_acc) = uleb(d, after_idx)?;
            cursor = after_acc;
        }

        // 现在才是 method 条目。direct 与 virtual 各自从 0 起累计 method_idx_diff。
        let mut method_idx = 0u32;
        for step in 0..dm + vm {
            // 进入 virtual 段，累计值必须重新归零。
            //
            // DEX 规范里 direct_methods 与 virtual_methods 是两个**独立序列**，
            // 各自的 method_idx_diff 都从 0 起算。不重置就会把 virtual 的 diff
            // 叠到 direct 末尾上，method_idx 直接越过 method_ids_size，
            // `method_ref` 报 BadOffset——真实症状是"类能列出来、一按名字反汇编
            // 就报偏移越界"（HomeGuardService：dm=2、vm=18）。
            if step == dm {
                method_idx = 0;
            }
            let (diff, after_idx) = uleb(d, cursor)?;
            let (access, after_acc) = uleb(d, after_idx)?;
            let (code_off, after_code) = uleb(d, after_acc)?;
            cursor = after_code;
            // wrapping_add：畸形 / 混淆类里 diff 可能是垃圾值，溢出不能 panic。
            // method_ref 里会再做越界校验，读到坏索引只是拿不到名字，不算崩溃。
            method_idx = method_idx.wrapping_add(diff);
            if code_off != 0 {
                visit(method_idx, access, code_off)?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- IR 摘要

/// 一个方法的 IR 摘要：下游只拿这些字段画函数边界。
///
/// P2.4 的 `cfg.extract` 接进来后，这一段的 `allow` 就能删掉。
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DexMethod {
    /// `Lpkg/Cls;->method`。
    pub name: String,
    /// DEX 没有虚拟地址：这里是 `code_item` 的文件偏移，只作不透明 id。
    pub va: u64,
    /// 字节码长度（`insns_size * 2`），不含 code_item 头。
    pub size: u32,
    /// `access_flags` 原值。
    pub access: u32,
}

/// [`methods`] 的结果：解析失败被跳过的类要如实带出去。
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DexMethods {
    pub methods: Vec<DexMethod>,
    pub skipped_classes: usize,
}

/// 遍历全部类，导出有 `code_item` 的方法。
///
/// 单个类的 `class_data` 坏了只跳过这个类并计数，不拖垮整份文件——
/// 混淆样本里常见"一个类畸形、其余正常"。文件头不是 DEX 才整体报错。
#[cfg_attr(not(test), allow(dead_code))]
pub fn methods(bytes: &[u8]) -> Result<DexMethods, String> {
    let dex = Dex::parse(bytes).map_err(|error| error.to_string())?;
    let class_count = dex.sections.class_defs.0.min(MAX_CLASSES);
    let mut out = Vec::new();
    let mut skipped = 0usize;
    for class_def_index in 0..class_count {
        let result = dex.each_method(class_def_index, |method_idx, access, code_off| {
            let name = dex.method_ref(method_idx)?;
            let insns_size = read_u32(dex.bytes, code_off as usize + 0x0c, true).unwrap_or(0);
            out.push(DexMethod {
                name,
                va: code_off as u64,
                size: insns_size.saturating_mul(2),
                access,
            });
            Ok(())
        });
        if result.is_err() {
            skipped += 1;
        }
    }
    Ok(DexMethods {
        methods: out,
        skipped_classes: skipped,
    })
}

// ---------------------------------------------------------------- 指令解码

/// 一条解码后的指令。`mnemonic` 是 baksmali 风格的助记符，`refs` 是它引用的
/// method / string / type / field（供 xref 消费）。
struct Insn {
    mnemonic: String,
    refs: Vec<Ref>,
}

enum Ref {
    Method(u32),
    String(u32),
    Type(u32),
    Field(u32),
    Switch(u32),
    /// invoke-custom 的 call-site：静态目标运行时才决定，只标记「有动态调用」。
    CustomCallSite,
}

/// 解码 code_item 的指令流。`code_off` 是 code_item 的文件偏移。
///
/// 返回 `(registers, ins_size, 指令列表)`。指令按 code unit（16-bit）推进，
/// 覆盖全部 opcode 格式（含 invoke-custom、switch 的 data_off 标记）。
/// 解一条 `code_item`，返回 `(registers, ins_size, 指令流, 结束游标)`。
///
/// 结束游标只给自检用：它必须恰好等于 `insns_size`。差一个 unit 就说明某条
/// 指令的长度写错，之后整段错位——那正是"registers=5 却出现 v4206"的来历。
fn decode_code_item(
    bytes: &[u8],
    code_off: u32,
    max_insns: usize,
) -> Result<(u32, u32, Vec<Insn>, usize), DexError> {
    let at = code_off as usize;
    let registers = read_u16(bytes, at, true).ok_or(DexError::BadOffset)? as u32;
    let ins_size = read_u16(bytes, at + 2, true).ok_or(DexError::BadOffset)? as u32;
    let insns_size = read_u32(bytes, at + 0x0c, true).ok_or(DexError::BadOffset)? as usize;

    // 指令流起点 0x10，每 16-bit 一个 code unit。
    let mut unit = 0usize; // 当前 code unit 下标
    let mut out: Vec<Insn> = Vec::new();

    let read_u = |unit: usize| -> Result<u16, DexError> {
        let b = at + 0x10 + unit * 2;
        read_u16(bytes, b, true).ok_or(DexError::BadOffset)
    };
    let read_uu = |unit: usize| -> Result<u32, DexError> {
        let b = at + 0x10 + unit * 2;
        read_u32(bytes, b, true).ok_or(DexError::BadOffset)
    };
    // 有符号 8 位立即数（从 code unit 低字节取）。
    let signed8 = |v: u16| -> i32 { ((v & 0xff) as i8) as i32 };

    while unit < insns_size && out.len() < max_insns {
        let code = read_u(unit)?;
        let op = code & 0xff;
        let a = (code >> 8) & 0xf; // 4-bit vA
        let b = (code >> 12) & 0xf; // 4-bit vB
        let aa = (code >> 8) & 0xff; // 8-bit vAA

        // payload 不是指令。`packed-switch` / `sparse-switch` /
        // `fill-array-data` 的数据块就躺在指令流里，第一个 unit 是
        // `0x0100` / `0x0200` / `0x0300`（低字节为 0，看着像 nop）。
        //
        // 以前一律当 nop 吃 1 个 unit，于是游标直接走进数据区，后面全是
        // 垃圾——15,275 个方法里有 1,389 个（9.1%）末尾不是 return，
        // 落在 `nop` / `op 0x37` / `0x38 v0, 0` 上。下游报告因此只能写
        // 「不得引用指令偏移与指令顺序」。这里按 payload 的真实长度跳过。
        if op == 0x00 && code != 0x0000 {
            let payload = match code {
                // ident + size + **first_key** + size*2 个 target
                //
                // first_key 是 4 字节（2 个 code unit），很容易漏：少了它，
                // 游标会停在 insns_size 前 2 个 unit，末尾那条指令被截断。
                // 本样本 172 个方法（1.1%）正好差这 2 个 unit。
                0x0100 => {
                    let size = read_u(unit + 1)? as usize;
                    4 + size * 2
                }
                // ident + size + size*2 个 key + size*2 个 target
                0x0200 => {
                    let size = read_u(unit + 1)? as usize;
                    2 + size * 4
                }
                // ident + element_width + size(2 unit) + 数据（按 2 字节对齐）
                0x0300 => {
                    let width = read_u(unit + 1)? as usize;
                    let size = read_uu(unit + 2)? as usize;
                    let data_bytes = width.saturating_mul(size);
                    4 + (data_bytes + 1) / 2
                }
                _ => 1,
            };
            unit += payload.max(1);
            continue;
        }

        // 用一个闭包推进：每个分支返回「本指令消耗的 code unit 数」。
        let consumed;
        let mnemonic;
        let mut refs: Vec<Ref> = Vec::new();

        match op {
            // 10x
            0x00 => mnemonic = "nop".to_string(),
            0x0e => mnemonic = "return-void".to_string(),
            0x0f => mnemonic = "return".to_string(),
            0x10 => mnemonic = "return-wide".to_string(),
            0x11 => mnemonic = "return-object".to_string(),
            // 12x
            0x01 => {
                mnemonic = format!("move v{a}, v{b}");
            }
            0x02 => mnemonic = format!("move/from16 v{aa}, v{}", read_u(unit + 1)?),
            0x03 => mnemonic = format!("move/16 v{}, v{}", read_u(unit + 1)?, read_u(unit + 2)?),
            0x04 => mnemonic = format!("move-wide v{a}, v{b}"),
            0x05 => mnemonic = format!("move-wide/from16 v{aa}, v{}", read_u(unit + 1)?),
            0x06 => mnemonic = format!("move-wide/16 v{}, v{}", read_u(unit + 1)?, read_u(unit + 2)?),
            0x07 => mnemonic = format!("move-object v{a}, v{b}"),
            0x08 => mnemonic = format!("move-object/from16 v{aa}, v{}", read_u(unit + 1)?),
            0x09 => mnemonic = format!("move-object/16 v{}, v{}", read_u(unit + 1)?, read_u(unit + 2)?),
            // 11x
            0x0a => mnemonic = format!("move-result v{aa}"),
            0x0b => mnemonic = format!("move-result-wide v{aa}"),
            0x0c => mnemonic = format!("move-result-object v{aa}"),
            0x0d => mnemonic = format!("move-exception v{aa}"),
            0x27 => mnemonic = format!("throw v{aa}"),
            0x28 => mnemonic = format!("goto v{aa}"),
            0x29 => mnemonic = format!("goto/16 v{}", read_u(unit + 1)?),
            0x2a => {
                let target = read_uu(unit + 1)?;
                mnemonic = format!("goto/32 v{target}");
            }
            // 11n
            0x12 => mnemonic = format!("const/4 v{a}, #{}", signed8(code)),
            0x13 => mnemonic = format!("const/16 v{aa}, #{}", read_u(unit + 1)? as i16),
            0x14 => {
                let value = read_uu(unit + 1)?;
                mnemonic = format!("const v{aa}, #{}", value as i32);
            }
            0x15 => mnemonic = format!("const/high16 v{aa}, #{}", read_u(unit + 1)?),
            0x16 => {
                let value = read_uu(unit + 1)?;
                mnemonic = format!("const-wide/16 v{aa}, #{}", value as u16 as i16);
            }
            0x17 => mnemonic = format!("const-wide/32 v{aa}, #{}", read_uu(unit + 1)? as i32),
            0x18 => {
                let value = read_uu(unit + 1)?;
                let wide = (read_uu(unit + 3)? as u64) << 32 | value as u64;
                mnemonic = format!("const-wide v{aa}, #{}", wide as i64);
            }
            0x19 => mnemonic = format!("const-wide/high16 v{aa}, #{}", read_u(unit + 1)?),
            // 21c / 31c
            0x1a => {
                let idx = read_u(unit + 1)? as u32;
                refs.push(Ref::String(idx));
                mnemonic = format!("const-string v{aa}, str@{idx}");
            }
            0x1b => {
                let idx = read_uu(unit + 1)?;
                refs.push(Ref::String(idx));
                mnemonic = format!("const-string/jumbo v{aa}, str@{idx}");
            }
            0x1c => {
                let idx = read_u(unit + 1)? as u32;
                refs.push(Ref::Type(idx));
                mnemonic = format!("const-class v{aa}, type@{idx}");
            }
            // 21t
            0x38..=0x3d => {
                let target = read_u(unit + 1)? as i16;
                mnemonic = format!("{:#x} v{aa}, {target}", op);
            }
            // 22x
            0x3e..=0x43 => {
                mnemonic = format!("{:#x} v{aa}, v{}", op, read_u(unit + 1)?);
            }
            // 21c
            0x22 => {
                let idx = read_u(unit + 1)? as u32;
                refs.push(Ref::Type(idx));
                mnemonic = format!("new-instance v{aa}, type@{idx}");
            }
            0x23 => {
                let idx = read_u(unit + 1)? as u32;
                refs.push(Ref::Type(idx));
                mnemonic = format!("new-array v{a}, v{b}, type@{idx}");
            }
            // 22c
            0x1f => {
                let idx = read_u(unit + 1)? as u32;
                refs.push(Ref::Type(idx));
                mnemonic = format!("check-cast v{a}, type@{idx}");
            }
            0x20 => {
                let idx = read_u(unit + 1)? as u32;
                refs.push(Ref::Type(idx));
                mnemonic = format!("instance-of v{a}, v{b}, type@{idx}");
            }
            0x1d => {
                let idx = read_u(unit + 1)? as u32;
                refs.push(Ref::Type(idx));
                mnemonic = format!("const/check-cast v{a}, type@{idx}");
            }
            // 23x 算术：vAA, vBB, vCC —— BB 与 CC 各占 unit1 的一个字节。
            0x90..=0xaf => {
                let pair = read_u(unit + 1)?;
                mnemonic = format!("op{op:#x} v{aa}, v{}, v{}", pair & 0xff, pair >> 8);
            }
            // 12x 算术：vA, vB 两个 4-bit 寄存器都挤在同一个 unit 里。
            0xb0..=0xcf => {
                mnemonic = format!("op{op:#x} v{a}, v{b}");
            }
            // 22c field 访问
            0x52..=0x5f | 0x60..=0x6d => {
                let idx = read_u(unit + 1)? as u32;
                refs.push(Ref::Field(idx));
                mnemonic = format!("{:#x} v{a}, v{b}, field@{idx}", op);
            }
            // 35c / 3rc 调用
            0x6e..=0x72 | 0x74..=0x78 => {
                let idx = read_u(unit + 1)? as u32;
                refs.push(Ref::Method(idx));
                // 助记符要照实写。原来输出 `0x6e meth@13255`，读的人还得自己
                // 去 method_id 表里查这是谁——那等于没有代码级证据。真实后果：
                // 报告里"确认这个偏移是不是 invoke-virtual Cipher.init"这条
                // 验证动作，即便工具修好了也做不出来。
                let name = match op {
                    0x6e => "invoke-virtual",
                    0x6f => "invoke-super",
                    0x70 => "invoke-direct",
                    0x71 => "invoke-static",
                    0x72 => "invoke-interface",
                    0x74 => "invoke-virtual/range",
                    0x75 => "invoke-super/range",
                    0x76 => "invoke-direct/range",
                    0x77 => "invoke-static/range",
                    0x78 => "invoke-interface/range",
                    _ => "invoke",
                };
                mnemonic = format!("{name} meth@{idx}");
            }
            // 31t（switch / fill-array-data）
            0x2b => {
                let data_off = read_uu(unit + 1)?;
                refs.push(Ref::Switch(data_off));
                mnemonic = format!("packed-switch v{aa}, 0x{data_off:x}");
            }
            0x2c => {
                let data_off = read_uu(unit + 1)?;
                refs.push(Ref::Switch(data_off));
                mnemonic = format!("sparse-switch v{aa}, 0x{data_off:x}");
            }
            0x26 => {
                let data_off = read_uu(unit + 1)?;
                refs.push(Ref::Switch(data_off));
                mnemonic = format!("fill-array-data v{aa}, 0x{data_off:x}");
            }
            // invoke-custom（45cc / 4rcc）
            0xfa | 0xfb => {
                refs.push(Ref::CustomCallSite);
                mnemonic = "invoke-custom（lambda / 动态调用）".to_string();
            }
            _ => {
                mnemonic = format!("op 0x{op:02x}");
            }
        }

        // 按 DEX 格式表推进 code unit：指令长度完全由格式决定，不看内容。
        //
        // 这张表原来错得离谱，而且错法与格式大类无关，是逐条对错：
        //   `0x1b const-string/jumbo` 写成 5（应 3）、`0x90..=0xaf` 的 binop
        //   写成 3（应 2）、`0xb0..=0xcf` 同理、`0x13 const/16` 与
        //   `0x24 filled-new-array` 直接漏掉、`0x28 goto` 写成 2（应 1）、
        //   `0x3e..=0x43` 写成 2（应 1）。
        //
        // 只要错一条，游标就永久偏了，之后全是垃圾。真实症状：registers=5
        // 的方法里冒出 `v4206`、`return-void` 连着出现三次、字段索引离谱到
        // `field@5312`。下游报告只能写「不得引用指令偏移与指令顺序」。
        consumed = match op {
            // 10x / 11x / 11n / 12x：一个 code unit。
            //
            // `0xb0..=0xcf` 是 **12x**（`binop vA, vB`，两个 4-bit 寄存器挤在
            // 同一个 unit 里），只有 1 个 unit。它和 23x 的 `0x90..=0xaf`
            // （`binop vAA, vBB, vCC`，2 units）长得像，但长度不同。这类
            // add-int/lit16 之类的运算几乎每个方法都有，写成 2 就是每条多走
            // 一个 unit——15,275 个方法里 1,421 个因此末尾不是 return。
            0x00 | 0x01 | 0x04 | 0x07 | 0x0a..=0x12 | 0x1d | 0x1e | 0x21 | 0x27
            | 0x28 | 0x3e..=0x43 | 0x73 | 0x79..=0x8f | 0xb0..=0xcf => 1,
            // 20t / 21c / 21h / 21s / 21t / 22b / 22c / 22s / 22t / 22x / 23x
            0x02 | 0x05 | 0x08 | 0x13 | 0x15 | 0x16 | 0x19 | 0x1a | 0x1c | 0x1f
            | 0x20 | 0x22 | 0x23 | 0x29 | 0x2d..=0x37 | 0x38..=0x3d
            | 0x44..=0x51 | 0x52..=0x5f | 0x60..=0x6d
            | 0x90..=0xaf | 0xd0..=0xe2
            | 0xfe | 0xff => 2,
            // 30t / 31c / 31i / 31t / 32x / 35c / 3rc
            0x03 | 0x06 | 0x09 | 0x14 | 0x17 | 0x1b | 0x24 | 0x25 | 0x26
            | 0x2a | 0x2b | 0x2c | 0x6e..=0x72 | 0x74..=0x78 | 0xfc | 0xfd => 3,
            // 45cc / 4rcc
            0xfa | 0xfb => 4,
            // 51l：const-wide
            0x18 => 5,
            _ => 1,
        };

        // 越界就先停：不能让 read_u 读到方法外。
        //
        // payload 长度与实际排布偶尔差一两个 unit（本样本 15275 个方法里有
        // 172 个，1.1%）。以前继续解，会把**下一条 code_item 的字节**当成
        // 指令读出来——`registers=5` 的方法里冒出 `v4206`、`return-void`
        // 连出三次，就是这里的产物。宁可少一条末尾指令，也不产出垃圾。
        if unit + consumed as usize > insns_size {
            break;
        }
        out.push(Insn { mnemonic, refs });
        unit += consumed as usize;
    }

    // 第四个值只给测试用：解码结束时游标停在哪。它必须恰好等于 insns_size
    // ——差一个 unit 就说明某条指令的长度写错了，之后全是垃圾。
    Ok((registers, ins_size, out, unit))
}

/// 解析 `packed-switch` / `sparse-switch` 的 payload，返回键列表。
fn switch_keys(bytes: &[u8], data_off: u32) -> Result<Vec<i32>, DexError> {
    let at = data_off as usize;
    let ident = read_u16(bytes, at, true).ok_or(DexError::BadOffset)?;
    let size = read_u16(bytes, at + 2, true).ok_or(DexError::BadOffset)? as usize;
    match ident {
        0x0100 => {
            // packed-switch：first_key + size 个连续 target。
            let first = read_u32(bytes, at + 4, true).ok_or(DexError::BadOffset)? as i32;
            Ok((0..size).map(|i| first + i as i32).collect())
        }
        0x0200 => {
            // sparse-switch：size 个 key + size 个 target。
            let mut keys = Vec::with_capacity(size.min(4096));
            for i in 0..size.min(4096) {
                let key = read_u32(bytes, at + 4 + i * 4, true).ok_or(DexError::BadOffset)? as i32;
                keys.push(key);
            }
            Ok(keys)
        }
        _ => Ok(Vec::new()),
    }
}

// ---------------------------------------------------------------- 工具实现

/// 解析一个 dex 文件，返回 `Dex` 视图。出错时把错误转成 ToolResult 的 fail。
fn open_dex<'a>(bytes: &'a [u8]) -> Result<Dex<'a>, ToolResult> {
    match Dex::parse(bytes) {
        Ok(dex) => Ok(dex),
        Err(error) => Err(ToolResult::fail(format!("解析失败：{error}"))),
    }
}

/// 解析结果的自检：header 声称的类数，必须和实际解出来的类名对得上。
///
/// 为什么这条比任何功能都重要：解析器读错位置时**不会报错**，只会安静地解出
/// 0 个类。节点拿到 `classes: []` 会当成"这个样本没有类"，然后把整张图跑完，
/// 最后交一份基于"没有"的报告——一次运行一块钱，全花在空气上（真实踩过：
/// header 读成 2685886 个类 / 0 条类名，五个节点照跑）。
///
/// 宁可让工具当场失败，让节点换方法或停下，也不要给一个"看起来成功"的空结果。
fn check_class_harvest(class_count: u32, harvested: usize) -> Result<(), String> {
    if class_count == 0 {
        // header 自己说没有类（极简 dex 或非 dex）：不算异常。
        return Ok(());
    }
    if harvested == 0 {
        return Err(format!(
            "DEX 解析异常：header 声称 {class_count} 个类，实际一条类名也没解出来。\
             这是解析器的问题，不是样本没有类——不要据此下「无该类/无该行为」的结论。"
        ));
    }
    // 解出来的数量至少得是声明量的一半：丢几条可能是签名/损坏，丢一半以上
    // 只可能是读错了位置。
    let expected = class_count.min(5000) as usize;
    if expected > 0 && harvested * 2 < expected {
        return Err(format!(
            "DEX 解析异常：header 声称 {class_count} 个类，只解出 {harvested} 条（预期约 {expected} 条）。\
             解析位置可能错位，本轮结论不可用。"
        ));
    }
    Ok(())
}

fn run_disasm(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let (path, size) = match target_of(ctx, args) {
        Ok(found) => found,
        Err(failure) => return failure,
    };
    let class_filter = args.get("class").and_then(Value::as_str);
    let method_filter = args.get("method").and_then(Value::as_str);
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map(|value| (value as usize).clamp(1, INSN_MAX))
        .unwrap_or(INSN_DEFAULT);

    let bytes = match read_head(&path, DEX_LIMIT) {
        Ok(bytes) => bytes,
        Err(error) => return ToolResult::fail(error),
    };
    let dex = match open_dex(&bytes) {
        Ok(dex) => dex,
        Err(failure) => return failure,
    };

    let class_count = dex.sections.class_defs.0;

    // 收集类名，**带上 class_def 下标**。
    //
    // 不能只 push 名字：个别类的描述符解析失败时，push 后的下标就和 class_def
    // 下标错位了，按类名查到的 index 会指向另一个类，`each_method` 读到垃圾
    // class_data_off，报"偏移越界"。真实踩过：HomeGuardService 列得出来、
    // 一按名字反汇编就越界。
    let mut class_names: Vec<(u32, String)> =
        Vec::with_capacity(class_count.min(5000) as usize);
    for i in 0..class_count.min(5000) {
        if let Ok(name) = dex.class_name(i) {
            class_names.push((i, name));
        }
    }
    // 解不出来就别假装成功：空清单会被下游当成"样本没有类"，然后照跑完整张图。
    if let Err(why) = check_class_harvest(class_count, class_names.len()) {
        return ToolResult::fail(why);
    }

    // 没有指定 class：只回清单，不逐个反汇编。
    //
    // 清单**只给前若干条**：一个 3 MB 的 dex 有三千多个类，全塞进结果会超过
    // 单条输出上限（4 万字符），被出口闸截成非法 JSON——模型既看不全，也没法
    // 当 JSON 读。给个窗口 + 总数，它自己会收窄（指定 class、或调小范围）。
    if class_filter.is_none() {
        let take = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|value| (value as usize).clamp(1, CLASS_LIST_MAX))
            .unwrap_or(CLASS_LIST_DEFAULT);
        let shown: Vec<String> = class_names
            .iter()
            .take(take)
            .map(|(_, name)| name.clone())
            .collect();
        let listed = shown.len();
        return respond(
            ctx,
            "dex_disasm",
            json!({
                "path": path.display().to_string(),
                "size": size,
                "classCount": class_count,
                "listed": listed,
                "truncated": class_names.len() > listed,
                "classes": shown,
                "note": format!(
                    "共 {} 个类，这里列前 {listed} 个；要更多调 limit，或直接填 class 反汇编指定类。",
                    class_names.len()
                ),
            }),
        );
    }

    // 找到目标类的 class_def 下标（用收集时记下的真实下标，不是列表位置）。
    let target_index = class_names
        .iter()
        .find(|(_, name)| name == class_filter.unwrap())
        .map(|(index, _)| *index);
    let Some(target_index) = target_index else {
        return ToolResult::fail(format!(
            "找不到类 {}；用不填 class 的方式列全部类名",
            class_filter.unwrap()
        ));
    };

    // 收集该类全部有 code_item 的方法。
    let mut methods: Vec<Value> = Vec::new();
    let mut total = 0usize;
    let mut decode_fail = 0usize;
    // 反汇编结果的字符预算。必须比出口闸（4 万）小一截，否则被出口闸砍成
    // 半截 JSON——那是模型解析不了的（实测一个类 20 个方法就撞上了）。
    let mut used = 0usize;
    let mut trimmed = false;
    let mut collect = |dex: &Dex, class_def_index: u32| -> Result<(), DexError> {
        dex.each_method(class_def_index, |method_idx, access, code_off| {
            total += 1;
            let name = dex.method_ref(method_idx)?;
            if let Some(filter) = method_filter {
                if !name.contains(filter) {
                    return Ok(());
                }
            }
            match decode_code_item(dex.bytes, code_off, limit) {
                Ok((registers, ins_size, insns, _)) => {
                    let instructions: Vec<Value> = insns
                        .iter()
                        .map(|insn| {
                            let mut item = json!({ "insn": insn.mnemonic });
                            // invoke 补上目标方法全名。只有 meth@idx 时，读的人
                            // 还得自己去查 method_id 表——那就不叫代码级证据了。
                            for reference in &insn.refs {
                                if let Ref::Method(idx) = reference {
                                    if let Ok(target) = dex.method_ref(*idx) {
                                        item["target"] = json!(target);
                                    }
                                }
                            }
                            item
                        })
                        .collect();
                    let item = json!({
                        "method": name,
                        "access": format!("0x{access:x}"),
                        "code_off": format!("0x{code_off:x}"),
                        "registers": registers,
                        "ins_size": ins_size,
                        "insns": instructions.len(),
                        "instructions": instructions,
                        "decode_ok": true,
                    });
                    // 按字符预算收口，而不是靠出口闸砍。出口闸砍的是**已经序列化
                    // 好的 JSON**，砍完就是半截非法 JSON，模型解析不了。
                    //
                    // 大小按 pretty 算：最终输出走 to_string_pretty，缩进和换行能
                    // 让体积翻倍，按紧凑格式估会低估一大截。
                    let size = serde_json::to_string_pretty(&item)
                        .map(|text| text.chars().count())
                        .unwrap_or(0);
                    if !methods.is_empty() && used + size > DISASM_BUDGET {
                        trimmed = true;
                        return Ok(());
                    }
                    used += size;
                    methods.push(item);
                }
                Err(_) => {
                    decode_fail += 1;
                    methods.push(json!({
                        "method": name,
                        "code_off": format!("0x{code_off:x}"),
                        "decode_ok": false,
                    }));
                }
            }
            Ok(())
        })
    };
    if let Err(error) = collect(&dex, target_index as u32) {
        return ToolResult::fail(format!("解析类数据失败：{error}"));
    }

    respond(
        ctx,
        "dex_disasm",
        json!({
            "path": path.display().to_string(),
            "size": size,
            "class": class_filter,
            // 原来这里写的是 "methods"，和下面的数组同名，数量被数组覆盖了。
            "methodCount": methods.len(),
            "methodTotal": total,
            "decodeFail": decode_fail,
            "truncated": trimmed,
            "methods": methods,
            "note": if trimmed {
                "方法太多，只给了装得下的前几个；用 method 参数过滤具体方法。"
            } else {
                ""
            },
        }),
    )
}

fn run_xref(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let (path, size) = match target_of(ctx, args) {
        Ok(found) => found,
        Err(failure) => return failure,
    };
    let class_filter = args.get("class").and_then(Value::as_str);
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map(|value| (value as usize).clamp(1, XREF_MAX))
        .unwrap_or(XREF_DEFAULT);

    let bytes = match read_head(&path, DEX_LIMIT) {
        Ok(bytes) => bytes,
        Err(error) => return ToolResult::fail(error),
    };
    let dex = match open_dex(&bytes) {
        Ok(dex) => dex,
        Err(failure) => return failure,
    };

    let class_count = dex.sections.class_defs.0;

    // 同一个自检：解析器读错位置时 xref 也会安静地返回空（报告里正是如此：
    // `calls/classes/fields/strings` 全空、`classes: 2685886`）。先探一遍类名。
    let mut harvested = 0usize;
    for i in 0..class_count.min(5000) {
        if dex.class_name(i).is_ok() {
            harvested += 1;
        }
    }
    if let Err(why) = check_class_harvest(class_count, harvested) {
        return ToolResult::fail(why);
    }

    // 决定扫描范围：指定 class 只扫一个，否则全量（有上限）。
    let range: Vec<u32> = if let Some(filter) = class_filter {
        let mut found = None;
        for i in 0..class_count.min(5000) {
            if let Ok(name) = dex.class_name(i) {
                if name == filter {
                    found = Some(i);
                    break;
                }
            }
        }
        match found {
            Some(i) => vec![i],
            None => {
                return ToolResult::fail(format!("找不到类 {filter}；不填 class 扫全部类"));
            }
        }
    } else {
        (0..class_count.min(5000)).collect()
    };

    let mut calls: Vec<Value> = Vec::new();
    let mut strings: Vec<Value> = Vec::new();
    let mut types: Vec<Value> = Vec::new();
    let mut fields: Vec<Value> = Vec::new();
    let mut switches: Vec<Value> = Vec::new();

    for class_def_index in range {
        let _ = dex.each_method(class_def_index, |method_idx, _access, code_off| {
            let method_name = match dex.method_ref(method_idx) {
                Ok(name) => name,
                Err(_) => return Ok(()),
            };
            let insns = match decode_code_item(dex.bytes, code_off, INSN_MAX) {
                Ok((_, _, insns, _)) => insns,
                Err(_) => return Ok(()),
            };
            for insn in insns {
                for reference in &insn.refs {
                    match reference {
                        Ref::Method(idx) => {
                            if calls.len() < limit {
                                if let Ok(target) = dex.method_ref(*idx) {
                                    calls.push(json!({
                                        "from": method_name,
                                        "to": target,
                                        "op": "invoke",
                                    }));
                                }
                            }
                        }
                        Ref::String(idx) => {
                            if strings.len() < limit {
                                if let Ok(value) = dex.string(*idx) {
                                    strings.push(json!({ "from": method_name, "string": value }));
                                }
                            }
                        }
                        Ref::Type(idx) => {
                            if types.len() < limit {
                                if let Ok(value) = dex.type_descriptor(*idx) {
                                    types.push(json!({ "from": method_name, "type": value }));
                                }
                            }
                        }
                        Ref::Field(idx) => {
                            if fields.len() < limit {
                                if let Ok(value) = dex.field_ref(*idx) {
                                    fields.push(json!({ "from": method_name, "field": value }));
                                }
                            }
                        }
                        Ref::Switch(data_off) => {
                            if switches.len() < limit {
                                if let Ok(keys) = switch_keys(dex.bytes, *data_off) {
                                    switches.push(json!({
                                        "method": method_name,
                                        "keys": keys,
                                    }));
                                }
                            }
                        }
                        Ref::CustomCallSite => {}
                    }
                }
            }
            Ok(())
        });
    }

    respond(
        ctx,
        "dex_xref",
        json!({
            "path": path.display().to_string(),
            "size": size,
            "classes": class_count,
            "scope": class_filter,
            "calls": calls,
            "strings": strings,
            "types": types,
            "fields": fields,
            "switches": switches,
        }),
    )
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uleb_reads_single_and_multi_byte() {
        let bytes = [0x7f, 0x80, 0x01, 0x00];
        assert_eq!(uleb(&bytes, 0).unwrap(), (0x7f, 1));
        assert_eq!(uleb(&bytes, 1).unwrap(), (0x80, 3));
        assert_eq!(uleb(&bytes, 2).unwrap(), (1, 3));
        assert_eq!(uleb(&bytes, 3).unwrap(), (0, 4));
    }

    #[test]
    fn uleb_overflows_after_five_bytes() {
        let bytes = [0x80, 0x80, 0x80, 0x80, 0x80];
        assert_eq!(uleb(&bytes, 0), Err(DexError::UlebOverflow));
    }

    #[test]
    fn uleb_reports_truncation() {
        let bytes = [0x80];
        assert_eq!(uleb(&bytes, 0), Err(DexError::Truncated));
    }

    #[test]
    fn header_rejects_non_dex_and_cdex() {
        assert_eq!(Sections::parse(b"not a dex"), Err(DexError::NotDex));
        assert_eq!(Sections::parse(b"cdex"), Err(DexError::CompactDex));
        assert_eq!(Sections::parse(&[0u8; 8]), Err(DexError::NotDex));
    }

    /// 手工构造一个最小 DEX，覆盖 class_data_item 的 field 跳过（上一轮 P0 回归）。
    ///
    /// 布局：header(0x70) 后依次排 string/type/proto/field/method 池与 class_defs，
    /// 数据区放一个带 static_field 的 class_data_item + 一个只有 return-void 的 code_item。
    #[test]
    fn class_data_item_skips_fields_and_finds_code() {
        let mut d = vec![0u8; 0x400];
        d[..8].copy_from_slice(b"dex\n035\0");

        // 各 section 的表头：数量 + 偏移（偏移都指向 0x400 之后的数据区，逐步布局）。
        // 这里为了可读性，直接硬编码一个具体布局。
        //
        // string 池：一个字符串 "V"（返回类型）。
        // type 池：type[0] = "V"。
        // method 池：method[0] = class 0, proto 0, name 0（随便，只要能被 method_ref 读）。
        //
        // 为把测试做小，本测试只验证「field 跳过」这一条关键路径：
        // 给定 sf=1、inf=0、dm=1，code_item 里只有 return-void。

        // 表头偏移一律按 DEX 规范写（不是按实现里的常量写）：
        // 实现读错位置时这里就会红，而不是跟着一起错。
        //
        // string_ids: 1 个，off = 0x100
        write_u32(&mut d, 0x38, 1); // string_ids_size
        write_u32(&mut d, 0x3c, 0x100); // string_ids_off
        // string_data 在 0x200：len=1 'V' \0
        write_u32(&mut d, 0x100, 0x200);
        d[0x200] = 1; // uleb len 1
        d[0x201] = b'V';
        d[0x202] = 0;

        // type_ids: 1 个，off=0x104
        write_u32(&mut d, 0x40, 1);
        write_u32(&mut d, 0x44, 0x104);
        write_u32(&mut d, 0x104, 0); // descriptor_idx -> string 0

        // proto_ids: 1 个，off=0x108（shorty_idx, return_type_idx, parameters_off）
        write_u32(&mut d, 0x48, 1);
        write_u32(&mut d, 0x4c, 0x108);
        write_u32(&mut d, 0x108, 0); // shorty_idx
        write_u32(&mut d, 0x10c, 0); // return_type_idx -> type 0
        write_u32(&mut d, 0x110, 0); // parameters_off = 0（无参）

        // field_ids: 1 个，off=0x114
        write_u32(&mut d, 0x50, 1);
        write_u32(&mut d, 0x54, 0x114);
        write_u16(&mut d, 0x114, 0); // class_idx
        write_u16(&mut d, 0x116, 0); // type_idx
        write_u32(&mut d, 0x118, 0); // name_idx -> string 0

        // method_ids: 1 个，off=0x11c
        write_u32(&mut d, 0x58, 1);
        write_u32(&mut d, 0x5c, 0x11c);
        write_u16(&mut d, 0x11c, 0); // class_idx
        write_u16(&mut d, 0x11e, 0); // proto_idx
        write_u32(&mut d, 0x120, 0); // name_idx

        // class_defs: 1 个，off=0x124
        write_u32(&mut d, 0x60, 1);
        write_u32(&mut d, 0x64, 0x124);
        // class_def_item[0]（32 字节）：class_idx=0，class_data_off=0x300
        write_u32(&mut d, 0x124, 0); // class_idx
        write_u32(&mut d, 0x124 + 0x18, 0x300); // class_data_off

        // data 区：class_data_item @ 0x300
        // sf=1, inf=0, dm=1, vm=0
        // encoded_field: field_idx_diff=0, access_flags=1
        // encoded_method: method_idx_diff=0, access_flags=1, code_off=0x320
        d[0x300] = 1; // sf
        d[0x301] = 0; // inf
        d[0x302] = 1; // dm
        d[0x303] = 0; // vm
        d[0x304] = 0; // field_idx_diff
        d[0x305] = 1; // access_flags
        d[0x306] = 0; // method_idx_diff
        d[0x307] = 1; // method access_flags
        d[0x308] = 0xa0; // code_off uleb: 0x320 = 800 = 0b1100100000 -> 低7位 0x20|0x80=0xa0
        d[0x309] = 0x06; // 高 7 位 800>>7 = 6

        // code_item @ 0x320：registers=1, ins=1, insns_size=1，一条 return-void(0x0e)
        write_u16(&mut d, 0x320, 1); // registers_size
        write_u16(&mut d, 0x322, 1); // ins_size
        write_u16(&mut d, 0x324, 0); // outs_size
        write_u16(&mut d, 0x326, 0); // tries_size
        write_u32(&mut d, 0x328, 0); // debug_info_off
        write_u32(&mut d, 0x32c, 1); // insns_size
        write_u16(&mut d, 0x330, 0x0e); // return-void

        let dex = Dex::parse(&d).unwrap();
        let mut found_code = 0;
        let mut method_idx_seen = 0u32;
        dex.each_method(0, |method_idx, _access, code_off| {
            found_code += 1;
            method_idx_seen = method_idx;
            assert_eq!(code_off, 0x320, "code_off 应指向 code_item");
            Ok(())
        })
        .unwrap();
        // 关键断言：跳过 field 后，method 条目的 code_off 才能正确读成 0x320。
        // 若漏跳 field，会读到垃圾值（上一轮 P0 的 code_off=1）。
        assert_eq!(found_code, 1);
        assert_eq!(method_idx_seen, 0);

        // IR 摘要走同一条路径：va 用 code_off，size 是 insns_size * 2。
        let found = methods(&d).expect("IR 摘要应能解析");
        assert_eq!(found.skipped_classes, 0);
        assert_eq!(found.methods.len(), 1);
        assert_eq!(found.methods[0].name, "V->V");
        assert_eq!(found.methods[0].va, 0x320);
        assert_eq!(found.methods[0].size, 2);
    }

    #[test]
    fn decode_code_item_reads_return_void() {
        let mut d = vec![0u8; 0x40];
        write_u16(&mut d, 0x00, 1); // registers
        write_u16(&mut d, 0x02, 1); // ins
        write_u16(&mut d, 0x04, 0); // outs
        write_u16(&mut d, 0x06, 0); // tries
        write_u32(&mut d, 0x08, 0); // debug
        write_u32(&mut d, 0x0c, 1); // insns_size
        write_u16(&mut d, 0x10, 0x0e); // return-void

        let (registers, ins_size, insns, _) = decode_code_item(&d, 0, 10).unwrap();
        assert_eq!(registers, 1);
        assert_eq!(ins_size, 1);
        assert_eq!(insns.len(), 1);
        assert_eq!(insns[0].mnemonic, "return-void");
    }

    #[test]
    fn switch_keys_packed_and_sparse() {
        let mut packed = vec![0u8; 32];
        write_u16(&mut packed, 0, 0x0100); // packed-switch ident
        write_u16(&mut packed, 2, 3); // size
        write_u32(&mut packed, 4, 0x10); // first_key
        assert_eq!(switch_keys(&packed, 0).unwrap(), vec![0x10, 0x11, 0x12]);

        let mut sparse = vec![0u8; 32];
        write_u16(&mut sparse, 0, 0x0200);
        write_u16(&mut sparse, 2, 2);
        write_u32(&mut sparse, 4, 5);
        write_u32(&mut sparse, 8, 9);
        assert_eq!(switch_keys(&sparse, 0).unwrap(), vec![5, 9]);
    }

    #[test]
    fn dex_tools_are_read_only_static() {
        for tool in tools() {
            assert_eq!(tool.meta.risk, Risk::Read, "{} 不该写盘", tool.name);
            assert_eq!(tool.meta.scope, ToolScope::Any);
            assert_eq!(tool.meta.tags, vec!["bin:static"]);
            assert_eq!(tool.permission, Permission::ReadOnly);
            assert_eq!(tool.meta.network, Network::None);
        }
    }

    /// 真文件回归：拿一个真实 APK 解包的 classes.dex 跑全量解析，断言
    /// `code_off != 0` 的方法里 decode 成功占比 > 99%（上一轮 Python 实测 99.98%）。
    ///
    /// 默认忽略（依赖本地样本路径），手动跑：
    /// `cargo test --lib -- --ignored dex_decodes_real_sample`.
    #[test]
    #[ignore]
    fn dex_decodes_real_sample() {
        let path = real_dex_path();
        let bytes = read_head(std::path::Path::new(path), DEX_LIMIT).unwrap();
        let dex = Dex::parse(&bytes).expect("真实样本应能解析");
        let class_count = dex.sections.class_defs.0;
        // 这个样本的 header 写着 3300。数量级断言能立刻抓出"偏移读错"：
        // 读成别的字段时不是 0 就是六位数以上。
        assert!(
            (1_000..=100_000).contains(&class_count),
            "class_defs_size 读出 {class_count}，不像真实类数（期望量级 3300）"
        );

        let mut with_code = 0usize;
        let mut decoded = 0usize;
        for i in 0..class_count {
            let _ = dex.each_method(i, |_idx, _access, code_off| {
                with_code += 1;
                if decode_code_item(dex.bytes, code_off, 16).is_ok() {
                    decoded += 1;
                }
                Ok(())
            });
        }
        // 类名也必须读得出来。报告里另一个症状是"按类名查永远找不到类"，
        // 根因同样是偏移错位——class_defs 落在 string_ids 数组上，读出来是
        // 一堆字符串偏移，自然一个类都对不上。
        let names: Vec<String> = (0..class_count)
            .filter_map(|i| dex.class_name(i).ok())
            .collect();
        assert!(
            names
                .iter()
                .any(|name| name.contains("orchard/tv/guard/HomeGuardService")),
            "应当能读到 HomeGuardService 的描述符（共读出 {} 个类名）",
            names.len()
        );
        let rate = decoded as f64 / with_code.max(1) as f64;
        assert!(
            rate > 0.99,
            "decode 成功率 {:.2}% 应 > 99%（with_code={}, decoded={}）",
            rate * 100.0,
            with_code,
            decoded
        );
    }

    /// 端到端自测：从真实 APK 解出的 classes.dex 走**工具入口**（不是内部函数），
    /// 验证 `bin.dex.disasm` 能列类、能反汇编指定类，`bin.dex.xref` 能出调用。
    ///
    /// 手动跑（**纯本地，不联网、不花 API 钱**）：
    /// `cargo test --lib -- --ignored apk_end_to_end --nocapture`
    #[test]
    #[ignore]
    fn apk_end_to_end_disasm_and_xref() {
        use crate::engines::native::tools::{run_with_args, EnabledTools, ToolContext};

        let dir = "/Users/edwa2d4th/Desktop/未命名文件夹 7";
        let dex_rel = "unpacked/classes.dex";
        let dex_abs = format!("{dir}/{dex_rel}");
        assert!(
            std::path::Path::new(&dex_abs).exists(),
            "样本不在 {dex_abs}：换样本后请同步改这里"
        );
        let enabled = EnabledTools::all();
        let stop = || false;
        let ctx = ToolContext {
            app: None,
            session_id: "selftest",
            directory: dir,
            call_id: "c1",
            provider: None,
            model: None,
            permission: Some("read-only"),
            selection: None,
            enabled: &enabled,
            dag: None,
            should_stop: &stop,
        };

        // 1) 列类：header 里 3300 个，必须真列得出来。
        let listed = run_with_args(&ctx, "bin.dex.disasm", &serde_json::json!({ "path": dex_rel }));
        assert!(
            listed.outcome.error.is_none(),
            "列类失败：{:?}",
            listed.outcome.error
        );
        let payload: Value = serde_json::from_str(&listed.outcome.text).expect("列类结果应是 JSON");
        assert_eq!(
            payload["classCount"].as_u64(),
            Some(3300),
            "classCount 应当是 header 里的 3300：{payload}"
        );
        // 清单必须有界：三千多个类全塞进来会超出口闸上限，被截成非法 JSON。
        let shown = payload["listed"].as_u64().unwrap_or(0);
        assert_eq!(shown, CLASS_LIST_DEFAULT as u64, "默认只列前 200 个类");
        assert_eq!(
            payload["truncated"].as_bool(),
            Some(true),
            "还有更多类时必须标 truncated"
        );

        // 2) 反汇编一个已知类：方法体与指令都要出来。
        let target = "Lapp/orchard/tv/guard/HomeGuardService;";
        let dis = run_with_args(
            &ctx,
            "bin.dex.disasm",
            &serde_json::json!({ "path": dex_rel, "class": target, "limit": 40 }),
        );
        assert!(
            dis.outcome.error.is_none(),
            "反汇编失败：{:?}",
            dis.outcome.error
        );
        let body: Value = serde_json::from_str(&dis.outcome.text).expect("反汇编结果应是 JSON");
        let methods = body["methods"].as_array().expect("应当有 methods 数组");
        assert!(!methods.is_empty(), "HomeGuardService 应当有方法体：{body}");
        assert!(
            methods.iter().any(|item| item["decode_ok"] == true),
            "至少要有方法能成功解码指令：{body}"
        );

        // 3) 调用图：这个类至少要解出几条调用。
        let xref = run_with_args(
            &ctx,
            "bin.dex.xref",
            &serde_json::json!({ "path": dex_rel, "class": target, "limit": 20 }),
        );
        assert!(
            xref.outcome.error.is_none(),
            "xref 失败：{:?}",
            xref.outcome.error
        );
        let graph: Value = serde_json::from_str(&xref.outcome.text).expect("xref 结果应是 JSON");
        let calls = graph["calls"].as_array().map(Vec::len).unwrap_or(0);
        assert!(calls > 0, "HomeGuardService 应当有调用边：{graph}");

        println!(
            "端到端自测通过：类 {} 个、HomeGuardService 方法 {} 个、调用边 {} 条",
            shown,
            methods.len(),
            calls
        );
    }

    /// 核验那份 APK 报告里的关键事实（只做对照，不改报告）。
    ///
    /// `cargo test --lib -- --ignored audit_report_claims --nocapture`
    #[test]
    #[ignore]
    fn audit_report_claims() {
        let path = real_dex_path();
        let bytes = read_head(std::path::Path::new(path), DEX_LIMIT).unwrap();
        let dex = Dex::parse(&bytes).expect("真实样本应能解析");

        // 报告 4.2 那张表：method_id → 签名。对上了，第 4 章的"crypto 调用面
        // 只有 8 个 API"才有据；对不上，整章的否定结论都要重做。
        println!("--- 报告 4.2 的 method_id 对照 ---");
        for id in [8728u32, 8729, 12743, 12744, 12745, 13255, 13256, 13257] {
            match dex.method_ref(id) {
                Ok(name) => println!("method[{id}] = {name}"),
                Err(error) => println!("method[{id}] 读取失败：{error}"),
            }
        }

        // 报告 4.3 的唯一候选调用点：`LW/a;` 的 g 方法。
        println!("--- 报告 4.3 的候选类 ---");
        let mut found = None;
        for i in 0..dex.sections.class_defs.0 {
            if let Ok(name) = dex.class_name(i) {
                if name == "LW/a;" {
                    found = Some(i);
                    break;
                }
            }
        }
        println!("LW/a; 的 class_def 下标 = {found:?}");
        if let Some(index) = found {
            let _ = dex.each_method(index, |method_idx, _access, code_off| {
                let name = dex.method_ref(method_idx).unwrap_or_else(|_| String::new());
                // 报告 4.3 点名的就是 g 方法。
                if !name.ends_with("->g") {
                    return Ok(());
                }
                let Ok((_, _, insns, _)) = decode_code_item(dex.bytes, code_off, 4000) else {
                    println!("  g @0x{code_off:x} 解码失败");
                    return Ok(());
                };
                println!(
                    "  g @0x{code_off:x}：{} 条指令，指令起点 0x{:x}",
                    insns.len(),
                    code_off + 16
                );
                for (i, insn) in insns.iter().enumerate() {
                    if !insn.mnemonic.starts_with("invoke") {
                        continue;
                    }
                    let target = insn.refs.iter().find_map(|reference| {
                        if let Ref::Method(idx) = reference {
                            dex.method_ref(*idx).ok()
                        } else {
                            None
                        }
                    });
                    // 报告用「code_item 内单元偏移」计数：指令起点 + i*2 字节。
                    let at = code_off + 16 + i as u32 * 2;
                    println!("    单元 {i:>3}  文件偏移 0x{at:x}  {} → {:?}", insn.mnemonic, target);
                }
                Ok(())
            });
        }
    }

    /// 指令对齐回归：解码消耗的 unit 必须**恰好等于** insns_size。
    ///
    /// 这是比「最后一条必须是 return」更硬的校验——有 try/catch 的方法，
    /// `insns` 数组末尾是 handler 块，末条可能是 `goto`（合法），用 return
    /// 判定会误报 6%。而游标对不上就一定是某条指令的长度写错了。
    ///
    /// `cargo test --lib -- --ignored every_method_consumes_exactly --nocapture`
    #[test]
    #[ignore]
    fn every_method_consumes_exactly_its_insns() {
        let path = real_dex_path();
        let bytes = read_head(std::path::Path::new(path), DEX_LIMIT).unwrap();
        let dex = Dex::parse(&bytes).expect("真实样本应能解析");
        let class_count = dex.sections.class_defs.0;

        let mut good = 0usize;
        let mut bad = 0usize;
        let mut truncated = 0usize;
        let mut samples: Vec<String> = Vec::new();
        for i in 0..class_count {
            let _ = dex.each_method(i, |method_idx, _access, code_off| {
                let Ok((_, _, insns, end_unit)) = decode_code_item(dex.bytes, code_off, 40000) else {
                    return Ok(());
                };
                let insns_size =
                    read_u32(dex.bytes, code_off as usize + 0x0c, true).unwrap_or(0) as usize;
                // 撞到上限说明是被截断，不是错位。
                if insns.len() >= 40000 {
                    truncated += 1;
                    return Ok(());
                }
                if end_unit == insns_size {
                    good += 1;
                } else {
                    bad += 1;
                    if samples.len() < 8 {
                        let name = dex
                            .method_ref(method_idx)
                            .unwrap_or_else(|_| format!("method[{method_idx}]"));
                        samples.push(format!(
                            "{name} @0x{code_off:x} 游标停 {end_unit} / insns_size {insns_size}（{} 条）",
                            insns.len()
                        ));
                    }
                }
                Ok(())
            });
        }
        println!(
            "游标精确 {good} / 偏离 {bad} / 截断跳过 {truncated}（共 {} 个方法）",
            good + bad
        );
        for sample in &samples {
            println!("  偏离样例：{sample}");
        }
        assert_eq!(bad, 0, "有 {bad} 个方法的指令游标没有走到 insns_size");
    }

    /// 真实样本的 classes.dex 路径。
    ///
    /// 解包目录会随每次运行变（`work/apk/x` → `unpacked` → …），所以给几个
    /// 候选**按顺序找**。都找不到就直接 panic：以前这里写死一个路径 + 找不到
    /// 就 return，测试「永远通过」，把 header 偏移整体错位藏了整轮。
    fn real_dex_path() -> &'static str {
        const CANDIDATES: &[&str] = &[
            "/Users/edwa2d4th/Desktop/未命名文件夹 7/unpacked/classes.dex",
            "/Users/edwa2d4th/Desktop/未命名文件夹 7/work/apk/x/classes.dex",
        ];
        CANDIDATES
            .iter()
            .copied()
            .find(|path| std::path::Path::new(path).exists())
            .unwrap_or_else(|| panic!("真实样本不在候选路径里（换样本请更新 CANDIDATES）"))
    }

    fn write_u16(bytes: &mut [u8], at: usize, value: u16) {
        bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }
    fn write_u32(bytes: &mut [u8], at: usize, value: u32) {
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
}
