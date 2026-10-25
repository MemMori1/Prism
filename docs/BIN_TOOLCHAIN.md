# 尾部工具对接设计（外部后端 · 反汇编 IR · 沙箱分级）

> 状态：设计稿，待评审后进入实现。
> 范围：`docs/SECURITY_AGENTS.md` §4.1 工具表里剩下的 16 行——5 个要外部程序、
> 4 个要系统级隔离、4 个要反汇编基础设施、3 个可以纯 Rust 先做。
> `bin.crypto.asn1.parse` 与 `bin.crypto.xor_brute` 多字节版已落地（`analysis.rs`），不在本文范围。
> 关联：`docs/ARTIFACTS.md`（制品与数据流）、`docs/RUNTIME_SANDBOX.md`（S0/S1 现状）、
> `docs/CRYPTO_TOOLS.md`、`docs/DEX_PARSER.md`、`docs/AGENTS.md`（工种提示词回填）。

## 0. 一句话

剩下的 16 个工具不是 16 个功能，是**三层地基**：外部后端探测（装没装）、统一的外部进程
执行契约（怎么跑、跑多久、产物放哪）、一份反汇编中间表示（下游工具读它，而不是各自去啃二进制）。
地基铺完，16 项里 11 项只是薄壳；真正的新工程量只有沙箱那 4 项和 Ghidra 的两个导出脚本。

## 1. 盘点：16 项，四个桶

### 1.1 四个桶

| 桶 | 工具 | 卡点 | 本文给什么 |
|---|---|---|---|
| **A 外部程序**（5） | `bin.unpack.upx`、`bin.dotnet.deobfuscate`、`bin.ghidra.analyze`、`bin.ghidra.decompile_func`、`bin.radare2.scan` | 本机没有可执行文件，或版本不对 | 后端探测 + 执行契约 + 产物落盘 |
| **B 反汇编下游**（4） | `bin.cfg.extract`、`bin.crypto.hash_identify`、`bin.vm.detect`、`prism.text.extract` | 没有函数边界与基本块 | 一份 `ProgramIndex`（IR），Ghidra / radare2 / DEX 三种来源映射进同一形状 |
| **C 沙箱**（4） | `bin.dyn.sandbox.run`、`.mem.dump`、`.trace.breakpoint`、`bin.unpack.dumper` | 没有能跑样本的隔离环境 | 沙箱分级 S0–S3 + `SandboxProvider` + 一份 QEMU/ELF 落地路径 |
| **D 纯 Rust**（3） | `bin.dyn.syscall.filter`、`bin.yara.rule_gen`、`bin.report.rev` | 只是没排期 | 设计 + 提前到 P2.5 |

合计 16。`bin.crypto.asn1.parse` 原在 P3，已在 `analysis.rs` 落地，从表里划掉。

### 1.2 归类修正（四条，都有依据）

| 原归类 | 实际归到 | 依据 |
|---|---|---|
| `bin.vm.detect` 归沙箱 | **B（IR）** | 它读的是分发器结构、处理表、上下文访问模式，不跑样本。`bin.pack.detect` 已经覆盖"是不是 VMProtect/Themida"的弱特征；剩下必须看基本块。 |
| `prism.text.extract` 归反汇编下游 | **B 的独立支线** | 输入是伪 C **文本**，不是二进制。Ghidra 没装也能对用户贴的伪 C、上一轮的 `pseudocode` 制品做清洗，可以先发。 |
| `bin.dyn.syscall.filter` 归沙箱 | **D（日志后处理）** | 输入是 syscall 日志，沙箱没到位也能先用 fixture 驱动实现并测通。 |
| `bin.crypto.hash_identify` 整体算"要反汇编" | **两阶段** | P1 常量识别不需要 IR（扩 `crypto.scan` 的签名表就能发）；只有"循环结构确认"才要基本块。 |

修正之后，**能脱离外部程序先做的从 2 个变成 4 个**（`text.extract` 的 strip/chunk、
`syscall.filter`、`yara.rule_gen`、`report.rev`）。

### 1.3 为什么不让模型自己 `run_command` 调 ghidra

技术上能。不这么做有四个理由，都是既有设计里已经吃过亏的：

1. **没有探测**：模型会照着记忆写 `ghidra` / `r2` 的命令，失败后开始编替代方案；
2. **没有版本约束**：`aflj` 的字段名在 r2 4.x/5.x 之间变过，解析器会静默读到空数组；
3. **没有产物与血缘**：`upx -d` 就地写文件，样本目录被污染，下游拿不到 `parent_id`；
4. **不可审计**：跑过什么、跑了多久、退出码多少，全在对话文本里。

所以外部程序一律**只经工具调用**，`run_command` 不引导用于反汇编/脱壳（工种提示词里写死）。

---

## 2. 三层地基

```text
16 个工具
  ├─ 要外部程序 ──► backends::Registry ──► exec::run(Job) ──► 产物落 artifact
  │                    (探测/版本/路径)      (超时/上限/进程组/env)
  ├─ 要反汇编  ──► ProgramIndex（IR）◄── ghidra / radare2 / dex
  │                    (缓存按 sha256)      └─► cfg / hash_identify / vm.detect
  └─ 要跑样本  ──► SandboxProvider（S0…S2）──► dyn.* / unpack.dumper
```

三条约束：

- **IR 是唯一契约**：下游工具只认 `ProgramIndex`，不认 Ghidra 的 JSON 也不认 r2 的 `aflj`。
  换后端不改下游，这是三个 IR 工具能一次写完的前提。
- **探测结果不落盘成"事实"**：只缓存路径覆盖（用户选的），版本每次启动重探。
  机器上换了 Ghidra 版本，Prism 不能还按老版本解析。
- **沙箱能力决定工具是否可见**：不是运行时才拒绝，而是**根本不下发 schema**——
  与现有 `scope=dag` 的做法一致（`ARTIFACTS.md` §6.1）。

---

## 3. 数据模型

### 3.1 后端登记（`engines/native/backends/mod.rs`）

```rust
pub struct Backend {
    pub id: &'static str,              // ghidra | radare2 | upx | dotnet
    pub name: &'static str,            // Ghidra / radare2 / UPX / .NET 工具链
    pub min_version: Option<&'static str>,
    pub tools: &'static [&'static str], // 受影响工具，设置页直接列
    pub install: &'static str,          // 一句安装指引（可复制）
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendState {
    Unknown,                            // 还没探过（启动后台线程之前）
    Ready,
    Missing,
    TooOld { found: String, need: String },
    Broken(String),                     // 找到了但跑不起来（缺 JDK / 无执行权限）
}

pub struct BackendStatus {
    pub id: &'static str,
    pub state: BackendState,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    pub source: ProbeSource,            // Env | Toolchains | Path | KnownDir | Bundled
    pub note: Option<String>,
}
```

`BackendStatus` 只在内存里（`RwLock<HashMap<&'static str, BackendStatus>>`），
启动时后台线程探一遍；`toolchain_probe` IPC 可手动重探。**不落盘**，理由见 §2。

### 3.2 工具声明扩展（`tools.rs::ToolMeta`）

```rust
pub struct ToolMeta {
    // ...原有 tags / risk / network / sandbox / scope
    pub needs: &'static [Need],
}

pub enum Need {
    Backend(&'static str),              // 后端 Ready 才可见
    // Sandbox(SandboxTier) —— P3 有真实 S2 时再加；
    // 现在加一个恒假的变体，只会让人以为沙箱已经接了一半。
}
```

- `ToolMeta::default()` 给 `needs: &[]`，现有 15 处字面量补 `..ToolMeta::default()` 即可；
- `visible()`（`tools.rs:1036`）在开关/权限/作用域之后再过滤一次 `needs_met()`；
- **`tools.list` 不过滤**：它要能回答"我为什么没有 Ghidra 工具"，输出里带
  `"available": false, "reason": "需要 Ghidra（未检测到）"`。
  自省工具被裁掉，等于让模型在最需要自查的时候失去自查手段（同 `SECURITY_AGENTS.md` §4.1 的既有判断）。

### 3.3 反汇编 IR（`backends/index.rs`）

```rust
pub struct ProgramIndex {
    pub schema: u32,                    // 1
    pub backend: String,                // ghidra | radare2 | dex
    pub backend_version: String,
    pub sample: SampleRef,              // sha256 / size / format / arch / bits / image_base
    pub functions: Vec<FunctionEntry>,
    pub imports: Vec<ImportEntry>,      // 可选：后端顺手给就带，不强制
    pub truncated: bool,
    pub note: Option<String>,
}

pub struct FunctionEntry {
    pub va: u64,
    pub name: String,
    pub size: u32,
    pub kind: String,                   // normal | thunk | import | plt | entry
    pub blocks: Option<Vec<BlockEntry>>, // 按需 detail pass 才有
}

pub struct BlockEntry {
    pub va: u64,
    pub size: u32,
    pub succ: Vec<u64>,                 // 直接后继（fallthrough + jump）
}
```

三个后端映射到同一形状：

| IR 字段 | Ghidra（导出脚本） | radare2（`aflj` / `afbj`） | DEX（`dex.rs` 已有） |
|---|---|---|---|
| `va` | `Function.getEntryPoint()` | `offset` | `code_off`（DEX 没有 VA） |
| `name` | `Function.getName()` | `name` | `Lpkg/Cls;->method` |
| `size` | `getBody().getNumAddresses()` | `size` | 方法字节码长度 |
| `blocks` | `BasicBlockModel` 遍历 | `afbj` 的 `addr/size/jump/fail` | 基本块按分支指令切 |
| `imports` | `SymbolTable` 的 EXTERNAL | `rabin2 -Ij` 或 `iij` | `method_ids` 外部引用 |

DEX 没有虚拟地址，IR 里的 `va` 用 `code_off` 顶替，`note` 写明这一点：
下游工具只把它当**不透明 id**（能相等、能画图），不假设可重定位。

`bin.dex.disasm` 已经能做代码级反汇编，所以 **DEX 线不依赖任何外部程序**——
IR 先支持 dex，`cfg.extract` 就能先对 APK 生效（这是本期最划算的一步）。

### 3.4 任务（`backends/job.rs`）

Ghidra 分析一个 10 MB 的 PE 可能要几分钟，不能一直占着回合。

```rust
pub struct Job {
    pub id: String,                     // job-20261024-153012-001
    pub tool: String,
    pub backend: String,
    pub state: JobState,                // pending | running | done | failed | cancelled
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub progress: Option<String>,       // 后端输出里能认出的阶段（"分析 42%"）
    pub artifacts: Vec<String>,
    pub log_tail: Vec<String>,          // 最近 200 行，失败时给模型看
}
```

规则：

- 每次调用**总是**建 job（统一一条路径），`wait=true` 只是"最多等 N 秒"；
- 聊天里超时 → 返回 `jobId` + "还在跑，用 `bin.job.status` 查"；
- **DAG 节点里强制 wait**：节点结束即产出，后台任务没有归属（会话关了谁去收）；
- 取消走 `should_stop`，每 100ms 查一次，整组 SIGTERM → 900ms → SIGKILL（复用 `tasks.rs` 的做法）。

新增两个自省工具（不在 16 项里，这里明说）：`bin.job.status`、`bin.job.cancel`。

### 3.5 磁盘布局

```text
~/.prism/
  toolchains/index.json          # 只存用户覆盖：{"ghidra":{"path":"/opt/ghidra_11.2"}}
  cache/programs/<sha256>/
    index.json                   # ProgramIndex（带 backend + version，不匹配即重分析）
    pseudocode/<va>.c            # 反编译缓存
  jobs/<job_id>/
    job.json  stdout.log  stderr.log  work/  out/
~/Library/Caches/prism/ghidra-projects/<sha256>/   # Ghidra 项目目录（decompile 要复用，不能删）
```

- 缓存键：`sha256 + backend + backend_version`；三者任一变化 → 重分析；
- **Ghidra 项目目录不放 `~/.prism`**：Ghidra 的 GhidraURL 校验拒绝 `.` 开头的
  路径段（`Path element starting with '.' is not permitted`），`~/.prism` /
  `~/.cache` 这类隐藏缓存目录全被它挡住——macOS 用 `Library/Caches/prism`，
  Windows 用 `LOCALAPPDATA/prism`，Linux 用 `/var/tmp/prism`（XDG 缓存同样带点）；
- 淘汰：`cache/programs` 总量上限 **8 GiB**，按 `index.json` 的 mtime LRU 删；
  `jobs/` 只留最近 20 个（与 `runtime` 沙箱目录同一策略）；
- 清理在启动时和每次 job 结束后各跑一次，不做定时器。

---

## 4. 后端探测

### 4.1 查找顺序

从"用户显式指定"到"常见安装位"，命中即停：

1. `~/.prism/toolchains/index.json` 里的 `path`（用户在设置里选的，最高优先）；
2. 环境变量：`PRISM_GHIDRA_HOME` / `GHIDRA_HOME`、`PRISM_R2`、`PRISM_UPX`、`PRISM_ILSPYCMD`；
3. 内置目录 `~/.prism/toolchains/<id>/`（以后做"Prism 自带工具链"时用，现在空着）；
4. `PATH`（复用 `runtime::which`）；
5. 已知安装位：`/Applications/ghidra_*/support/analyzeHeadless`、`/opt/homebrew/bin/r2`、
   `~/.dotnet/tools/ilspycmd`。

**不扫全盘**。找不到就是找不到，报未接入，比扫 30 秒然后超时诚实。

### 4.2 探测表

| 后端 | 探测命令 | 版本解析 | 最低版本 | 额外检查 |
|---|---|---|---|---|
| ghidra | `<home>/support/analyzeHeadless` 存在且可执行 | `<home>/Ghidra/application.properties` 的 `application.version=` | 11.0 | `java -version`（stderr）主版本 ≥ 17；11.2 起要 21，版本不足给提示不拦 |
| radare2 | `r2 -v` | 输出第二列 `radare2 5.9.8 ...` | 5.5 | 无（`aflj` 字段在 5.x 稳定） |
| upx | `upx --version` | `upx 4.2.4` 第二列 | 3.9 | 无 |
| jadx | `jadx --version` | 输出即版本号（`1.5.6`） | 1.4 | 无（1.4 起 `--single-class` 稳定） |
| dotnet | `ilspycmd --version`（dotnet tool） | 整行即版本号 | 8.0 | `dotnet --list-sdks` 非空；`de4dot` 可选，探测到才启用 T2 |

版本比较只做"主.次"数值比较，不做 semver 全序——我们只需要拦"太老"，不需要精确排序。

### 4.3 时机

| 时机 | 行为 |
|---|---|
| 应用启动 | 后台线程探一遍（不阻塞窗口），完成后广播 `toolchain-changed` |
| 设置页「重新检测」 | `toolchain_probe` 同步重探（单后端 < 2s） |
| 工具调用前 | 只在状态为 `Unknown` 时补探一次；`Missing` 直接报错，不每次 `which` |

### 4.4 未接入的语义（三处一致）

1. **schema 不发**：模型看不到 `bin.ghidra.*`，不会幻觉调用；
2. **强行调用报明确错误**：历史里翻出旧调用时，错误文案带安装指引（§11）；
3. **`tools.list` 照列**，带 `available: false` 与原因。

---

## 5. 外部进程执行契约（`backends/exec.rs`）

所有外部程序共用一条路径，不允许各写各的 `Command::new`。

### 5.1 启动

```rust
pub struct ExecSpec {
    pub program: PathBuf,
    pub args: Vec<String>,        // argv 数组，绝不拼 shell
    pub cwd: PathBuf,             // ~/.prism/jobs/<id>/work
    pub env: Vec<(String, String)>, // 在 sanitized_env 之上叠加
    pub timeout: Duration,
    pub stdout_cap: usize,
    pub stderr_cap: usize,
}
```

- **不用 `sh -c`**：样本文件名里带 `;` `$()` 也只是参数。这条比任何转义都可靠；
- `env` = `guard::sanitized_env()` + 后端需要的最小集（`JAVA_HOME`、`GHIDRA_HOME`、
  把工具目录前置进 `PATH`）；密钥仍然一个都不给；
- 样本路径先 `resolve_path` + `canonicalize`，必须落在会话目录内，符号链接逃逸拒绝
  （复用 `forensics::target_of` 的规则，多一步 canonicalize）。

### 5.2 上限与超时

| 后端 | 默认超时 | 上限 | stdout 上限 | stderr 上限 |
|---|---|---|---|---|
| upx | 120s | 300s | 256 KiB | 256 KiB |
| radare2 | 300s | 900s | 8 MiB | 512 KiB |
| ghidra.analyze | 1800s | 3600s | 2 MiB | 2 MiB |
| ghidra.decompile_func | 120s | 300s | 1 MiB | 512 KiB |
| ilspycmd | 600s | 1800s | 2 MiB | 1 MiB |

超限保留**头 64 KiB + 尾 512 KiB**，中间写一行 `…（截断 N 字节）…`。
只留尾部会让 JSON 解析从头就崩，只留头部会让"到底哪一步失败"消失。

### 5.3 并发与去重

- 全局信号量 **2**；Ghidra 独占 **1**（一个 JVM 起步就吃 1 GB，开两个用户机器会卡）；
- 同一 `sha256 + backend` 正在跑时，第二个调用**等第一个**（singleflight），不重复分析；
- 排队超过 30s 的调用返回"前面还有 N 个分析在跑"，而不是静默挂着。

### 5.4 输出解析失败

外部程序输出是**不可信文本**。规则：

- JSON 解析失败 → 原文落制品（key `<key>_raw`，kind `text`），工具返回 `fail`，
  附前 200 字符与"未做猜测解析"；
- 退出码非 0 但 stdout 有内容 → 两者都保留，结论按失败算；
- **不重试**。外部程序失败通常不是抖动（文件不对、版本不对、超时），
  自动重试只会让用户多等一倍。

---

## 6. 逐工具对接

### 6.1 A 桶：外部程序（6 个；jadx 为后补，不在最初 16 项里）

#### `bin.unpack.upx`

```text
upx -t <sample>                     # 先验证是不是标准 UPX
upx -d -o <job>/out/<name>.unpacked <sample>
```

- 先调**已有的** `bin.pack.detect` 判断是不是 UPX？不——工具之间不互相调用（模型自己会串），
  这里只做 `upx -t`，失败就把 UPX 的原话给出去；
- **绝不 `--force`**：改过头的样本硬解出来是垃圾，还会让人以为脱成功了。失败文案引导到
  `bin.dyn.mem.dump`（§6.3）；
- 产物：`key=unpacked`、`kind=binary`、`parent_id=原样本制品`、`name=<原名>.unpacked`；
- 落盘只在 `jobs/<id>/out/`，样本目录一个字节都不写；
- 元信息（`Permission::ReadOnly`、`Risk::Read`、`Network::None`）——它不执行样本，也不碰工作区。

#### `bin.radare2.scan`

```text
r2 -q -e scr.color=0 -e bin.cache=true -c 'aaa; aflj' <sample>
```

- 解析 `aflj` 数组 → `ProgramIndex`（映射见 §3.3）；函数数超 20000 标 `truncated`；
- **不做伪 C**：`pdc` 的输出质量不足以支撑结论，给了模型就会当真。要伪 C 就走 Ghidra；
- 用途有两个：Ghidra 没装时的降级后端；Ghidra 装了也能做交叉校验（函数数差 30% 说明有代码没识别）；
- 产物：`key=functions`。

#### `bin.ghidra.analyze`

```text
<ghidra>/support/analyzeHeadless <cache>/ghidra <projName> \
  -import <sample> \
  -scriptPath <resources/ghidra> \
  -postScript PrismExport.java <out>/functions.json \
  -analysisTimeoutPerFile 900 -max-cpu 2
```

- 项目目录**保留**（`decompile_func` 要复用）；缓存淘汰时才删；
- 导出脚本随应用打包（`src-tauri/resources/ghidra/PrismExport.java`），
  用 `-scriptPath` 指过去，**不往用户的 Ghidra 脚本目录写东西**；
- 脚本只做三件事：遍历函数（entry/name/size/是否 thunk）、导出基本块（va/size/后继）、
  导出外部符号；不做反编译（那是下一个工具）；
- 大样本超时 → 返回"分析超时（900s），已导出前 N 个函数"，标 `truncated`，不判失败；
- 产物：`key=functions`。

#### `bin.ghidra.decompile_func`

```text
<ghidra>/support/analyzeHeadless <cache>/ghidra <projName> \
  -process <programName> -noanalysis \
  -scriptPath <resources/ghidra> \
  -postScript PrismDecompile.java <va> <out>/<va>.c
```

- 没有项目缓存 → 先跑一次 analyze（一次，不递归），并在结果里说明"为反编译先做了一次全量分析"；
- `DecompInterface` 单函数超时 60s；失败给 Ghidra 的原话（"low-level error" 也要原样给）；
- 参数 `va` 接受 `0x401000` / 十进制 / 函数名（名字走 `getFunction(name)`，找不到报"没有这个函数"）；
- 产物：`key=pseudocode`，`name=<va>.c`；同时写 `cache/programs/<sha>/pseudocode/<va>.c`。

#### `bin.dotnet.deobfuscate`

分两层，装了什么给什么：

| 层 | 依赖 | 做什么 | 产出 |
|---|---|---|---|
| T1 反编译 | `ilspycmd -p -o <out> <dll>` | 类型/方法还原成 C# 项目 | `key=il`（目录打包成 zip，kind binary） |
| T2 去混淆 | `de4dot <dll> -o <out>`（可选） | 重命名表还原、常见字符串解密 | `key=unpacked`，`parent_id` 指向原样本 |

- **不自研 dnlib CLI**：要维护一个 C# 工程 + 跟无穷的混淆变体，收益远小于直接调 ILSpy；
  `dnlib` 只在"以后要写自己的静态改写器"时才进来；
- T2 是**静态改写**（dnlib 读模块不执行代码），但仍然标 `Risk::Write`、`Network::None`；
- 只支持 PE/.NET 程序集；不是 .NET 就报"这不是 .NET 程序集（没有 CLR 头）"。

#### `bin.jadx.decompile`

```text
jadx -d <workdir>/_out/jadx --no-res [--single-class <完全限定类名>] <sample>
```

- **解决的是"读不出语义"这一层**：DEX 指令级（`bin.dex.disasm`）能说明"调了谁"，
  读不出"想干什么"；R8 混淆后类名是 `LA;` / `LW/a;`，控制流看懂了也没法给行为命名。
  jadx 出的是可读 Java，功能还原度由此上一档；
- `--single-class` **只认完全限定类名**（`app.orchard.tv.guard.HomeGuardService`），
  短名会报 `Input class not found` 退出 1。schema 里写死这条，失败文案翻译成中文并指路
  `bin.dex.disasm`（只填 path 就能列类名）；
- 默认 `--no-res`：资源反编译对代码理解没帮助，却会把产物目录塞满 XML 噪音；
- 重跑先清 `_out/jadx/`：新旧混在一起会让模型读到过期类；
- 只读样本（`Permission::ReadOnly`、`Risk::Read`、`Network::None`），样本原件不动；
- 安装：`brew install jadx` 会拖一个 300 MB+ 的 openjdk 依赖（实测卡在下载），
  第二条路是把 release 解压到 `~/.prism/toolchains/jadx/`，入口包一层壳脚本指定
  `JAVA_HOME`——macOS 的 `/usr/bin/java` 是 stub，直接跑 `bin/jadx` 会报
  "Unable to locate a Java Runtime"。

### 6.2 B 桶：IR 下游（4 个）

统一前置：`index_of(ctx, path)` → 查缓存 → 没有就按可用后端生成
（`ghidra` 优先，其次 `radare2`，DEX 走 `dex.rs`）→ 一个都没有就报"需要 Ghidra 或 radare2"。

#### `bin.cfg.extract`

- 输入：`path`、`function`（可选，va 或名字）、`format`（`json|dot|mermaid`，默认 json）；
- `blocks` 为空 → 触发一次 detail pass（Ghidra 遍历 BasicBlockModel / r2 `afbj`），结果写回缓存；
- 上限：函数 200（最多 2000）、边 5000；超了标 `truncated` 并说明；
- 输出：

```json
{
  "function": "sub_401000", "va": "0x401000",
  "blocks": [{ "va": "0x401000", "size": 24, "succ": ["0x401018", "0x401030"] }],
  "edges": [["0x401000", "0x401018", "jump"]],
  "backend": "ghidra"
}
```

- dot / mermaid 是**渲染用**，节点 id 一律 `b_<va>`，不塞函数名（名字里有引号会炸图）。

#### `bin.crypto.hash_identify`

两阶段，第一阶段现在就能做：

| 阶段 | 依赖 | 判据 | 置信度 |
|---|---|---|---|
| P1 常量 | 无（扩 `analysis.rs::SIGNATURES`） | MD5/SHA1/SHA256/SHA512 初始向量、K 表、CRC32 多项式 | high |
| P2 循环确认 | IR + blocks | 含常量的函数里有 64/80 轮循环、4 个 32 位状态变量、消息扩展结构 | certain |

- P1 与 `bin.crypto.scan` 共用一张表，不重复实现；`scan` 报"有 SHA-256 常量"，
  `hash_identify` 报"哪个函数、几轮、像不像真的实现"；
- 输出 `{algorithm, confidence, va, function, rounds, evidence}`；
- P2 命中不了的（比如只有常量没有循环）如实写 `loopConfirmed: false`，confidence 停在 `high`。

#### `bin.vm.detect`

只报"虚机保护结构"，不重复报壳名（那是 `bin.pack.detect` 的活）：

| 信号 | 判据 | 权重 |
|---|---|---|
| 分发器 | 单个基本块后继 ≥ 8，且后继都回到同一头部 | 强 |
| 处理表 | 间接跳转经过 ≥ 16 项的地址表 | 强 |
| 上下文 | 多个 handler 读写同一个结构体偏移集合 | 强 |
| 反调试/反 VM | `rdtsc` 差值、`cpuid` 叶子检查、`IsDebuggerPresent` | 中 |
| 节名/壳名 | 交给 `pack.detect`，这里只引用不判定 | 弱 |

三项强信号全中 → `high`；两项 → `medium`；一项 → `low` 并写"可能是普通状态机（如解析器）"。
**误报代价**：把状态机说成 VMProtect，会让人放弃静态分析，所以宁可低一档。

#### `prism.text.extract`

不吃反汇编器，只吃文本（`path` 或 `artifact:<id>`）：

| op | 做什么 | 上限 |
|---|---|---|
| `strip` | 去 Ghidra 噪声：`/* WARNING ... */`、`DAT_00401000`、`undefined4`、行首地址注释 | 64 MiB 输入 |
| `chunk` | 按 `// Function: name @ 0x...` 或函数签名行切块，每块 ≤ 4 KiB | 500 块 |
| `summary` | 每函数首 20 行 + 调到的 API 清单 | 200 函数 |

输出 `{chunks:[{name,va,text}], dropped:{comments,lines}, truncated}`。
`strip` 只删注释与地址前缀，**不动控制流**——删出语法错误的伪 C 比不删更糟。

### 6.3 C 桶：沙箱（4 个）

#### 分级

| 级 | 手段 | 平台 | 能跑什么 | 现状 |
|---|---|---|---|---|
| **S0** | 策略层（命令检测 / 受保护路径 / 环境清洗） | 三平台 | 不跑样本 | ✅ 已有（`guard`） |
| **S1** | seatbelt（写限工作目录 + 默认禁网） | macOS | 自测脚本、无害样本 | ✅ 已有（`runtime`） |
| **S2** | 轻量 VM + 快照回滚 + 只读样本挂载 | macOS(HVF) / Linux(KVM) | 真恶意样本 | ⬜ 本文设计 |
| **S3** | 远端沙箱集群 | — | 批量 | ❌ 不做 |

**规则**：`bin.dyn.*` 与 `bin.unpack.dumper` 需要 **S2**（`bin.dyn.syscall.filter` 除外——
它是纯后处理，只要日志在就能跑）；S1 只允许"用户明确说这是自测样本"，
且结果头必须写 `沙箱：seatbelt（不是恶意样本隔离）`。这与现有"降级要如实说"的纪律一致
（`RUNTIME_SANDBOX.md` §5）。

#### Provider 接口

```rust
pub trait SandboxProvider {
    fn id(&self) -> &'static str;            // qemu-linux
    fn tier(&self) -> SandboxTier;           // S2
    fn probe(&self) -> SandboxStatus;        // 镜像在不在、qemu 在不在
    fn run(&self, spec: &RunSpec, on: &mut dyn FnMut(SandboxEvent)) -> Result<RunResult, String>;
}

pub struct RunSpec {
    pub sample: PathBuf,
    pub argv: Vec<String>,                   // 默认 ["./sample"]
    pub timeout: Duration,                   // 默认 30s，上限 300s
    pub network: bool,                       // 默认 false
    pub profile: String,                     // linux-x64 | win10-x64（P3.2）
    pub watch: Watch,                        // syscalls | pcap | proctree | all
}
```

#### 落地路径（P3.0，只承诺 Linux ELF）

- **QEMU + 预建 Alpine rootfs**（不是 Virtualization.framework）：一套代码在 macOS(HVF) 与
  Linux(KVM) 都能跑，代价是启动慢 1–2 秒——反正每次都是从快照起；
- **快照回滚**：`base.qcow2` + 每次运行的 `overlay.qcow2`（backing file），跑完删 overlay，
  宿主与上一次运行都不留痕；
- **文件进出**：`virtfs` 挂载，样本目录**只读**，`out/` 可写；不放 9p 写回样本目录；
- **guest agent**：一个静态链接的小程序，走 vsock 收 JSON 行（`run` / `dump` / `bp` / `kill`），
  宿主侧不解析 guest 的 stdout 语义，只转发事件；
- **网络**：`-netdev user,restrict=on`（无出站路由），pcap 在 QEMU 的 `-object filter-dump` 上抓，
  不需要 guest 里再装 tcpdump；
- **syscall**：guest 内 `strace -f -qq -e trace=file,network,process` → JSONL；
- **PE 样本**：P3.0 明确不支持（"PE 动态分析需要 Windows guest，还没做"），不拿 Wine 糊弄——
  Wine 下跑出来的行为既不是 Windows 也不是 Linux，结论没法用。

#### 各工具行为

| 工具 | 入参 | 产出 | 备注 |
|---|---|---|---|
| `bin.dyn.sandbox.run` | `path`、`timeoutSec`(30/300)、`network`(false)、`profile`、`argv` | `pcap`、`syscalls`、`proctree`、`report` | 返回 `runId`，VM 租约保留 **10 分钟**供后续 dump/trace |
| `bin.dyn.syscall.filter` | `runId` 或 `artifact:<id>`、`focus`（file/net/inject/crypto/registry） | `syscalls_focus` | 纯后处理，**不依赖 S2**（见 §6.4），列在这里只是同族放一起 |
| `bin.dyn.mem.dump` | `runId`、`when`（`exit`/`breakpoint`/`signal`/`afterSec`）、`region`（module/heap/all） | `memdump` + `regions` | 租约过期 → "这次运行的 VM 已经销毁，重新跑一次" |
| `bin.dyn.trace.breakpoint` | `runId`、`breakpoints`（module!symbol 或 va）、`actions`（regs/args/mem）、`maxHits`(64) | `trace`（JSONL） | 断点由 guest 内 gdb 执行；有 `rr` 时额外给可回放记录 |
| `bin.unpack.dumper` | `runId` 或 `path`（自己跑一次）、`strategy`（oop/iat/section）、`oep` | `unpacked` + `parent_id` | OEP 三种找法：断点停在 OEP、IAT 重建后 dump、按节特征 dump |

#### 闸门与审计

- 每个 `bin.dyn.*` 调用都走 `approval::gate`，动作 `malware`，卡片标题
  「在隔离沙箱里运行样本（默认断网）」；
- **不提供"总是允许"**：这是与现有"按会话+动作记住"的唯一一处刻意偏离——
  样本是会变的，第二次跑的不是同一个东西；
- 审计写 `~/.prism/audit/tools.jsonl`（append-only）：
  `{ts, session, node, tool, sampleSha256, sandboxTier, network, exit, durationMs, artifacts}`；
  16 MiB 轮转一份；**不提供读审计的工具**，用户自己在 Finder 里看；
- `dynamic-sandbox` 工种的提示词要改：现在写的是"本机当前没有真实隔离沙箱"，
  S2 可用后改成"先 `tools.list` 看有没有 `bin.dyn.*`；有就用它，没有就只做只读准备"。

### 6.4 D 桶：纯 Rust（3 个）

| 工具 | 输入 | 输出 | 为什么能提前 |
|---|---|---|---|
| `bin.dyn.syscall.filter` | syscall 日志（`artifact:<id>` 或 fixture） | 聚焦后的 JSONL | 纯日志后处理，不需要 VM；先按日志 schema 实现并测通 |
| `bin.yara.rule_gen` | `path`（+ 可选 `artifact:<id>` 的 strings/pack 结果） | `.yar` 文本制品 | 只用已有工具产出的证据：3 条最独特的字符串、节名、哈希、导入特征 |
| `bin.report.rev` | 同 DAG 的制品（meta/hashes/strings/pack/imports/crypto/functions/pseudocode） | Markdown 报告 | 模板渲染，不解析二进制 |

- `yara.rule_gen` 的规则必须**可读**：`meta` 段写 sample hash 与生成时间，
  `strings` 段每条带来源偏移注释；不生成"全文件哈希匹配"这种一次性规则；
- `report.rev` 缺哪节写"未采集（缺 `bin.ghidra.analyze` 产出）"，**不编**；
- 三个都 `Risk::Read`（`report.rev` 只读制品，`rule_gen` 只读样本 + 写新制品，
  `syscall.filter` 只读日志）。

---

## 7. 可见性、IPC 与界面

### 7.1 过滤链（在现有基础上加一层）

```text
enabled（开关）→ permission（权限档）→ scope（dag/any）→ needs（后端/沙箱）
```

顺序不动，`needs` 放最后：前面的失败原因是"你关了/权限不够"，后面才是"机器上没装"。

### 7.2 IPC

| 命令 | 参数 | 返回 |
|---|---|---|
| `toolchain_list` | — | `BackendView[]`（状态/路径/版本/受影响工具） |
| `toolchain_probe` | `id?` | 重探后的 `BackendView[]` |
| `toolchain_set_path` | `id`, `path?`（空 = 清除覆盖） | `BackendView` |
| `job_list` | — | `Job[]`（进行中 + 最近 20 条） |
| `job_cancel` | `id` | `Job` |

`ToolView` 加两个字段：`available: bool`、`reason: Option<String>`（设置页标灰 + 一行原因）。

### 7.3 设置页「工具链」

- 每行：状态点（绿/灰/黄）、名称、版本、路径（可点「选择…」）、受影响工具数；
- 未接入的行给一句可复制的安装指引（`brew install radare2` / Ghidra 下载页 + 设 `GHIDRA_HOME`）；
- 底部一行：「这 N 个工具因为缺后端不可用」——数字从 `toolchain_list` 聚合，不写死。

前端改动只在设置页 + `types.ts` + `dev-preview-fake.ts`，不动工具清单的布局。

---

## 8. 产物 key 与血缘

| key | 产出工具 | kind | parent | 说明 |
|---|---|---|---|---|
| `functions` | ghidra.analyze / radare2.scan / dex | json | 样本 | `ProgramIndex` |
| `pseudocode` | ghidra.decompile_func | text | 样本 | 单函数伪 C |
| `cfg` | cfg.extract | json | 样本 | 单函数或全量图 |
| `unpacked` | unpack.upx / dumper / dotnet T2 | binary | **样本** | 脱壳产物，血缘必填 |
| `il` | dotnet T1 | binary | 样本 | 反编译项目 zip |
| `pcap` / `syscalls` / `syscalls_focus` / `proctree` | dyn.* | binary/json | 样本 | 一次运行一组 |
| `memdump` / `trace` | dyn.mem.dump / trace.breakpoint | binary/jsonl | 样本 | 大文件，别 inline |
| `rules` | yara.rule_gen | text | 样本 | `.yar` 原文 |
| `rev_report` | report.rev | text | — | 报告。**不叫 `report`**：那是节点默认产出 key，同名会被节点主产出遮蔽 |

统一纪律（沿用 `ARTIFACTS.md` §4）：样本只读、`sha256` 必存、派生件必带 `parent_id`、
超配额报错不截断。

---

## 9. 安全红线（`SECURITY_AGENTS.md` §6 五条之外，这里补六条）

1. 外部程序**一律 argv 调用**，不拼 shell；样本路径 canonicalize 后必须在会话目录内；
2. 外部程序**不出网**：`Network::None` 是声明也是断言——exec 层在子进程环境里删掉
   `http_proxy`/`https_proxy`，并在 job 结果里记录"未联网"；
3. `bin.dotnet.deobfuscate` 不做 `dotnet restore`（会联网拉包），缺依赖就报错让人自己装；
4. 动态分析**只在 S2**；S0/S1 下工具不进 schema，强行调用报错（§11）；
5. 样本目录零写入：所有中间文件在 `jobs/<id>/`，产物只进制品库；
6. 高风险调用全部落审计，且**不可通过工具读回**（防模型自我清理痕迹）。

---

## 10. 实施顺序与验收

### P2.0 地基（✅ 已落地，~900 行）

`backends/{mod,probe,exec}.rs` + `ToolMeta.needs` + 过滤链 + `toolchain_*` IPC。

落地时的两处调整（都比原计划更小）：

- **没有单独的 `job.rs`**：作业目录就是 `backends::JobDir`（40 行，drop 自删），
  只有 Ghidra 那种"分析几分钟、产物要留"的场景才需要持久任务表，随 P2.3 一起加；
- **后端表按工具落地逐条加**（upx → radare2 → ghidra）：`dotnet` 在它自己的工具落地时
  再进表。现在登记一个没有任何工具用的后端，设置页只会多一行看不懂的东西。

**验收**：本机没装 radare2 时，`bin.radare2.scan` 不在 schema 里；
`tools.list` 列出它并写"需要 radare2（未检测到）"；手动调用报同一条原因；
设置页能看到已登记后端的状态。用假后端脚本（临时目录里的 `upx` 打印版本）跑通探测优先级
——`unpacks_through_a_fake_backend` 走完了探测 → `-t` → `-d` → 读产物整条链路。

### P2.1 `bin.unpack.upx`（✅ 已落地，第一个端到端）

最小的一个：一个二进制、一个产物、一个 parent。跑通就等于验证了探测 + 执行 + 制品 + 血缘。

落地时的两个决定：

- **聊天里也有交付物**：没有 DAG 作用域时产物落 `~/.prism/unpacked/<sha 前 12 位>-<原名>`，
  留最近 20 个。否则这个工具在聊天里只能报一个哈希，等于什么都没做；
- **`parent_id` 两条来源**：`artifact:<id>` 直接用；普通路径则在同一 DAG 的制品索引里
  按 `sha256 + size` 找最新的那份。找不到就不硬编——`parent` 字段缺省，比挂错爹强。

**验收**：对一个真 UPX 样本，产物 `sha256` 与 `upx -d` 手工结果一致，`parent_id` 指向原样本；
对非 UPX 文件报 UPX 的原话，不硬解；样本目录 `ls -la` 前后一致。
（真文件回归是 `#[ignore]` 的 `unpacks_a_real_sample`，本机没装 upx，未跑。）

### P2.2 radare2 + IR（✅ 已落地）

`radare2.rs` + `index.rs`（类型 + 映射 + 缓存）。DEX 线同时接进 IR（`dex.rs` 已有数据）。

落地时的三个决定：

- **`index.rs` 只管形状与缓存**：`aflj` 与 DEX 各自映射，Ghidra 的导出脚本 P2.3 再补；
  缓存键严格按 `sha256 + backend + backend_version`，后端升级后旧 IR 不再命中；
- **`blocks` 先空着**：`aflj` 不给基本块，`cfg.extract` 的 detail pass（P2.4）再跑 `afbj` 回填；
- **DEX 的 `va` 是 `code_off`**：IR 里写明"不透明 id"，下游不假设可重定位；
  内置解析器的版本写成 `builtin-<应用版本>`，升级即失效。

**验收**：同一 Mach-O 用 r2 出 `functions`，函数数与 `r2 -c 'afl'` 一致；
二次调用不再 spawn（用假后端计数验证）；DEX 样本能出 IR 且 `cfg.extract` 可用。
（本机没装 radare2：映射/截断/缓存/失败原文用 fixture 与假后端验证，
真样本回归是 `#[ignore]` 的 `scans_a_real_sample`；DEX 的 IR 映射在 `index.rs` 单测里覆盖。）

### P2.3 Ghidra analyze + decompile（✅ 已落地）

`ghidra.rs` + `PrismExport.java` + `PrismDecompile.java` + 缓存项目目录 + JVM 并发限制。

落地时的四个决定：

- **一次只跑一个 JVM**：`JVM` 互斥锁包住 analyze 与 decompile；反编译缺项目时
  在**同一把锁内**补一次分析，不递归、不抢锁；
- **程序名固定 `sample.bin`**：优先硬链接、其次符号链接、最后复制到作业目录，
  这样 `-process sample.bin` 永远找得到；样本目录仍然不写；
- **脚本随应用走**：`bundle.resources` 带 `resources/ghidra/*`，运行时按
  `PRISM_GHIDRA_SCRIPTS` → Tauri 资源目录 → `CARGO_MANIFEST_DIR/resources/ghidra`
  → 可执行文件旁依次找；找不到就报"安装包不完整"，不静默降级；
- **超时给部分结果**：`-analysisTimeoutPerFile 900` 让 Ghidra 自己收尾；
  只要 `functions.json` 写出来了就交付并标 `truncated`，硬超时（1800s）才判失败。

**验收**：真样本出 `functions`，与 r2 的函数数差异 < 15%（差异要能解释：thunk / PLT）；
`JAVA_HOME` 缺失时状态是 `Broken` 并给出原因；分析超时返回部分结果 + `truncated`。
（本机没装 Ghidra / JDK：探测、版本、假后端 analyze → 缓存 → decompile → 补分析
整条链路用假 `analyzeHeadless` 验证；真样本回归是 `#[ignore]` 的
`analyzes_and_decompiles_a_real_sample`。）

### P2.4 反编译 + 下游四件

`PrismDecompile.java` + `cfg.extract` + `hash_identify`（P1+P2）+ `vm.detect` + `text.extract`。

**验收**：单函数反编译出可读伪 C，`text.extract strip` 后括号配平；
`cfg.extract` 的 dot 能被 Graphviz 渲染、mermaid 能被前端渲染；
`hash_identify` 对一个已知 SHA-256 实现的函数报 `certain`，对只有常量的报 `high`。

### P2.5 纯 Rust 提前项

`yara.rule_gen`、`report.rev`、`dyn.syscall.filter`（fixture 驱动）。

**验收**：`yara` 规则能被 `yara` 命令行编译通过（若本机有），并命中原样本、不命中系统文件；
`report.rev` 在缺 5 类制品时仍能出报告且每节写明缺什么。

### P3.0 沙箱 S2（只承诺 ELF）

`SandboxProvider` + QEMU 后端 + guest agent + 快照回滚 + `bin.dyn.sandbox.run`。

**验收**：跑一个打印 + 写文件的 ELF，`proctree` / `syscalls` / `pcap` 三件都在；
跑完宿主 `~` 与样本目录无变化；断网时样本的 `connect()` 返回 `ENETUNREACH`；
第二次运行从干净快照起（第一次写的文件不存在）。

### P3.1 dump / trace / dumper

**验收**：对加壳 ELF 用 `strategy=oop` dump 出的产物能 `bin.meta.identify`、能再跑
`bin.pack.detect` 且壳特征消失；`trace.breakpoint` 命中数不超过 `maxHits`；
租约过期后的 dump 报明确错误。

### P3.2（可选）Windows guest / .NET T2

PE 动态分析、`de4dot` 链路。没做之前，PE 样本的动态工具直接报"只支持 ELF"。

### 回填清单（与代码同批提交）

| 位置 | 改什么 |
|---|---|
| `docs/SECURITY_AGENTS.md` §4.1 | 16 行的状态列改成 ✅ / ⬜→进行中，并注明本文 |
| `docs/CRYPTO_TOOLS.md` 状态行 | 改为"已实现（`analysis.rs`）" |
| `docs/RUNTIME_SANDBOX.md` | 补一节"S2 沙箱"指向本文 §6.3 |
| `agents.rs` 七个安全工种 | 把"本机没有反编译器/沙箱"的绝对句改成条件句（先 `tools.list`） |
| `README.md` 工具表 | 数字与分组同步 |

---

## 11. 错误文案（照抄，别各写各的）

| 场景 | 文案 |
|---|---|
| 后端缺失 | `Ghidra 没接上：没找到 analyzeHeadless。装好后在 设置 → 工具链 里指一下 support/analyzeHeadless（或设 GHIDRA_HOME）。` |
| 版本过低 | `radare2 版本太低：检测到 4.5.0，至少要 5.5（aflj 的字段在 5.x 才稳定）。升级：brew upgrade radare2` |
| 找到但跑不起来 | `找到 Ghidra 11.2，但 java 起不来（java: command not found）。Ghidra 需要 JDK 17+，装好后点「重新检测」。` |
| 沙箱不可用 | `动态分析需要 S2 沙箱（轻量 VM），本机只有 S0/S1。要跑样本先在 设置 → 工具链 里装沙箱镜像。` |
| 沙箱租约过期 | `这次运行的 VM 已经销毁（租约 10 分钟）。重新跑 bin.dyn.sandbox.run 再 dump。` |
| 超时 | `Ghidra 分析超时（900s）：已导出前 312 个函数，标了 truncated。样本较大可以先脱壳，或调小 -analysisTimeoutPerFile。` |
| 输出不可解析 | `radare2 的输出不是合法 JSON（退出码 0）。原文前 200 字符：…（已存为制品 functions_raw，未做猜测解析）` |
| UPX 拒绝 | `UPX 拒绝了这份文件：CantUnpackException: file is modified/hacked/protected。改过头的样本不要 --force 硬解，走 bin.dyn.mem.dump。` |
| 需要先有 IR | `这个工具需要函数边界：先跑 bin.ghidra.analyze 或 bin.radare2.scan（DEX 样本不用外部程序，自动用内置解析器）。` |
| 平台不支持 | `PE 动态分析需要 Windows guest，本机还不支持；静态部分照常（bin.ghidra.analyze）。` |

---

## 12. 测试策略

**探测（纯函数 + 假后端）**

- 优先级：`toolchains/index.json` > 环境变量 > PATH > 已知目录；
- 版本解析：`radare2 5.9.8 0 @ darwin-arm-64`、`upx 4.2.4`、`ilspycmd 8.2.0.7535`、
  缺字段的畸形输出 → `Broken` 不 panic；
- 太老 → `TooOld`，文案带检测到的版本。

**执行契约**

- argv 不做 shell：文件名 `a; rm -rf ~.bin` 原样作为参数传进去（用假后端断言收到的 argv）；
- 超时：睡 5s 的假后端在 1s 超时下被杀，且**进程组**（含孙进程）一起死；
- 输出上限：吐 10 MiB 的假后端被截断，头尾都在、中间有"截断 N 字节"；
- env：`OPENAI_API_KEY` 不进子进程（复用 `RUNTIME_SANDBOX.md` §9 的做法）；
- 解析失败：假后端吐非 JSON → `fail` + `functions_raw` 制品存在。

**IR**

- fixture：一份 `aflj` JSON → `ProgramIndex` 字段逐项断言（va/name/size/kind）；
- 截断：20001 个函数 → `truncated=true`；
- 缓存：同 sha256 二次调用不 spawn（假后端写计数文件）；backend_version 变了要重跑；
- 淘汰：造 3 个缓存目录超上限，最旧的被删、Ghidra 项目目录一起删。

**下游**

- `cfg.extract`：三块两边的 fixture → edges 正确；dot/mermaid 语法冒烟（括号配平、节点 id 合法）；
- `hash_identify`：P1 常量 fixture；P2 用"64 轮循环 + 4 状态变量"的 blocks fixture；
- `vm.detect`：分发器 fixture → medium；三项全中 → high；普通 switch → low 且带"可能是状态机"；
- `text.extract`：真 Ghidra 伪 C（`#[ignore]` 真文件回归）→ strip 后无 `/* WARNING`、括号配平；
- `syscall.filter`：fixture JSONL → 五个 focus 各命中预期条数。

**真文件回归（`#[ignore]`）**

- `upx_unpacks_a_real_sample`：仓库外样本，`#[ignore]`，CI 不跑；
- `ghidra_index_matches_radare2`：同一 ELF 两后端函数数差异 < 15%；
- `sandbox_elf_run_is_clean`：P3，宿主快照前后 `git status` 与 `~` 抽样目录不变。

---

## 13. 工作量估算

| 部分 | 估算 | 说明 |
|---|---|---|
| `backends/{mod,probe,exec}` | ~900 行 | 地基，含单测（`job.rs` 随 P2.3 Ghidra 落地） |
| 过滤链 + IPC + 设置页 | ~300 行 | Rust 180 + TS/Vue 120 |
| `upx.rs` | ~90 行 | 最小 |
| `radare2.rs` | ~220 行 | 含 `aflj`/`afbj` 解析 |
| `ghidra.rs` + 两个 Java 脚本 | ~380 行 | Rust 180 + Java 200 |
| `index.rs`（IR + 缓存 + 淘汰） | ~240 行 | |
| `cfg.extract` | ~180 行 | 三种输出格式 |
| `hash_identify` | ~200 行 | P1 扩表 + P2 循环判定 |
| `vm.detect` | ~160 行 | |
| `text.extract` | ~120 行 | |
| `dotnet.rs` | ~160 行 | T1 + T2 探测 |
| `yara.rule_gen` / `report.rev` | ~300 行 | |
| `dyn.syscall.filter` | ~120 行 | 纯后处理 |
| 沙箱（provider + QEMU + guest agent） | ~1200 行 + 镜像 | P3，独立一档 |
| `dyn.*` 其余 3 个 + dumper | ~500 行 | 依赖沙箱 |
| **P2 合计** | **~3,300 行** | 不含沙箱；含单测（各项约 20%） |
| **P3 合计** | **~1,700 行 + 镜像分发** | |

---

## 14. 已拍板与仍待确认

已定（用户授权按实现方判断）：

- [x] **Ghidra 只探测，不自带、不下载**——400 MB 不进安装包，按 11.x 的 Program API 写脚本；
- [x] **选 radare2，不选 rizin**——装机量大，`aflj` 在 5.x 稳定；
- [x] **沙箱镜像首次使用时明确询问后下载**，QEMU + HVF（P3 才动，先不阻塞）；
- [x] **`upx --force` 彻底不给**——`bin.unpack.upx` 没有这个参数，改过头的样本走动态 dump；
- [x] `bin.vm.detect` 归 IR 组、`yara.rule_gen` / `report.rev` 提前到 P2.5；
- [x] `bin.dyn.*` 的审批不提供"总是允许"。

仍待确认（都要真样本/真后端才能答）：

- [ ] Ghidra 11.x 的具体小版本（决定 `PrismExport.java` 用哪套 API；本机没装，P2.3 开工时问）；
- [ ] `.NET` 去混淆做到 T2（de4dot）就够，还是要自研 dnlib 静态改写器？
