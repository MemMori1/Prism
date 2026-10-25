//! APK 二进制清单解析：`bin.apk.manifest`。
//!
//! 回答的是「这个包自称是谁、要什么权限、哪些组件对着外面开门」——包名 / 版本 /
//! minSdk / 权限 / exported 组件 / `debuggable` 这些安装前就能确定的事实。
//! 不回答「有没有恶意行为」：那要看代码与运行时，清单只提供入口与授权面。
//!
//! 只读二进制 AXML（`AndroidManifest.xml` 在 APK 里是编译过的），不碰
//! `resources.arsc`：`@0x7f0c0001` 这类引用原样输出，不猜它指向哪个字符串。
//! 猜错一个应用名是小事，猜错一个 `exported` 或缺省值就会误导风险定级。
//!
//! 三条纪律与同目录其它工具一致：
//! - 解析不出来的字段省略，不补默认值（`exported` 缺省由系统按 targetSdk 与
//!   intent-filter 推断，这里只报「没声明」）；
//! - 所有偏移都做边界检查，畸形输入返回错误而不是 panic；
//! - 输出是**事实**，`facts` 里只写清单显式声明的内容，不做风险评分。

use serde_json::{json, Map, Value};

use super::apkzip;
use super::forensics::{read_head, respond, target_of};
use super::tools::{
    builtin_full, Network, Permission, Risk, ToolContext, ToolDef, ToolMeta, ToolResult, ToolScope,
};

/// 读整个 APK 的上限：清单在文件头部，但中央目录在尾部，必须能定位到两端。
const APK_LIMIT: u64 = 256 * 1024 * 1024;
/// 清单解压后的上限。真实 APK 的 AXML 通常 20–200 KB，16 MiB 已经宽到离谱。
const AXML_LIMIT: usize = 16 * 1024 * 1024;
/// 元素 / 属性 / 字符串池的硬上限：畸形输入不能把执行线程拖死。
const ELEMENT_LIMIT: usize = 20_000;
const DEPTH_LIMIT: usize = 64;
const ATTR_LIMIT: usize = 512;
const STRING_LIMIT: usize = 100_000;
const STRING_CHARS_LIMIT: usize = 8_192;
const RESOURCE_MAP_LIMIT: usize = 4_096;
/// 输出上限：权限与组件都可能有几百条，超出只保留前 N 条并标注。
const PERMISSION_LIMIT: usize = 300;
const COMPONENT_LIMIT: usize = 200;
const FILTER_LIMIT: usize = 8;
const ACTION_LIMIT: usize = 12;
const FACT_LIMIT: usize = 60;

pub fn tools() -> Vec<std::sync::Arc<ToolDef>> {
    vec![builtin_full(
        "bin.apk.manifest",
        "APK 清单解析",
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
    )]
}

fn schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "bin.apk.manifest",
            "description": "解析 APK 的二进制 AndroidManifest.xml：包名、版本、minSdk / targetSdk、权限、uses-feature、application 标志（debuggable / allowBackup / cleartext / 网络配置）、四类组件与 intent-filter，以及清单显式声明的风险事实。只读结构，不执行样本；@0x… 是资源引用，未解析 resources.arsc。",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "相对工作目录的 APK 路径" }
                },
                "required": ["path"]
            }
        }
    })
}

fn run(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let (path, size) = match target_of(ctx, args) {
        Ok(found) => found,
        Err(failure) => return failure,
    };
    if size > APK_LIMIT {
        return ToolResult::fail(format!(
            "{} 有 {} MiB，超过本工具 {} MiB 的读取上限；请先裁剪样本，或解包后单独提供 AndroidManifest.xml。",
            path.display(),
            size / (1024 * 1024),
            APK_LIMIT / (1024 * 1024)
        ));
    }
    let blob = match read_head(&path, APK_LIMIT) {
        Ok(bytes) => bytes,
        Err(error) => return ToolResult::fail(error),
    };
    if blob.len() as u64 != size {
        return ToolResult::fail(format!(
            "只读到 {} / {size} 字节，数据不完整；拒绝在截断的 APK 上解析清单",
            blob.len()
        ));
    }
    let mut payload = match inspect(&blob) {
        Ok(payload) => payload,
        Err(error) => return ToolResult::fail(format!("APK 清单解析失败：{error}")),
    };
    payload["path"] = json!(path.display().to_string());
    payload["size"] = json!(size);
    respond(ctx, "manifest", payload)
}

/// 纯函数入口：输入完整 APK 字节，输出清单摘要。单测直接吃构造的 zip。
pub fn inspect(blob: &[u8]) -> Result<Value, String> {
    let layout = apkzip::layout(blob)?;
    let entry = layout
        .entry("AndroidManifest.xml")
        .ok_or_else(|| "APK 里没有 AndroidManifest.xml 条目（不是有效 APK？）".to_string())?;
    let axml = apkzip::read_entry(blob, entry, AXML_LIMIT)?;
    if axml.starts_with(b"<") {
        return Err(
            "AndroidManifest.xml 是明文 XML（没有被 aapt 编译成 AXML），本工具只解析二进制清单；\
             可以解包后用 read_file 查看"
                .to_string(),
        );
    }
    let root = parse(&axml)?;
    if root.name != "manifest" {
        return Err(format!("根元素是 <{}>，不是 <manifest>", root.name));
    }
    Ok(interpret(&root))
}

// ---------------------------------------------------------------- AXML

const CHUNK_FILE: u16 = 0x0003;
const CHUNK_STRING_POOL: u16 = 0x0001;
const CHUNK_RESOURCE_MAP: u16 = 0x0180;
const CHUNK_START_NAMESPACE: u16 = 0x0100;
const CHUNK_START_ELEMENT: u16 = 0x0102;
const CHUNK_END_ELEMENT: u16 = 0x0103;
const NO_ENTRY: u32 = 0xffff_ffff;

const ANDROID_NS: &str = "http://schemas.android.com/apk/res/android";

/// 一个解析后的 XML 元素。属性已经换成本地名（android 命名空间去掉前缀），
/// 但保留资源 ID：有的清单把属性名字符串删了/改了，ID 才是结构事实。
#[derive(Debug, Default)]
struct Element {
    name: String,
    attrs: Vec<Attr>,
    children: Vec<Element>,
}

#[derive(Debug)]
struct Attr {
    ns: String,
    name: String,
    rid: Option<u32>,
    value: AttrValue,
}

#[derive(Debug, Clone, PartialEq)]
enum AttrValue {
    Text(String),
    Int(i64),
    Bool(bool),
    /// 类型认识但没法简单表达（颜色 / 尺寸 / 未知类型），保留可读的原始表示。
    Other(String),
}

impl Element {
    /// 按规范化键取属性：android 属性优先用资源 ID 认，其次才看名字。
    fn attr(&self, key: &str) -> Option<&AttrValue> {
        self.attrs
            .iter()
            .find(|attr| attr.key() == key)
            .map(|attr| &attr.value)
    }

    fn text(&self, key: &str) -> Option<String> {
        self.attr(key).and_then(AttrValue::as_text)
    }

    fn int(&self, key: &str) -> Option<i64> {
        self.attr(key).and_then(AttrValue::as_int)
    }

    fn boolean(&self, key: &str) -> Option<bool> {
        self.attr(key).and_then(AttrValue::as_bool)
    }

    fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |child| child.name == name)
    }
}

impl Attr {
    fn key(&self) -> String {
        if let Some(rid) = self.rid {
            if let Some(name) = known_attr(rid) {
                return name.to_string();
            }
        }
        if self.ns.is_empty() || self.ns == ANDROID_NS {
            self.name.clone()
        } else if self.ns == "http://schemas.android.com/apk/res-auto" {
            format!("app:{}", self.name)
        } else if self.ns == "http://schemas.android.com/tools" {
            format!("tools:{}", self.name)
        } else {
            // 未知命名空间不猜前缀：完整 URI + 本地名，至少还能人工对回去。
            format!("{}:{}", self.ns, self.name)
        }
    }
}

impl AttrValue {
    fn as_text(&self) -> Option<String> {
        match self {
            AttrValue::Text(text) | AttrValue::Other(text) => Some(text.clone()),
            AttrValue::Int(value) => Some(value.to_string()),
            AttrValue::Bool(value) => Some(value.to_string()),
        }
    }

    fn as_int(&self) -> Option<i64> {
        match self {
            AttrValue::Int(value) => Some(*value),
            AttrValue::Bool(value) => Some(*value as i64),
            AttrValue::Text(text) | AttrValue::Other(text) => text.parse::<i64>().ok(),
        }
    }

    fn as_bool(&self) -> Option<bool> {
        match self {
            AttrValue::Bool(value) => Some(*value),
            AttrValue::Int(value) => Some(*value != 0),
            AttrValue::Text(text) | AttrValue::Other(text) => match text.as_str() {
                "true" | "1" => Some(true),
                "false" | "0" => Some(false),
                _ => None,
            },
        }
    }
}

/// 属性资源 ID → 规范化名字。只收录要读取/输出的字段；不在表里的继续用
/// 字符串池里的名字，仍然能读到，只是没法在名字被抹掉时救回来。
fn known_attr(rid: u32) -> Option<&'static str> {
    Some(match rid {
        0x0101_0000 => "theme",
        0x0101_0001 => "label",
        0x0101_0002 => "icon",
        0x0101_0003 => "name",
        0x0101_0006 => "permission",
        0x0101_0009 => "protectionLevel",
        0x0101_000b => "sharedUserId",
        0x0101_000e => "enabled",
        0x0101_000f => "debuggable",
        0x0101_0010 => "exported",
        0x0101_0011 => "process",
        0x0101_0018 => "authorities",
        0x0101_001b => "grantUriPermissions",
        0x0101_001d => "launchMode",
        0x0101_001e => "screenOrientation",
        0x0101_001f => "configChanges",
        0x0101_0024 => "value",
        0x0101_0025 => "resource",
        0x0101_0026 => "mimeType",
        0x0101_0027 => "scheme",
        0x0101_0028 => "host",
        0x0101_0029 => "port",
        0x0101_002a => "path",
        0x0101_002b => "pathPrefix",
        0x0101_002c => "pathPattern",
        0x0101_020c => "minSdkVersion",
        0x0101_021b => "versionCode",
        0x0101_021c => "versionName",
        0x0101_0270 => "targetSdkVersion",
        0x0101_0271 => "maxSdkVersion",
        0x0101_0272 => "testOnly",
        0x0101_0280 => "allowBackup",
        0x0101_028e => "required",
        0x0101_02b7 => "installLocation",
        0x0101_035a => "supportsRtl",
        0x0101_03af => "fullBackupContent",
        0x0101_04ea => "extractNativeLibs",
        0x0101_04ec => "usesCleartextTraffic",
        0x0101_0505 => "directBootAware",
        0x0101_0527 => "networkSecurityConfig",
        0x0101_0572 => "compileSdkVersion",
        0x0101_0573 => "compileSdkVersionCodename",
        0x0101_0576 => "versionCodeMajor",
        0x0101_057a => "appComponentFactory",
        _ => return None,
    })
}

/// 解析整份 AXML。返回文档根元素。
fn parse(blob: &[u8]) -> Result<Element, String> {
    let (kind, header, size) = chunk_header(blob, 0)?;
    if kind != CHUNK_FILE {
        return Err(format!("不是 AXML：首个块类型是 0x{kind:04x}，应为 0x0003"));
    }
    if header < 8 {
        return Err(format!("文件块 headerSize={header} 小于 8"));
    }
    let end = size.min(blob.len());
    if size > blob.len() {
        return Err(format!(
            "文件块声明 {size} 字节，实际只有 {} 字节（截断）",
            blob.len()
        ));
    }

    let mut pool: Vec<String> = Vec::new();
    let mut resmap: Vec<u32> = Vec::new();
    let mut roots: Vec<Element> = Vec::new();
    let mut stack: Vec<Element> = Vec::new();
    let mut elements = 0usize;
    let mut at = header as usize;
    while at < end {
        let (kind, header, size) = chunk_header(blob, at)?;
        if header < 8 || size < header as usize || at + size > end {
            return Err(format!(
                "块 0x{kind:04x} 在 {at:#x} 处尺寸不自洽（header={header} size={size}）"
            ));
        }
        match kind {
            CHUNK_STRING_POOL => {
                pool = parse_string_pool(blob, at, header, size)?;
            }
            CHUNK_RESOURCE_MAP => {
                resmap = parse_resource_map(blob, at, header, size)?;
            }
            CHUNK_START_NAMESPACE => {
                // 命名空间前缀只影响显示。URI 已经能区分 android / app，前缀
                // 被抹掉也不影响属性归属，所以这里不维护映射表。
            }
            CHUNK_START_ELEMENT => {
                if pool.is_empty() {
                    return Err("元素出现在字符串池之前".to_string());
                }
                elements += 1;
                if elements > ELEMENT_LIMIT {
                    return Err(format!("元素超过 {ELEMENT_LIMIT} 个，拒绝继续解析"));
                }
                if stack.len() >= DEPTH_LIMIT {
                    return Err(format!("元素嵌套超过 {DEPTH_LIMIT} 层"));
                }
                let element = parse_element(blob, at, header, size, &pool, &resmap)?;
                stack.push(element);
            }
            CHUNK_END_ELEMENT => {
                let element = stack
                    .pop()
                    .ok_or_else(|| format!("{at:#x} 的 end element 没有对应的 start"))?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(element),
                    None => roots.push(element),
                }
            }
            _ => {}
        }
        at += size;
    }
    if !stack.is_empty() {
        return Err(format!("有 {} 个元素没有闭合（AXML 截断）", stack.len()));
    }
    match roots.len() {
        0 => Err("AXML 里没有元素".to_string()),
        1 => Ok(roots.remove(0)),
        count => Err(format!("AXML 有 {count} 个根元素")),
    }
}

fn chunk_header(blob: &[u8], at: usize) -> Result<(u16, u16, usize), String> {
    let kind = apkzip::le16(blob, at)?;
    let header = apkzip::le16(blob, at + 2)?;
    let size = apkzip::le32(blob, at + 4)? as usize;
    Ok((kind, header, size))
}

fn parse_string_pool(blob: &[u8], at: usize, header: u16, size: usize) -> Result<Vec<String>, String> {
    if header < 28 {
        return Err(format!("字符串池 headerSize={header} 小于 28"));
    }
    let count = apkzip::le32(blob, at + 8)? as usize;
    let flags = apkzip::le32(blob, at + 16)?;
    let strings_start = apkzip::le32(blob, at + 20)? as usize;
    if count > STRING_LIMIT {
        return Err(format!("字符串池有 {count} 条，超过 {STRING_LIMIT} 上限"));
    }
    let offsets_end = 28usize
        .checked_add(count * 4)
        .ok_or_else(|| "字符串池偏移表长度溢出".to_string())?;
    if offsets_end > size {
        return Err(format!("字符串池偏移表越过块尾（{offsets_end} > {size}）"));
    }
    if strings_start < offsets_end || strings_start > size {
        return Err(format!(
            "字符串池 stringsStart={strings_start} 不在 {offsets_end}..{size} 之间"
        ));
    }
    let utf8 = flags & 0x100 != 0;
    let data_start = at + strings_start;
    let data_end = at + size;
    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        let offset = apkzip::le32(blob, at + 28 + index * 4)? as usize;
        let string_at = data_start
            .checked_add(offset)
            .ok_or_else(|| format!("字符串 {index} 的偏移溢出"))?;
        if string_at >= data_end {
            return Err(format!("字符串 {index} 的偏移越过池尾（{string_at:#x}）"));
        }
        out.push(parse_pool_string(blob, string_at, data_end, utf8, index)?);
    }
    Ok(out)
}

fn parse_pool_string(
    blob: &[u8],
    at: usize,
    end: usize,
    utf8: bool,
    index: usize,
) -> Result<String, String> {
    if utf8 {
        let first = *blob
            .get(at)
            .ok_or_else(|| format!("字符串 {index} 越界"))? as usize;
        let mut cursor = at + 1;
        let length = if first & 0x80 != 0 {
            let second = *blob
                .get(cursor)
                .ok_or_else(|| format!("字符串 {index} 的长度前缀截断"))?
                as usize;
            cursor += 1;
            ((first & 0x7f) << 8) | second
        } else {
            first
        };
        let stop = cursor
            .checked_add(length)
            .ok_or_else(|| format!("字符串 {index} 长度溢出"))?;
        if stop > end {
            return Err(format!("字符串 {index} 越过池尾"));
        }
        let text = String::from_utf8_lossy(&blob[cursor..stop]).to_string();
        check_string_length(index, &text)?;
        Ok(text)
    } else {
        let first = apkzip::le16(blob, at)? as usize;
        let mut cursor = at + 2;
        let length = if first & 0x8000 != 0 {
            let second = apkzip::le16(blob, cursor)? as usize;
            cursor += 2;
            ((first & 0x7fff) << 16) | second
        } else {
            first
        };
        let bytes = length
            .checked_mul(2)
            .ok_or_else(|| format!("字符串 {index} 长度溢出"))?;
        let stop = cursor
            .checked_add(bytes)
            .ok_or_else(|| format!("字符串 {index} 长度溢出"))?;
        if stop > end {
            return Err(format!("字符串 {index} 越过池尾"));
        }
        let units: Vec<u16> = blob[cursor..stop]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let text = String::from_utf16_lossy(&units);
        check_string_length(index, &text)?;
        Ok(text)
    }
}

fn check_string_length(index: usize, text: &str) -> Result<(), String> {
    if text.chars().count() > STRING_CHARS_LIMIT {
        return Err(format!(
            "字符串池第 {index} 条超过 {STRING_CHARS_LIMIT} 字符，拒绝解析"
        ));
    }
    Ok(())
}

fn parse_resource_map(
    blob: &[u8],
    at: usize,
    header: u16,
    size: usize,
) -> Result<Vec<u32>, String> {
    let header = header as usize;
    if header < 8 || header > size {
        return Err(format!("资源映射表 headerSize={header} 不自洽"));
    }
    let count = (size - header) / 4;
    if count > RESOURCE_MAP_LIMIT {
        return Err(format!("资源映射表有 {count} 条，超过上限"));
    }
    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        out.push(apkzip::le32(blob, at + header + index * 4)?);
    }
    Ok(out)
}

fn parse_element(
    blob: &[u8],
    at: usize,
    header: u16,
    size: usize,
    pool: &[String],
    resmap: &[u32],
) -> Result<Element, String> {
    // ResXMLTree_node：8 字节块头 + lineNumber + comment；之后是 ns / name /
    // attributeStart / attributeSize / attributeCount / id / class / style。
    // 元素自身的 ns 只影响前缀显示，这里不读。
    let name_index = apkzip::le32(blob, at + 20)?;
    let name = pool
        .get(name_index as usize)
        .ok_or_else(|| format!("元素名索引 {name_index} 越过字符串池"))?
        .clone();
    let attr_start = apkzip::le16(blob, at + 24)? as usize;
    let attr_size = apkzip::le16(blob, at + 26)? as usize;
    let count = apkzip::le16(blob, at + 28)? as usize;
    if count > ATTR_LIMIT {
        return Err(format!("<{name}> 有 {count} 个属性，超过 {ATTR_LIMIT} 上限"));
    }

    let attrs = if count == 0 {
        Vec::new()
    } else {
        if attr_size < 20 {
            return Err(format!("<{name}> 的 attributeSize={attr_size} 小于 20"));
        }
        // 规范写法：属性区起点 = 节点头 + attributeStart。少数写出器把头长度
        // 与 attributeStart 的定义弄反了，退回到固定偏移 36 再试一次。
        let candidates = [at + header as usize + attr_start, at + 36];
        let base = candidates
            .into_iter()
            .find(|base| {
                let first = base + 20;
                let all = base + count * attr_size;
                all <= at + size
                    && first <= at + size
                    && apkzip::le32(blob, *base + 4)
                        .map(|index| (index as usize) < pool.len())
                        .unwrap_or(false)
            })
            .ok_or_else(|| format!("<{name}> 的属性区越过块尾"))?;

        let mut attrs = Vec::with_capacity(count);
        for index in 0..count {
            let offset = base + index * attr_size;
            let attr_ns = apkzip::le32(blob, offset)?;
            let attr_name_index = apkzip::le32(blob, offset + 4)?;
            let raw = apkzip::le32(blob, offset + 8)?;
            let data_type = blob
                .get(offset + 15)
                .copied()
                .ok_or_else(|| format!("<{name}> 属性 {index} 的值类型越界"))?;
            let data = apkzip::le32(blob, offset + 16)?;
            let attr_name = pool
                .get(attr_name_index as usize)
                .ok_or_else(|| format!("<{name}> 属性 {index} 的名字索引越过字符串池"))?
                .clone();
            let ns = if attr_ns == NO_ENTRY {
                String::new()
            } else {
                pool.get(attr_ns as usize).cloned().unwrap_or_default()
            };
            attrs.push(Attr {
                ns,
                name: attr_name,
                rid: resmap
                    .get(attr_name_index as usize)
                    .copied()
                    .filter(|rid| *rid != 0),
                value: attr_value(pool, raw, data_type, data)?,
            });
        }
        attrs
    };

    Ok(Element {
        name,
        attrs,
        children: Vec::new(),
    })
}

fn attr_value(
    pool: &[String],
    raw: u32,
    data_type: u8,
    data: u32,
) -> Result<AttrValue, String> {
    let raw_text = if raw == NO_ENTRY {
        None
    } else {
        pool.get(raw as usize).cloned()
    };
    let value = match data_type {
        // aapt 偶尔只写 raw 值、类型留 0。
        0x00 => raw_text
            .map(AttrValue::Text)
            .unwrap_or_else(|| AttrValue::Other(format!("0x{data:08x}"))),
        0x03 => match pool.get(data as usize) {
            Some(text) => AttrValue::Text(text.clone()),
            None => AttrValue::Other(format!("type=0x03 data=0x{data:08x}")),
        },
        0x01 => AttrValue::Text(format!("@0x{data:08x}")),
        0x02 => AttrValue::Text(format!("?0x{data:08x}")),
        0x04 => AttrValue::Text(format!("{}", f32::from_bits(data))),
        0x05 | 0x06 => AttrValue::Other(format!("0x{data:08x}")),
        0x10 | 0x11 => AttrValue::Int(i64::from(data)),
        0x12 => AttrValue::Bool(data != 0),
        0x1c..=0x1f => AttrValue::Text(format!("#{data:08x}")),
        other => AttrValue::Other(format!("type=0x{other:02x} data=0x{data:08x}")),
    };
    Ok(value)
}

// ---------------------------------------------------------------- 解释

fn interpret(root: &Element) -> Value {
    let mut out = Map::new();
    out.insert("format".to_string(), json!("apk-manifest"));
    insert_text(&mut out, root, "package");
    insert_text(&mut out, root, "sharedUserId");
    insert_value(&mut out, root, "versionCode");
    insert_value(&mut out, root, "versionCodeMajor");
    insert_text(&mut out, root, "versionName");
    insert_value(&mut out, root, "compileSdkVersion");
    insert_text(&mut out, root, "compileSdkVersionCodename");
    if let Some(location) = root.int("installLocation") {
        out.insert(
            "installLocation".to_string(),
            json!(match location {
                0 => "auto".to_string(),
                1 => "internalOnly".to_string(),
                2 => "preferExternal".to_string(),
                other => other.to_string(),
            }),
        );
    }

    if let Some(sdk) = root.children_named("uses-sdk").next() {
        let mut uses_sdk = Map::new();
        for key in ["minSdkVersion", "targetSdkVersion", "maxSdkVersion"] {
            if let Some(value) = sdk.attr(key) {
                uses_sdk.insert(key.to_string(), attr_json(value));
            }
        }
        out.insert("usesSdk".to_string(), Value::Object(uses_sdk));
        // 常用字段平铺一份，提示词里少一层路径。
        for key in ["minSdkVersion", "targetSdkVersion", "maxSdkVersion"] {
            if let Some(value) = sdk.attr(key) {
                out.insert(key.to_string(), attr_json(value));
            }
        }
    }

    let mut permissions: Vec<Value> = Vec::new();
    let mut permission_names: Vec<String> = Vec::new();
    for child in root
        .children
        .iter()
        .filter(|child| child.name == "uses-permission" || child.name == "uses-permission-sdk-23")
    {
        let Some(name) = child.text("name") else {
            continue;
        };
        let mut detail = Map::new();
        detail.insert("name".to_string(), json!(name));
        if child.name == "uses-permission-sdk-23" {
            detail.insert("sdk23".to_string(), json!(true));
        }
        if let Some(max) = child.attr("maxSdkVersion") {
            detail.insert("maxSdkVersion".to_string(), attr_json(max));
        }
        if !permission_names.contains(&name) {
            permission_names.push(name);
        }
        if permissions.len() < PERMISSION_LIMIT {
            permissions.push(Value::Object(detail));
        }
    }
    out.insert("permissions".to_string(), json!(permission_names));
    out.insert("permissionDetails".to_string(), Value::Array(permissions));

    let mut declared: Vec<Value> = Vec::new();
    let mut dangerous_permissions: Vec<String> = Vec::new();
    for child in root.children_named("permission") {
        let Some(name) = child.text("name") else {
            continue;
        };
        let mut detail = Map::new();
        detail.insert("name".to_string(), json!(name));
        if let Some(level) = child.attr("protectionLevel") {
            let raw = level.as_int().unwrap_or(0);
            let level = protection_level(raw);
            if level.split('|').any(|part| part == "dangerous") {
                dangerous_permissions.push(name.clone());
            }
            detail.insert("protectionLevelRaw".to_string(), json!(raw));
            detail.insert("protectionLevel".to_string(), json!(level));
        }
        if declared.len() < PERMISSION_LIMIT {
            declared.push(Value::Object(detail));
        }
    }
    let declared_count = declared.len();
    out.insert("declaredPermissions".to_string(), Value::Array(declared));

    let mut features: Vec<Value> = Vec::new();
    let mut feature_count = 0usize;
    for child in root.children_named("uses-feature") {
        feature_count += 1;
        if features.len() >= PERMISSION_LIMIT {
            continue;
        }
        let Some(name) = child.text("name") else {
            continue;
        };
        let mut detail = Map::new();
        detail.insert("name".to_string(), json!(name));
        if let Some(required) = child.boolean("required") {
            detail.insert("required".to_string(), json!(required));
        }
        if let Some(version) = child.attr("glEsVersion") {
            detail.insert("glEsVersion".to_string(), attr_json(version));
        }
        features.push(Value::Object(detail));
    }
    out.insert("features".to_string(), Value::Array(features));

    let mut facts: Vec<String> = Vec::new();
    let mut components = Vec::new();
    let mut counts = Map::new();
    counts.insert("permissions".to_string(), json!(permission_names.len()));
    counts.insert("declaredPermissions".to_string(), json!(declared_count));
    counts.insert("features".to_string(), json!(feature_count));

    if let Some(app) = root.children_named("application").next() {
        out.insert("application".to_string(), application(app, &mut facts));
    }

    for kind in ["activity", "activity-alias", "service", "receiver", "provider"] {
        let mut kind_count = 0usize;
        for child in root
            .children
            .iter()
            .filter(|child| child.name == "application")
            .flat_map(|app| app.children_named(kind))
        {
            kind_count += 1;
            if components.len() >= COMPONENT_LIMIT {
                continue;
            }
            if let Some(component) = component(kind, child, &mut facts) {
                components.push(component);
            }
        }
        let key = match kind {
            "activity-alias" => "activityAliases",
            "activity" => "activities",
            "service" => "services",
            "receiver" => "receivers",
            _ => "providers",
        };
        counts.insert(key.to_string(), json!(kind_count));
    }
    counts.insert("components".to_string(), json!(components.len()));
    out.insert("counts".to_string(), Value::Object(counts));
    out.insert("components".to_string(), Value::Array(components));

    // 清单层面的显式风险事实：只写「声明了什么」，不写「有多危险」。
    if let Some(uid) = root.text("sharedUserId") {
        facts.push(format!(
            "声明 sharedUserId={uid}：与同一签名的其它应用共享 UID 与数据目录"
        ));
    }
    for name in &dangerous_permissions {
        facts.push(format!(
            "自定义权限 {name} 的 protectionLevel 含 dangerous"
        ));
    }
    facts.sort();
    facts.dedup();
    if facts.len() > FACT_LIMIT {
        let omitted = facts.len() - FACT_LIMIT;
        facts.truncate(FACT_LIMIT);
        facts.push(format!("…另有 {omitted} 条清单事实未展开"));
    }
    out.insert("facts".to_string(), json!(facts));

    out.insert(
        "note".to_string(),
        json!(
            "二进制 AndroidManifest（AXML）的结构化读取：`@0x…` 是资源引用，未解析 \
             resources.arsc；exported 的缺省值由系统按 targetSdk 与 intent-filter 推断，\
             本工具只报「没声明」。facts 是清单显式声明的事实，不是风险评分。"
        ),
    );
    Value::Object(out)
}

fn application(app: &Element, facts: &mut Vec<String>) -> Value {
    let mut out = Map::new();
    for key in [
        "name",
        "label",
        "icon",
        "theme",
        "networkSecurityConfig",
        "appComponentFactory",
        "fullBackupContent",
        "process",
        "permission",
    ] {
        insert_text(&mut out, app, key);
    }
    for key in [
        "debuggable",
        "allowBackup",
        "usesCleartextTraffic",
        "testOnly",
        "extractNativeLibs",
        "supportsRtl",
        "requestLegacyExternalStorage",
    ] {
        if let Some(value) = app.boolean(key) {
            out.insert(key.to_string(), json!(value));
            match (key, value) {
                ("debuggable", true) => {
                    facts.push("application.debuggable=true：清单显式允许调试".to_string())
                }
                ("testOnly", true) => {
                    facts.push("application.testOnly=true：只允许测试安装".to_string())
                }
                ("usesCleartextTraffic", true) => facts.push(
                    "application.usesCleartextTraffic=true：清单显式允许明文 HTTP".to_string(),
                ),
                ("allowBackup", true) => {
                    facts.push("application.allowBackup=true：允许系统备份应用数据".to_string())
                }
                ("requestLegacyExternalStorage", true) => facts.push(
                    "application.requestLegacyExternalStorage=true：请求旧的共享存储模型"
                        .to_string(),
                ),
                ("extractNativeLibs", false) => facts.push(
                    "application.extractNativeLibs=false：so 直接从 APK 映射，不落到 lib 目录"
                        .to_string(),
                ),
                _ => {}
            }
        }
    }
    if let Some(factory) = app.text("appComponentFactory") {
        facts.push(format!(
            "application.appComponentFactory={factory}：组件实例化走自定义工厂，不是系统默认"
        ));
    }
    if app.attr("networkSecurityConfig").is_some() {
        facts.push(
            "声明了 networkSecurityConfig：明文 / 证书信任的具体策略在引用的 XML 里，本工具未解析"
                .to_string(),
        );
    }
    Value::Object(out)
}

fn component(kind: &str, element: &Element, facts: &mut Vec<String>) -> Option<Value> {
    let name = element.text("name")?;
    let mut out = Map::new();
    out.insert("type".to_string(), json!(kind));
    out.insert("name".to_string(), json!(name.clone()));

    let exported = element.boolean("exported");
    if let Some(value) = exported {
        out.insert("exported".to_string(), json!(value));
    }
    let permission = element.text("permission");
    if let Some(value) = &permission {
        out.insert("permission".to_string(), json!(value));
    }
    for key in ["process", "taskAffinity", "authorities"] {
        if let Some(value) = element.text(key) {
            out.insert(key.to_string(), json!(value));
        }
    }
    if let Some(value) = element.boolean("enabled") {
        out.insert("enabled".to_string(), json!(value));
    }
    if let Some(value) = element.boolean("directBootAware") {
        out.insert("directBootAware".to_string(), json!(value));
    }
    if let Some(value) = element.boolean("grantUriPermissions") {
        out.insert("grantUriPermissions".to_string(), json!(value));
    }
    if let Some(mode) = element.int("launchMode") {
        out.insert(
            "launchMode".to_string(),
            json!(match mode {
                0 => "standard".to_string(),
                1 => "singleTop".to_string(),
                2 => "singleTask".to_string(),
                3 => "singleInstance".to_string(),
                4 => "singleInstancePerTask".to_string(),
                other => other.to_string(),
            }),
        );
    }

    let mut filters = Vec::new();
    for (index, filter) in element.children_named("intent-filter").enumerate() {
        if index >= FILTER_LIMIT {
            break;
        }
        filters.push(intent_filter(filter));
    }
    let has_filter = element.children_named("intent-filter").next().is_some();
    if !filters.is_empty() {
        out.insert("intentFilters".to_string(), Value::Array(filters));
    }

    // 导出面的事实：只按清单显式声明写。exported 缺省时的实际值由系统推断，
    // 这里不替它下结论。
    match exported {
        Some(true) if permission.is_none() => facts.push(format!(
            "{kind} {name} 声明 exported=true 且未声明 permission"
        )),
        Some(_) => {}
        None if has_filter => facts.push(format!(
            "{kind} {name} 未声明 exported 但有 intent-filter（实际是否导出由系统推断，本工具不推断）"
        )),
        None => {}
    }
    if kind == "provider" && exported == Some(true) && permission.is_none() {
        if let Some(authorities) = element.text("authorities") {
            facts.push(format!(
                "provider {name} 对外暴露 authorities={authorities}（exported=true、无 permission）"
            ));
        }
    }
    Some(Value::Object(out))
}

fn intent_filter(filter: &Element) -> Value {
    let mut out = Map::new();
    let mut actions = Vec::new();
    for action in filter.children_named("action") {
        if actions.len() >= ACTION_LIMIT {
            break;
        }
        if let Some(name) = action.text("name") {
            actions.push(name);
        }
    }
    let mut categories = Vec::new();
    for category in filter.children_named("category") {
        if categories.len() >= ACTION_LIMIT {
            break;
        }
        if let Some(name) = category.text("name") {
            categories.push(name);
        }
    }
    let mut data = Vec::new();
    for item in filter.children_named("data") {
        if data.len() >= FILTER_LIMIT {
            break;
        }
        let mut entry = Map::new();
        for key in [
            "scheme",
            "host",
            "port",
            "path",
            "pathPrefix",
            "pathPattern",
            "mimeType",
        ] {
            if let Some(value) = item.text(key) {
                entry.insert(key.to_string(), json!(value));
            }
        }
        if !entry.is_empty() {
            data.push(Value::Object(entry));
        }
    }
    if let Some(auto_verify) = filter.boolean("autoVerify") {
        out.insert("autoVerify".to_string(), json!(auto_verify));
    }
    if !actions.is_empty() {
        out.insert("actions".to_string(), json!(actions));
    }
    if !categories.is_empty() {
        out.insert("categories".to_string(), json!(categories));
    }
    if !data.is_empty() {
        out.insert("data".to_string(), Value::Array(data));
    }
    Value::Object(out)
}

fn protection_level(raw: i64) -> String {
    let base = match raw & 0xf {
        0 => "normal",
        1 => "dangerous",
        2 => "signature",
        3 => "signatureOrSystem",
        _ => "unknown",
    };
    let mut parts = vec![base.to_string()];
    for (bit, name) in [
        (0x10, "privileged"),
        (0x20, "development"),
        (0x40, "appop"),
        (0x80, "pre23"),
        (0x100, "installer"),
        (0x200, "verifier"),
        (0x400, "preinstalled"),
        (0x800, "setup"),
        (0x1000, "instant"),
        (0x2000, "runtime"),
        (0x4000, "oem"),
        (0x8000, "vendorPrivileged"),
    ] {
        if raw & bit != 0 {
            parts.push(name.to_string());
        }
    }
    parts.join("|")
}

fn insert_text(out: &mut Map<String, Value>, element: &Element, key: &str) {
    if let Some(value) = element.text(key) {
        out.insert(key.to_string(), json!(value));
    }
}

fn insert_value(out: &mut Map<String, Value>, element: &Element, key: &str) {
    if let Some(value) = element.attr(key) {
        out.insert(key.to_string(), attr_json(value));
    }
}

fn attr_json(value: &AttrValue) -> Value {
    match value {
        AttrValue::Text(text) | AttrValue::Other(text) => json!(text),
        AttrValue::Int(value) => json!(value),
        AttrValue::Bool(value) => json!(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------ AXML 构造器

    #[derive(Clone)]
    enum V {
        Text(&'static str),
        Int(u32),
        Bool(bool),
        Ref(u32),
    }

    struct Doc {
        strings: Vec<String>,
        resmap: Vec<u32>,
        chunks: Vec<u8>,
        utf8: bool,
    }

    impl Doc {
        fn new() -> Self {
            Doc {
                strings: Vec::new(),
                resmap: Vec::new(),
                chunks: Vec::new(),
                utf8: false,
            }
        }

        fn utf8() -> Self {
            Doc {
                utf8: true,
                ..Doc::new()
            }
        }

        fn intern(&mut self, text: &str) -> u32 {
            if let Some(index) = self.strings.iter().position(|item| item == text) {
                return index as u32;
            }
            self.strings.push(text.to_string());
            (self.strings.len() - 1) as u32
        }

        /// 注册一个属性名。`rid=0` 表示没有资源 ID（例如 `package`）。
        fn attr_name(&mut self, rid: u32, name: &str) -> u32 {
            let index = self.intern(name) as usize;
            if self.resmap.len() <= index {
                self.resmap.resize(index + 1, 0);
            }
            if rid != 0 {
                self.resmap[index] = rid;
            }
            index as u32
        }

        fn start(&mut self, ns: Option<&str>, name: &str, attrs: &[(Option<&str>, u32, V)]) {
            let ns_index = ns.map(|uri| self.intern(uri)).unwrap_or(NO_ENTRY);
            let name_index = self.intern(name);
            let size = 36 + attrs.len() * 20;
            self.chunks.extend_from_slice(&CHUNK_START_ELEMENT.to_le_bytes());
            self.chunks.extend_from_slice(&16u16.to_le_bytes());
            self.chunks.extend_from_slice(&(size as u32).to_le_bytes());
            self.chunks.extend_from_slice(&0u32.to_le_bytes()); // line
            self.chunks.extend_from_slice(&0u32.to_le_bytes()); // comment
            self.chunks.extend_from_slice(&ns_index.to_le_bytes());
            self.chunks.extend_from_slice(&name_index.to_le_bytes());
            self.chunks.extend_from_slice(&20u16.to_le_bytes()); // attributeStart
            self.chunks.extend_from_slice(&20u16.to_le_bytes()); // attributeSize
            self.chunks.extend_from_slice(&(attrs.len() as u16).to_le_bytes());
            self.chunks.extend_from_slice(&0u16.to_le_bytes()); // idIndex
            self.chunks.extend_from_slice(&0u16.to_le_bytes()); // classIndex
            self.chunks.extend_from_slice(&0u16.to_le_bytes()); // styleIndex
            for (attr_ns, name_index, value) in attrs {
                let ns_index = attr_ns.map(|uri| self.intern(uri)).unwrap_or(NO_ENTRY);
                let (raw, data_type, data) = match value {
                    V::Text(text) => {
                        let index = self.intern(text);
                        (NO_ENTRY, 0x03, index)
                    }
                    V::Ref(id) => (NO_ENTRY, 0x01, *id),
                    V::Int(number) => (NO_ENTRY, 0x10, *number),
                    V::Bool(value) => (NO_ENTRY, 0x12, u32::from(*value)),
                };
                self.chunks.extend_from_slice(&ns_index.to_le_bytes());
                self.chunks.extend_from_slice(&name_index.to_le_bytes());
                self.chunks.extend_from_slice(&raw.to_le_bytes());
                self.chunks.extend_from_slice(&8u16.to_le_bytes());
                self.chunks.push(0);
                self.chunks.push(data_type);
                self.chunks.extend_from_slice(&data.to_le_bytes());
            }
        }

        fn end(&mut self, ns: Option<&str>, name: &str) {
            let ns_index = ns.map(|uri| self.intern(uri)).unwrap_or(NO_ENTRY);
            let name_index = self.intern(name);
            self.chunks.extend_from_slice(&CHUNK_END_ELEMENT.to_le_bytes());
            self.chunks.extend_from_slice(&16u16.to_le_bytes());
            self.chunks.extend_from_slice(&24u32.to_le_bytes());
            self.chunks.extend_from_slice(&0u32.to_le_bytes());
            self.chunks.extend_from_slice(&0u32.to_le_bytes());
            self.chunks.extend_from_slice(&ns_index.to_le_bytes());
            self.chunks.extend_from_slice(&name_index.to_le_bytes());
        }

        fn finish(&self) -> Vec<u8> {
            let mut data = Vec::new();
            let mut offsets = Vec::new();
            for text in &self.strings {
                offsets.push(data.len() as u32);
                if self.utf8 {
                    let bytes = text.as_bytes();
                    assert!(bytes.len() < 0x80);
                    data.push(bytes.len() as u8);
                    data.extend_from_slice(bytes);
                    data.push(0);
                } else {
                    let units: Vec<u16> = text.encode_utf16().collect();
                    assert!(units.len() < 0x8000);
                    data.extend_from_slice(&(units.len() as u16).to_le_bytes());
                    for unit in units {
                        data.extend_from_slice(&unit.to_le_bytes());
                    }
                    data.extend_from_slice(&0u16.to_le_bytes());
                }
            }
            let strings_start = 28 + offsets.len() * 4;
            let mut pool = Vec::new();
            pool.extend_from_slice(&CHUNK_STRING_POOL.to_le_bytes());
            pool.extend_from_slice(&28u16.to_le_bytes());
            pool.extend_from_slice(&((strings_start + data.len()) as u32).to_le_bytes());
            pool.extend_from_slice(&(self.strings.len() as u32).to_le_bytes());
            pool.extend_from_slice(&0u32.to_le_bytes()); // styleCount
            pool.extend_from_slice(&(if self.utf8 { 0x100u32 } else { 0 }).to_le_bytes());
            pool.extend_from_slice(&(strings_start as u32).to_le_bytes());
            pool.extend_from_slice(&0u32.to_le_bytes()); // stylesStart
            for offset in offsets {
                pool.extend_from_slice(&offset.to_le_bytes());
            }
            pool.extend_from_slice(&data);

            let mut body = pool;
            if !self.resmap.is_empty() {
                body.extend_from_slice(&CHUNK_RESOURCE_MAP.to_le_bytes());
                body.extend_from_slice(&8u16.to_le_bytes());
                body.extend_from_slice(&((8 + self.resmap.len() * 4) as u32).to_le_bytes());
                for id in &self.resmap {
                    body.extend_from_slice(&id.to_le_bytes());
                }
            }
            body.extend_from_slice(&self.chunks);

            let mut out = Vec::new();
            out.extend_from_slice(&CHUNK_FILE.to_le_bytes());
            out.extend_from_slice(&8u16.to_le_bytes());
            out.extend_from_slice(&((8 + body.len()) as u32).to_le_bytes());
            out.extend_from_slice(&body);
            out
        }
    }

    /// 一份覆盖主要字段的清单：包信息、SDK、权限、应用标志、四类组件。
    fn sample_manifest(doc: &mut Doc) -> Vec<u8> {
        let package = doc.attr_name(0, "package");
        let version_code = doc.attr_name(0x0101_021b, "versionCode");
        let version_name = doc.attr_name(0x0101_021c, "versionName");
        let compile_sdk = doc.attr_name(0x0101_0572, "compileSdkVersion");
        let compile_codename = doc.attr_name(0x0101_0573, "compileSdkVersionCodename");
        let name = doc.attr_name(0x0101_0003, "name");
        let min_sdk = doc.attr_name(0x0101_020c, "minSdkVersion");
        let target_sdk = doc.attr_name(0x0101_0270, "targetSdkVersion");
        let max_sdk = doc.attr_name(0x0101_0271, "maxSdkVersion");
        let required = doc.attr_name(0x0101_028e, "required");
        let protection = doc.attr_name(0x0101_0009, "protectionLevel");
        let debuggable = doc.attr_name(0x0101_000f, "debuggable");
        let allow_backup = doc.attr_name(0x0101_0280, "allowBackup");
        let cleartext = doc.attr_name(0x0101_04ec, "usesCleartextTraffic");
        let nsc = doc.attr_name(0x0101_0527, "networkSecurityConfig");
        let factory = doc.attr_name(0x0101_057a, "appComponentFactory");
        let exported = doc.attr_name(0x0101_0010, "exported");
        let permission = doc.attr_name(0x0101_0006, "permission");
        let authorities = doc.attr_name(0x0101_0018, "authorities");
        let grant = doc.attr_name(0x0101_001b, "grantUriPermissions");
        let scheme = doc.attr_name(0x0101_0027, "scheme");
        let host = doc.attr_name(0x0101_0028, "host");

        doc.start(
            None,
            "manifest",
            &[
                (None, package, V::Text("com.example.demo")),
                (None, version_code, V::Int(45)),
                (None, version_name, V::Text("1.42-demo")),
                (None, compile_sdk, V::Int(36)),
                (None, compile_codename, V::Text("16")),
            ],
        );
        doc.start(
            None,
            "uses-sdk",
            &[(None, min_sdk, V::Int(24)), (None, target_sdk, V::Int(35))],
        );
        doc.end(None, "uses-sdk");
        doc.start(
            None,
            "uses-permission",
            &[(None, name, V::Text("android.permission.INTERNET"))],
        );
        doc.end(None, "uses-permission");
        doc.start(
            None,
            "uses-permission",
            &[
                (None, name, V::Text("android.permission.GET_ACCOUNTS")),
                (None, max_sdk, V::Int(22)),
            ],
        );
        doc.end(None, "uses-permission");
        doc.start(
            None,
            "permission",
            &[
                (None, name, V::Text("com.example.DYNAMIC_PERMISSION")),
                (None, protection, V::Int(1)),
            ],
        );
        doc.end(None, "permission");
        doc.start(
            None,
            "uses-feature",
            &[
                (None, name, V::Text("android.software.leanback")),
                (None, required, V::Bool(false)),
            ],
        );
        doc.end(None, "uses-feature");
        doc.start(
            None,
            "application",
            &[
                (None, name, V::Text(".App")),
                (None, debuggable, V::Bool(true)),
                (None, allow_backup, V::Bool(true)),
                (None, cleartext, V::Bool(true)),
                (None, nsc, V::Ref(0x7f0f0001)),
                (None, factory, V::Text("androidx.core.app.CoreComponentFactory")),
            ],
        );
        doc.start(
            None,
            "activity",
            &[
                (None, name, V::Text(".MainActivity")),
                (None, exported, V::Bool(true)),
            ],
        );
        doc.start(None, "intent-filter", &[]);
        doc.start(
            None,
            "action",
            &[(None, name, V::Text("android.intent.action.MAIN"))],
        );
        doc.end(None, "action");
        doc.start(
            None,
            "category",
            &[(None, name, V::Text("android.intent.category.LAUNCHER"))],
        );
        doc.end(None, "category");
        doc.start(
            None,
            "data",
            &[
                (None, scheme, V::Text("https")),
                (None, host, V::Text("example.com")),
            ],
        );
        doc.end(None, "data");
        doc.end(None, "intent-filter");
        doc.end(None, "activity");

        // 未声明 exported、但有 intent-filter：只报事实，不推断系统结论。
        doc.start(
            None,
            "activity",
            &[(None, name, V::Text(".ImplicitActivity"))],
        );
        doc.start(None, "intent-filter", &[]);
        doc.start(
            None,
            "action",
            &[(None, name, V::Text("com.example.ACTION"))],
        );
        doc.end(None, "action");
        doc.end(None, "intent-filter");
        doc.end(None, "activity");

        doc.start(
            None,
            "service",
            &[
                (None, name, V::Text(".BoundService")),
                (None, exported, V::Bool(true)),
                (None, permission, V::Text("com.example.BIND_SERVICE")),
            ],
        );
        doc.end(None, "service");
        doc.start(
            None,
            "receiver",
            &[
                (None, name, V::Text(".BootReceiver")),
                (None, exported, V::Bool(true)),
            ],
        );
        doc.end(None, "receiver");
        doc.start(
            None,
            "provider",
            &[
                (None, name, V::Text(".FileProvider")),
                (None, exported, V::Bool(true)),
                (None, authorities, V::Text("com.example.files")),
                (None, grant, V::Bool(true)),
            ],
        );
        doc.end(None, "provider");
        doc.end(None, "application");
        doc.end(None, "manifest");
        doc.finish()
    }

    #[test]
    fn reads_package_sdk_permissions_and_components() {
        let mut doc = Doc::new();
        let axml = sample_manifest(&mut doc);
        let report = inspect(
            &{
                // 走完整链路：把 AXML 塞进一个最小的 stored zip。
                zip_with(&[("AndroidManifest.xml", &axml)])
            },
        )
        .expect("应当能解析");

        assert_eq!(report["package"], "com.example.demo");
        assert_eq!(report["versionCode"], 45);
        assert_eq!(report["versionName"], "1.42-demo");
        assert_eq!(report["minSdkVersion"], 24);
        assert_eq!(report["targetSdkVersion"], 35);
        assert_eq!(report["compileSdkVersion"], 36);
        assert_eq!(report["usesSdk"]["minSdkVersion"], 24);

        let permissions = report["permissions"].as_array().unwrap();
        assert!(permissions.contains(&json!("android.permission.INTERNET")));
        let details = report["permissionDetails"].as_array().unwrap();
        let accounts = details
            .iter()
            .find(|item| item["name"] == "android.permission.GET_ACCOUNTS")
            .expect("GET_ACCOUNTS 详情");
        assert_eq!(accounts["maxSdkVersion"], 22);

        assert_eq!(
            report["declaredPermissions"][0]["protectionLevel"],
            "dangerous"
        );

        assert_eq!(report["application"]["debuggable"], true);
        assert_eq!(report["application"]["allowBackup"], true);
        assert_eq!(report["application"]["usesCleartextTraffic"], true);
        assert_eq!(
            report["application"]["networkSecurityConfig"],
            "@0x7f0f0001",
            "资源引用必须原样保留，不能猜成资源名"
        );

        let activities = report["components"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["type"] == "activity")
            .collect::<Vec<_>>();
        assert_eq!(activities.len(), 2);
        let main = activities
            .iter()
            .find(|item| item["name"] == ".MainActivity")
            .unwrap();
        assert_eq!(main["exported"], true);
        assert_eq!(
            main["intentFilters"][0]["actions"][0],
            "android.intent.action.MAIN"
        );
        assert_eq!(
            main["intentFilters"][0]["categories"][0],
            "android.intent.category.LAUNCHER"
        );
        assert_eq!(main["intentFilters"][0]["data"][0]["host"], "example.com");
        let implicit = activities
            .iter()
            .find(|item| item["name"] == ".ImplicitActivity")
            .unwrap();
        assert!(
            implicit.get("exported").is_none(),
            "没声明就不能补默认值：{implicit}"
        );

        let facts = report["facts"].as_array().unwrap();
        let has = |needle: &str| {
            facts
                .iter()
                .any(|item| item.as_str().unwrap_or("").contains(needle))
        };
        assert!(has("debuggable=true"), "{facts:?}");
        assert!(has("usesCleartextTraffic=true"), "{facts:?}");
        assert!(has("activity .MainActivity 声明 exported=true 且未声明 permission"), "{facts:?}");
        assert!(has("receiver .BootReceiver 声明 exported=true 且未声明 permission"), "{facts:?}");
        assert!(has("provider .FileProvider 对外暴露 authorities=com.example.files"), "{facts:?}");
        assert!(has(".ImplicitActivity 未声明 exported 但有 intent-filter"), "{facts:?}");
        assert!(has("protectionLevel 含 dangerous"), "{facts:?}");
        // 带 permission 的 service 不该被算成「无保护导出」。
        assert!(
            !facts
                .iter()
                .any(|item| item.as_str().unwrap_or("").contains(".BoundService")),
            "{facts:?}"
        );

        assert_eq!(report["counts"]["activities"], 2);
        assert_eq!(report["counts"]["services"], 1);
        assert_eq!(report["counts"]["receivers"], 1);
        assert_eq!(report["counts"]["providers"], 1);
        assert!(
            report["note"]
                .as_str()
                .unwrap()
                .contains("未解析 resources.arsc")
        );
    }

    #[test]
    fn utf8_string_pool_is_supported() {
        let mut doc = Doc::utf8();
        let package = doc.attr_name(0, "package");
        let name = doc.attr_name(0x0101_0003, "name");
        doc.start(None, "manifest", &[(None, package, V::Text("com.示例.demo"))]);
        doc.start(None, "application", &[(None, name, V::Text(".App"))]);
        doc.end(None, "application");
        doc.end(None, "manifest");
        let report = inspect(&zip_with(&[("AndroidManifest.xml", &doc.finish())]))
            .expect("UTF-8 字符串池也要能读");
        assert_eq!(report["package"], "com.示例.demo");
        assert_eq!(report["application"]["name"], ".App");
    }

    #[test]
    fn plain_text_manifest_is_reported_as_unsupported() {
        let apk = zip_with(&[("AndroidManifest.xml", b"<manifest package=\"x\"/>")]);
        let error = inspect(&apk).expect_err("明文 XML 不应被猜着解析");
        assert!(error.contains("明文 XML"), "{error}");
    }

    #[test]
    fn missing_manifest_is_an_error() {
        let apk = zip_with(&[("classes.dex", b"dex\n035\0")]);
        let error = inspect(&apk).expect_err("没有清单必须报错");
        assert!(error.contains("AndroidManifest.xml"), "{error}");
    }

    #[test]
    fn broken_chunk_size_is_rejected() {
        let mut doc = Doc::new();
        let package = doc.attr_name(0, "package");
        doc.start(None, "manifest", &[(None, package, V::Text("x"))]);
        doc.end(None, "manifest");
        let mut axml = doc.finish();
        // 字符串池 size 改成超过文件长度。
        axml[8 + 4..8 + 8].copy_from_slice(&0xffff_ffffu32.to_le_bytes());
        let error = inspect(&zip_with(&[("AndroidManifest.xml", &axml)]))
            .expect_err("块尺寸越界必须报错");
        assert!(error.contains("尺寸不自洽"), "{error}");
    }

    #[test]
    fn unclosed_element_is_rejected() {
        let mut doc = Doc::new();
        let package = doc.attr_name(0, "package");
        doc.start(None, "manifest", &[(None, package, V::Text("x"))]);
        // 故意不写 end。
        let error = inspect(&zip_with(&[("AndroidManifest.xml", &doc.finish())]))
            .expect_err("截断必须报错");
        assert!(error.contains("没有闭合"), "{error}");
    }

    /// 真样本验收：包信息、权限与导出组件（样本在开发机上；缺失时跳过）。
    #[test]
    fn real_sample_if_present() {
        let path = std::path::Path::new("/Users/edwa2d4th/Desktop/未命名文件夹 7/aerial-guard.apk");
        if !path.exists() {
            eprintln!("跳过：{} 不在本机", path.display());
            return;
        }
        let blob = std::fs::read(path).expect("读样本");
        let report = inspect(&blob).expect("应当能解析真实 APK");
        assert_eq!(report["package"], "app.orchard.tv");
        assert_eq!(report["versionCode"], 45);
        assert_eq!(report["minSdkVersion"], 21);
        assert_eq!(report["targetSdkVersion"], 34);
        assert_eq!(report["application"]["allowBackup"], false);
        assert_eq!(
            report["application"]["networkSecurityConfig"],
            "@0x7f0f0001"
        );
        let permissions = report["permissions"].as_array().unwrap();
        assert!(
            permissions.contains(&json!("android.permission.REQUEST_INSTALL_PACKAGES")),
            "安装包权限是这份样本的关键事实：{permissions:?}"
        );
        let components = report["components"].as_array().unwrap();
        let main = components
            .iter()
            .find(|item| item["name"] == "app.orchard.tv.MainActivity")
            .expect("主 Activity");
        assert_eq!(main["exported"], true);
        let accessibility = components
            .iter()
            .find(|item| item["name"] == "app.orchard.tv.guard.HomeGuardService")
            .expect("无障碍服务");
        assert_eq!(
            accessibility["permission"],
            "android.permission.BIND_ACCESSIBILITY_SERVICE"
        );
        let provider = components
            .iter()
            .find(|item| item["name"] == "androidx.core.content.FileProvider")
            .expect("FileProvider");
        assert_eq!(provider["exported"], false);
        assert_eq!(provider["authorities"], "app.orchard.tv.files");
        let facts = report["facts"].as_array().unwrap();
        let has = |needle: &str| {
            facts
                .iter()
                .any(|item| item.as_str().unwrap_or("").contains(needle))
        };
        assert!(has("networkSecurityConfig"), "{facts:?}");
        assert!(has("appComponentFactory=androidx.core.app.CoreComponentFactory"), "{facts:?}");
        // 声明了 exported=false 的 provider 不该被当成「对外暴露」。
        assert!(
            !has("provider androidx.core.content.FileProvider 对外暴露"),
            "{facts:?}"
        );
    }

    /// 测试用最小 zip（stored）：只需要能让 apkzip 找到条目。
    fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut body = Vec::new();
        let mut central = Vec::new();
        for (name, data) in entries {
            let offset = body.len() as u32;
            body.extend_from_slice(b"PK\x03\x04");
            body.extend_from_slice(&20u16.to_le_bytes());
            body.extend_from_slice(&0u16.to_le_bytes());
            body.extend_from_slice(&0u16.to_le_bytes());
            body.extend_from_slice(&0u16.to_le_bytes());
            body.extend_from_slice(&0u16.to_le_bytes());
            body.extend_from_slice(&0u32.to_le_bytes());
            body.extend_from_slice(&(data.len() as u32).to_le_bytes());
            body.extend_from_slice(&(data.len() as u32).to_le_bytes());
            body.extend_from_slice(&(name.len() as u16).to_le_bytes());
            body.extend_from_slice(&0u16.to_le_bytes());
            body.extend_from_slice(name.as_bytes());
            body.extend_from_slice(data);

            central.extend_from_slice(b"PK\x01\x02");
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let central_offset = body.len() as u32;
        body.extend_from_slice(&central);
        body.extend_from_slice(b"PK\x05\x06");
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        body.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        body.extend_from_slice(&(central.len() as u32).to_le_bytes());
        body.extend_from_slice(&central_offset.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body
    }
}
