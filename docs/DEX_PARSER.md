# DEX 解析器设计（`engines/native/dex.rs`）

> 状态：设计稿，待评审后进入实现。
> 目标：补全 prism「安全研究线」缺失的 DEX `code_item` 指令级解析能力，让逆向 agent 拿到真正的「哪个方法调用了什么」证据，闭合 `aerial-guard` 那类报告里「指令级证据 = 0」的硬天花板。

---

## 1. 背景与动机

现有安全研究线（`engines/native/`）已覆盖 PE / ELF / Mach-O：

| 工具 | 能力 | DEX 支持 |
|---|---|---|
| `bin.meta.identify` | 格式识别 | ✅ 已认得 `dex\n` magic |
| `bin.hash.compute` | 哈希 | ✅ 通用 |
| `bin.strings.extract` | 字符串 | ✅ 通用（但拿不到「谁引用」） |
| `bin.pack.detect` | 加壳识别 | ❌ 无 DEX 壳特征 |
| `bin.xref.search` | 字节级交叉引用 | ⚠️ 只做 needle 匹配，非语义引用 |
| `bin.importexport.list` | 导入导出表 | ❌ 明确只支持 PE / ELF |
| `bin.crypto.scan` | 密码常量 | ⚠️ 通用字节扫描 |

**缺口**：没有 DEX 的 `class_data_item → code_item` 解析器。导致逆向 agent 只能拿到「符号在池」级证据（字符串池、类型池、method_id 表），进不了「方法字节码」层 —— 即「哪个方法 invoke 了什么、`const-string` 引用了什么」永远拿不到。

上一轮 `aerial-guard` 逆向中，这个缺口被临时 Python 脚本（`dexparse.py` / `dexcf.py`）补上，并暴露出一个 P0 bug（`class_data_item` 漏跳 `encoded_field` 条目 → `code_off` 读到垃圾值）。本设计把那份已验证的逻辑**固化为 prism 内置 Rust 能力**，一次性根治。

---

## 2. 目标与非目标

### 目标
- 纯 Rust 解析 DEX，产出**指令级反汇编**与**语义调用图**，覆盖全部 opcode。
- 走既有工具注册表（`builtin_tools()` + `static_tool` 范式），零新依赖。
- 只读、有上限、不猜 —— 遵守安全研究线三条纪律。

### 非目标
- 不做 DEX 反编译成 Java 源码（那是 Ghidra/jadx 的活）。
- 不做 APK 解包（zip 解包由现有 `run_command` 完成）。
- 不做动态执行 / 脱壳。
- 不解析 `resources.arsc`（另立任务）。
- 不处理 `dalvik` 之外的扩展（如 compact-dex `cdex`），遇到即明确报「不支持」。

---

## 3. 工具划分

与 `analysis.rs` 的「结构 / 引用 / 算法」分离思路一致，拆两个工具：

| 工具 | wire 名 | 职责 |
|---|---|---|
| `bin.dex.disasm` | `bin_dex_disasm` | 反汇编：给定类/方法，输出 code_item 指令流 |
| `bin.dex.xref` | `bin_dex_xref` | 调用图：谁 invoke 了谁、谁引用了哪个 string/type |

两个工具共用同一套解析器（`dex.rs` 内的私有函数），只是「出口视角」不同。

---

## 4. 数据结构（DEX 头部与 section）

DEX 文件头（112 字节）关键字段，偏移从 `header_item` 起：

| 偏移 | 字段 | 类型 | 说明 |
|---|---|---|---|
| 0x00 | magic | 8B | `dex\n035\0`（或 036/037/038/039） |
| 0x20 | checksum | u4 | Adler-32（不校验，仅展示） |
| 0x24 | signature | 20B | SHA-1（不校验，仅展示） |
| 0x38 | file_size | u4 | 文件总长 |
| 0x3c | header_size | u4 | 恒 0x70 |
| 0x40 | endian_tag | u4 | 恒 0x12345678 |
| 0x44 | link_size / link_off | u4×2 | 可忽略 |
| 0x4c | map_off | u4 | map_list |
| 0x50 | string_ids_size / off | u4×2 | 字符串池 |
| 0x58 | type_ids_size / off | u4×2 | 类型描述符池 |
| 0x60 | proto_ids_size / off | u4×2 | 原型池 |
| 0x68 | field_ids_size / off | u4×2 | 字段池 |
| 0x70 | method_ids_size / off | u4×2 | 方法池 |
| 0x78 | class_defs_size / off | u4×2 | 类定义表 |
| 0x80 | data_size / off | u4×2 | 数据区 |

各池条目布局（小端）：

- `string_id_item` = `u4 string_data_off` → `string_data_item = uleb utf16_len + MUTF-8 字节 + 0x00`
- `type_id_item` = `u4 descriptor_idx`（指向 string_id）
- `proto_id_item` = `u4 shorty_idx + u4 return_type_idx + u4 parameters_off`
- `field_id_item` = `u2 class_idx + u2 type_idx + u4 name_idx`
- `method_id_item` = `u2 class_idx + u2 proto_idx + u4 name_idx`
- `class_def_item` = 32 字节（见 §6）

---

## 5. `uleb128` 读取

DEX 全程使用 unsigned LEB128（`uleb128p1` 用于 `index` 编码，即读取后 `-1`）。

```rust
/// 读取一个 uleb128，返回 (值, 下一个位置)。
/// 上限 5 字节（u32 范围），超出即 Err —— 防畸形输入死循环。
fn uleb(bytes: &[u8], at: usize) -> Result<(u32, usize), DexError> {
    let mut result = 0u32;
    let mut shift = 0;
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
```

所有变长读取（`string_data` 长度、`class_data_item` 计数、`encoded_method`/`encoded_field` 的 idx/access/code_off、`debug_info` 等）统一走它。

---

## 6. `class_data_item`（本次 P0 根因所在）

`class_data_item` 结构（注意顺序，**field 条目在 method 条目之前**）：

```
uleb static_fields_size   (sf)
uleb instance_fields_size (inf)
uleb direct_methods_size  (dm)
uleb virtual_methods_size (vm)
encoded_field  × (sf + inf)   // 每项：field_idx_diff(uleb) + access_flags(uleb)
encoded_method × (dm + vm)    // 每项：method_idx_diff(uleb) + access_flags(uleb) + code_off(uleb)
```

**关键纪律（上一轮踩过的坑）**：读完 `sf/inf/dm/vm` 后，**必须先消费 `sf + inf` 个 encoded_field 条目**，才能开始解析 method 条目。漏掉这一步，method 游标整体错位，`code_off` 会读到垃圾值（复现观测为 `1`），进而 `code_item` 解析越界。

实现骨架：

```rust
let (sf, p) = uleb(d, p)?;
let (inf, p) = uleb(d, p)?;
let (dm, p) = uleb(d, p)?;
let (vm, p) = uleb(d, p)?;
// 跳过字段条目：field_idx_diff + access_flags（无 code_off）
for _ in 0..sf + inf {
    let (_, p2) = uleb(d, p)?;
    let (_, p) = uleb(d, p2)?;
}
// 现在才开始读 method 条目
let mut method_idx = 0u32;
for _ in 0..dm + vm {
    let (diff, p2) = uleb(d, p)?;
    let (access, p3) = uleb(d, p2)?;
    let (code_off, p) = uleb(d, p3)?;
    method_idx += diff;
    // 记录 (method_idx, access, code_off)
}
```

`code_off == 0` 表示 abstract / native 方法，**没有 code_item**，属正常，不算失败。

---

## 7. `code_item` 与指令流

`code_item` 结构（`code_off` 指向处）：

| 偏移 | 字段 | 类型 | 说明 |
|---|---|---|---|
| 0x00 | registers_size | u2 | 总寄存器数 |
| 0x02 | ins_size | u2 | 入参寄存器数 |
| 0x04 | outs_size | u2 | 出参寄存器数 |
| 0x06 | tries_size | u2 | try 块数 |
| 0x08 | debug_info_off | u4 | 调试信息 |
| 0x0c | insns_size | u4 | **指令数（以 16-bit code unit 计）** |
| 0x10 | insns | u2 × insns_size | 指令流 |
| ... | tries + handlers | 变长 | try/catch 表（tries_size > 0 时） |

指令是 16-bit 单元的流。解码时维护一个「code unit 指针」，按 opcode 的格式推进。

---

## 8. 指令解码表

DEX 指令按首字节（低 8 位 opcode）查表。格式与操作数从 Android 官方 `dalvik bytecode` 规范照搬。这里列全量格式（覆盖「完整指令级」目标）：

| 格式 | 结构（16-bit 单元） | 典型 opcode |
|---|---|---|
| `10x` | `op` | `return-void`, `nop` |
| `12x` | `op vA vB`（各 4bit） | `move`, `add-int/2addr` |
| `11n` | `op vA #+B`（4bit 立即数） | `const/4` |
| `11x` | `op vAA`（8bit） | `move-result`, `return` |
| `10t` | `op +AA`（8bit 有符号偏移） | `goto` |
| `20t` | `op 0x00 +AAAA` | `goto/16` |
| `22x` | `op vAA vBBBB` | `move/from16` |
| `21t` | `op vAA +BBBB` | `if-eqz` |
| `21s` | `op vAA #+BBBB` | `const/16` |
| `21h` | `op vAA #+BBBB`（高位/低位填充） | `const/high16`, `const-wide/high16` |
| `21c` | `op vAA kind@BBBB` | `const-string`, `new-instance` |
| `23x` | `op vAA vBB vCC` | `add-int`, `cmp` |
| `22b` | `op vAA vBB #+CC` | `add-int/lit8` |
| `22t` | `op vA vB +CCCC` | `if-eq`, `if-ne` |
| `22s` | `op vA vB #+CCCC` | `add-int/lit16` |
| `22c` | `op vA vB type@CCCC` | `instance-of`, `new-array` |
| `22cs` | `op vA vB fieldoff@CCCC`（odex 专用，可忽略） | — |
| `30t` | `op 0x00 +AAAAAAAA` | `goto/32` |
| `32x` | `op 0x00 vAAAA vBBBB` | `move/16` |
| `31i` | `op vAA #+BBBBBBBB` | `const` |
| `31t` | `op vAA +BBBBBBBB` | `fill-array-data` |
| `31c` | `op vAA string@BBBBBBBB` | `const-string/jumbo` |
| `35c` | `op {vC,vD,vE,vF,vG} meth@BBBB` | `invoke-*` |
| `35ms` | `op {vC..} vtaboff@BBBB`（odex） | — |
| `35mi` | `op {vC..} inline@BBBB`（odex） | — |
| `3rc` | `op {vCCCC..} meth@BBBB` | `invoke-*/range` |
| `3rms`/`3rmi` | odex 专用，可忽略 | — |
| `45cc` | `op {vC..} meth@BBBB proto@HHHH` | `invoke-custom`（含 call-site） |
| `4rcc` | `op {vCCCC..} meth@BBBB proto@HHHH` | `invoke-custom/range` |
| `51l` | `op vAA #+BBBBBBBBBBBBBBBB` | `const-wide` |

### 关键 opcode 的语义标注（供 xref 用）

- `invoke-virtual / invoke-direct / invoke-static / invoke-super / invoke-interface`（`35c`/`3rc`）：解析出目标 `method_id` → 作为一条**调用边**。
- `invoke-custom`（`45cc`/`4rcc`）：额外读 call-site，解出 `proto`，目标为 lambda/`invokedynamic`，静态目标可能为空 —— 标注「lambda（目标运行时决定）」。
- `const-string` / `const-string/jumbo`：解析出 `string_id` → 作为一条**字符串引用**。
- `new-instance` / `const-class` / `check-cast` / `instance-of`：解析出 `type_id` → **类型引用**。
- `packed-switch` / `sparse-switch`（`31t`）：记录 `data_off`，payload 解析见 §9。
- `fill-array-data`（`31t`）：记录 `data_off`。

---

## 9. 数据 payload 解析

三种 `data_off` 指向的 payload：

- `packed-switch-payload`：`u2 ident=0x0100 + u2 size + i4 first_key + i4[] targets`
- `sparse-switch-payload`：`u2 ident=0x0200 + u2 size + i4[] keys + i4[] targets`
- `array-data-payload`：`u2 ident=0x0300 + u2 element_width + u4 size + 数据`

switch payload 用于「枚举跳转分支」——这是回答「`onReceive` 的 switch 键表」这类问题的关键。

---

## 10. 输出格式

### `bin.dex.disasm`

```json
{
  "path": "unpack/aerial/classes.dex",
  "size": 3139068,
  "classes": 3850,
  "methods": 20661,
  "class": "Lapp/orchard/tv/guard/BootReceiver;",
  "method": "onReceive(Landroid/content/Context;Landroid/content/Intent;)V",
  "code_off": "0x2dc0ac",
  "registers": 7,
  "ins_size": 2,
  "insns_size": 42,
  "instructions": [
    { "addr": "0x0000", "opcode": "invoke-virtual", "mnemonic": "invoke-virtual {v1, v2}, Landroid/content/Intent;->getAction()Ljava/lang/String;", "xref": "method:...", "offset": "0x2dc0bc" },
    { "addr": "0x0004", "opcode": "const-string", "value": "android.intent.action.BOOT_COMPLETED", "xref": "string:..." }
  ],
  "decode_ok": true
}
```

### `bin.dex.xref`

```json
{
  "path": "...",
  "class": "Lapp/orchard/tv/guard/BootReceiver;",
  "calls": [
    { "from": "...onReceive(...)V", "to": "Lapp/orchard/tv/MainActivity;->...", "op": "invoke-virtual" }
  ],
  "strings": [ ... ],
  "types": [ ... ],
  "switches": [ { "method": "...", "keys": ["export_tiles", "clear_pin", "check_update"] } ]
}
```

---

## 11. 纪律与上限

- `Permission::ReadOnly`、`Risk::Read`、`Network::None`、`ToolScope::Any`、tags `["bin:static"]`、`delegable = true`。
- 读取上限沿用 `SCAN_LIMIT = 64 MiB`（DEX 单文件远超此值极罕见，超了标注 `truncated`）。
- 单方法指令数上限（防巨型方法撑爆输出），默认截断 + 标注。
- **不猜**：解析不出的字段省略；`code_off == 0` 明确标「abstract/native，无 code_item」；遇到 `cdex`/损坏的 header 直接报「不支持/损坏」，不硬解。

---

## 12. 接入点

1. 新建 `src-tauri/src/engines/native/dex.rs`，导出 `pub fn tools() -> Vec<Arc<ToolDef>>`。
2. `engines/native/mod.rs` 加 `pub mod dex;`。
3. `tools.rs` 的 `builtin_tools()` 里加一行 `list.extend(super::dex::tools());`。
4. 复用 `forensics` 的 `read_head` / `read_u32` / `respond` / `target_of`，不重复造轮子。

---

## 13. 测试策略（`#[cfg(test)]`）

用**手工构造的最小 DEX** 做单元测试（照 `analysis.rs` 造 PE 样本的既有范式），覆盖：

- `uleb128` 边界（单字节 / 多字节 / 溢出 / 截断）。
- `class_data_item` 的 field 跳过（**回归上一轮 P0**：构造带 field 的类，断言 `code_off` 正确）。
- `code_item` 头部解析（registers/insns_size 正确）。
- 指令解码：`invoke-*` / `const-string` / `goto` / `packed-switch` 各至少一条。
- `code_off == 0` 的 abstract 方法标「无 code_item」。
- 畸形输入不 panic（truncated header、越界 code_off 返回 `DexError` 而非崩溃）。

真文件回归（`#[ignore]`）：拿 `aerial-guard` 的 `classes.dex` 跑，断言 `decode_ok` 占比 > 99%（对照上一轮 Python 实测 99.98%）。

---

## 14. 工作量估算

| 部分 | 估算 |
|---|---|
| 头部 + 各池解析 | ~80 行 |
| `uleb128` + `class_data_item` | ~50 行 |
| `code_item` + 指令表（全 opcode） | ~200 行 |
| switch/array payload | ~40 行 |
| 两个工具 schema + run + 出口 | ~120 行 |
| 测试 | ~150 行 |
| **合计** | **~650 行，零新依赖** |

---

## 15. 待确认项

- [ ] 工具命名 `bin.dex.disasm` / `bin.dex.xref` 是否认可？
- [ ] 是否需要同时把 DEX 壳检测（`360加固`/`梆梆` 等）加进 `bin.pack.detect`？（本设计先不做，另立）
- [ ] 指令助记符是否要求完全对齐 `baksmali` 的输出格式（含 `p` 前缀、类型描述符原样 vs 可读化）？
