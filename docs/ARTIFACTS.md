# 制品仓库与节点数据流 · 详细设计

> 状态：**已实现（P0 + P0.5）**；上游文档 `docs/SECURITY_AGENTS.md`（需求分析）。
> 本文是实施依据：模块、类型、函数签名、占位符规则、错误文案、测试清单。
> 与需求分析不一致的地方集中在 §2，逐条写了为什么改。

## 0. 一句话

节点产出不再靠"把正文整段拼进下游提示词"传递：**跑完即落制品**（`~/.prism/artifacts/<dag_id>/`），
下游拿 `{{artifacts.<名>.id|summary|meta}}` 或工具 `prism.artifact.get` 按需取全文。
大文本只进一次磁盘、进一次上下文摘要，上下文不再随节点数线性膨胀。

## 1. 范围

**做**：制品仓库（put/get/list/stat/delete）、`DagNode` 数据流字段、执行器落制品、
占位符解析与截断、校验规则、IPC 读取、编排页产物展示、三个 `bin.*` 真工具、工具标签。

**不做**：跨 DAG 共享、制品加密、远程同步、制品 GC（删计划不删制品）、
Ghidra / 动态沙箱（P2/P3）、攻击类工具。

## 2. 与需求分析的差异（四条，都是有理由的收窄或放宽）

| # | 需求分析 | 这里 | 为什么 |
|---|---|---|---|
| 1 | `{{nodes.x.output}}` 超 **2 KB** 自动截断 | 超 **8 KB** 截断（`PROMPT_INLINE_LIMIT`） | 2 KB 对"读完再改"的下游太狠：一份 5 KB 调研结论被砍掉 60%，模型会开始猜。截断的职责是拦住 100 KB 级倾倒，不是省 3 KB。制品引用照给，需要全文的节点自己取。`summary` 仍按 2 KB（`SUMMARY_LIMIT`）。 |
| 2 | `inputArtifacts` 必须显式声明 | **可省**：不声明时，每个上游的产出制品自动以「节点 id」为别名可用 | 让 90% 的图零配置就能引用上游产物；`inputArtifacts` 只在需要改别名、或要读上游**非主产出**（工具写的 `strings` 等）时才写。老计划因此完全不受影响。 |
| 3 | 上游 key 不匹配就报错 | 只有"主产出 key"能静态校验；其它 key 给**警告**不拦 | 工具写入的制品 key（`strings`/`hashes`）运行时才存在，静态拦死会让合法的图跑不起来。警告文案写清"运行时才知道"。 |
| 4 | 工具名直接用点分层（`bin.meta.identify`） | 内部名不变，**发给模型时换成下划线**（`bin_meta_identify`） | OpenAI / Anthropic / Gemini 的函数名都只接受 `[A-Za-z0-9_-]`，点号会让整个请求 400——工具一个都发不出去，模型连"我没有工具"都说不出来。内部名保持点分层，白名单、开关存储、文档都不用跟着改；`find()` 两种写法都认。 |

## 3. 数据模型

### 3.1 `ArtifactRef`（`dag::artifact`）

```rust
pub struct ArtifactRef {
    pub id: String,        // art-20261022-224501123-001，同时是磁盘文件名
    pub dag_id: String,
    pub node_id: String,   // 谁产的；工具写入时是当前节点
    pub key: String,       // 语义键：report / strings / hashes / meta
    pub kind: String,      // text / json / binary / image
    pub name: String,      // 原始文件名（有的话）
    pub size: u64,
    pub sha256: String,    // 十六进制小写
    pub parent_id: Option<String>, // 血缘：脱壳产物指向原始样本
    pub created_at: i64,   // 毫秒
}
```

### 3.2 计划侧（`dag::model`）

```rust
pub struct DagNode {
    // ...原有字段
    #[serde(default)]
    pub input_artifacts: Vec<ArtifactInput>,
    #[serde(default = "default_artifact_key")] // "report"
    pub output_artifact_key: String,
}

pub struct ArtifactInput {
    pub from: String,  // 上游节点 id
    pub key: String,   // 要它哪个制品；主产出可省略（空 = 上游的 output key）
    #[serde(rename = "as")]
    pub alias: String, // 占位符里用的名字
}
```

`DagNode::artifact_key()` 返回 `output_artifact_key.trim()`，空则回落 `"report"`。

### 3.3 运行侧（`dag::run`）

```rust
pub struct NodeRun {
    // ...原有字段
    pub artifact: Option<ArtifactRef>, // 主产出落制品的结果；写失败为 None
}
```

`mark_succeeded(node_id, output, artifact)`：一次状态迁移同时写产出与制品引用，
只落一次盘、只广播一次（原来分开调会多推一份快照）。

### 3.4 工具侧作用域

```rust
pub struct DagScope {          // 由原生执行器为每个节点构造
    pub dag_id: String,
    pub node_id: String,
    pub run_id: String,
}
```

`ToolContext.dag: Option<&DagScope>`。制品类工具**只在该作用域下出现**（`ToolScope::Dag`），
聊天里根本不进 schema——"能不能跨 DAG 读"在工具解析层就不成立，不靠参数校验。

## 4. 仓库

### 4.1 布局

```text
~/.prism/artifacts/
  <dag_id>/
    index.json        # Vec<ArtifactRef>，原子写
    art-...-001       # 内容原样（无扩展名，kind 说了算）
```

`dag_id` / `artifact_id` 都先过 id 校验（字母数字 + `-` `_`，长度上限），
这条校验同时是路径穿越防线——和 `store::check_id` 同一套规则。

### 4.2 写入

`put` 的顺序：**先写内容（临时文件 + rename），再改索引**。

- 内容写成功、索引写失败：留一个孤儿文件，`index.json` 里没有它，读不到也删不掉，
  但不会出现"索引里有、文件没有"的坏引用（那个更麻烦）。
- 单制品上限 256 MiB；单 DAG 上限 2 GiB。超限**报错**，不静默截断。
- 同进程并发：索引读改写套一把进程级 `Mutex`（节点是并行跑的，索引是共享文件）。
- `kind` 探测：显式传入优先；否则按内容——合法 UTF-8 且首字符是 `{`/`[` → `json`，
  其它合法 UTF-8 → `text`，非 UTF-8 → `binary`。图片按扩展名（png/jpg/gif/webp）。

### 4.3 读取矩阵

| 调用方 | 能不能读 |
|---|---|
| 同 DAG 的节点工具 | ✅ `get(dag_id, id)`，`dag_id` 来自执行器注入的作用域 |
| 本机 IPC（编排页预览） | ✅ 传 `dagId` |
| 另一个 DAG 的节点 | ❌ 作用域里的 `dag_id` 不同，取不到；工具没有"指定 DAG"的参数 |
| 聊天会话 | ❌ 工具不在 schema 里 |

### 4.4 删除

`delete(dag_id, id)`：删内容 → 从索引移除。只给内部/测试用，
**删计划不会自动删制品**（制品是证据，误删不可恢复；要做清理得单独确认）。

## 5. 节点数据流

### 5.1 落制品（执行器）

节点成功 → `artifact::put(dag_id=plan.id, node_id, key=node.artifact_key(), name=<node>.<ext>, bytes=正文)`。

- 正文为空不发制品（空产出没有引用价值，`artifact: None`）；
- `put` 失败**不把节点判失败**：正文还在运行记录里，判失败等于丢掉已经花钱跑出来的结果。
  降级为 `artifact: None` + 一条 step（`制品写入失败：<原因>`），下游退回内联文本。

### 5.2 消费（拼提示词）

`build_request` 除了 `outputs`（上游正文），再构造 `artifacts: HashMap<别名, ArtifactView>`：

```rust
pub struct ArtifactView {
    pub id: String,       // 空串 = 没有可用制品
    pub key: String,
    pub kind: String,
    pub size: u64,
    pub sha256: String,
    pub summary: String,  // 正文前 2 KB
}
```

别名来源（后者覆盖前者）：

1. **隐式**：每个上游节点 → 别名 = 节点 id，key = 该节点的 `artifact_key()`；
2. **显式** `input_artifacts`：按 `from`+`key` 精确找（先内存里的运行记录，
   再磁盘索引，这样工具写入的 `strings` 也能被下游取到）。

找不到时仍插入一个 `id` 为空的视图：占位符要能降级成"有摘要、没引用"，不能变成空白。

### 5.3 占位符

| 写法 | 注入 |
|---|---|
| `{{nodes.<id>.output}}` `.report` `.files` | 上游正文；超 8 KB 截断，并追加「完整内容见制品 art-xxx」 |
| `{{artifacts.<名>.id}}` | 制品 id；没有则 `（制品不可用）` |
| `{{artifacts.<名>.summary}}` | 正文前 2 KB；被截断时补一句 |
| `{{artifacts.<名>.meta}}` | `{"id":...,"key":...,"kind":...,"size":...,"sha256":...}` 一行 JSON |
| `{{artifacts.<名>}}` | 等价 `.summary` |

未知别名 → `（未声明的制品 x）`；未闭合的 `{{` 原样保留（和现有行为一致）。

### 5.4 校验（`dag::validate`）

新增规则，全部是**能指出是谁的问题**的句子：

| 情况 | 级别 | 文案 |
|---|---|---|
| `as` 不合法 / 重复 | error | `制品别名「x」不合法：只能用字母、数字、连字符与下划线` / `制品别名「x」重复了` |
| `as` 与某个节点 id 同名 | error | `制品别名「x」和节点 id 撞名：占位符会分不清该取哪个` |
| `from` 不存在 | error | `制品引用的节点「x」不存在` |
| `from` 不是祖先 | error | `制品引用了「x」，但它不是上游` |
| `key` 与上游主产出 key 不一致 | warning | `要「x」的 strings 制品，但它的主产出是 report：这个 key 得由工具写入，运行时才确认` |
| `{{artifacts.a.field}}` 的 `a` 未声明且不是祖先 | error | `占位符引用了未声明的制品「a」` |
| `field` 不在 id/summary/meta | error | `制品占位符不支持 .xxx（只有 id / summary / meta）` |

## 6. 工具标签与 P0.5 三个工具

### 6.1 `ToolDef` 扩展

```rust
pub tags: &'static [&'static str], // bin:static / bin:dyn-sandbox / artifact / dag:planner
pub risk: Risk,                    // read / write / exec / attack
pub network: Network,              // none / outbound / isolated
pub sandbox: Option<Sandbox>,      // code / malware
pub scope: ToolScope,              // any / dag
```

- `builtin()` 保持原签名（8 个老工具零改动），内部填默认值：
  `tags=[]`、`risk` 按权限档（只读→read，可写→write）、`network=none`、`sandbox=None`、`scope=any`；
- 新工具用 `builtin_full(..., meta, run)` 显式写标签；
- `specs()` / `specs_for(enabled, max, dag)`：`scope == dag` 且没有作用域时**不发**。
- **声明边界改一次名**：`ToolDef::schema_value()` 把 `function.name` 换成
  `wire_name()`（点号 → 下划线）后再发出去，`find()` 同时认内部名与 wire 名。
  白名单过滤（`filter_specs`）也因此要把两边压平再比，否则带点号的工具会被
  静默筛空——比 400 更难查的那种"不干活"。

本期只把标签用起来（清单、过滤、设置页展示），**不实现网络/沙箱的执行语义**：
`network` / `sandbox` 目前是声明，给 P2/P3 留位，文档不假装它们已经生效。

### 6.2 新工具

| 工具 | 权限 | 风险 | 标签 | 作用域 | 说明 |
|---|---|---|---|---|---|
| `prism.artifact.put` | write | write | `artifact` | dag | `{key, content?, path?, name?, parent?}` → 制品引用 JSON |
| `prism.artifact.get` | read | read | `artifact` | dag | `{id, limit?}` → 文本（超 40 KB 截断并说明） |
| `bin.meta.identify` | read | read | `bin:static` | any | 文件格式 / 架构 / 位数 / 段数 / 入口 |
| `bin.hash.compute` | read | read | `bin:static` | any | md5 / sha1 / sha256，流式读 |
| `bin.strings.extract` | read | read | `bin:static` | any | ASCII + UTF-16LE，可疑标记 |

三个 `bin.*` 的产出：**有 DAG 作用域时同时落制品**（key 分别为 `meta` / `hashes` / `strings`），
返回 JSON 里带 `artifact` 字段；没有作用域（聊天）时只返回 JSON 本身。
这样"分析结果不进上下文"在 DAG 里成立，同时聊天里也能直接用。

### 6.3 实现要点

- `bin.meta.identify`：手写 magic 解析，不引库。
  PE（`MZ` → `e_lfanew` → `PE\0\0`，machine / sections / timestamp / subsystem）、
  ELF（class / endian / e_type / e_machine / e_phnum / e_shnum / e_entry）、
  Mach-O（32/64、大小端、fat、cputype、ncmds）、DEX（`dex\n0xx`）、
  APK（zip 头 + 在头部 256 KB 里找 `AndroidManifest.xml`）。
  解析不了的字段**直接省略**，不猜。
- `bin.hash.compute`：64 KB 分块喂 digest，文件上限 512 MiB（超了报错，不硬算）。
- `bin.strings.extract`：扫描上限 32 MiB、条目上限 200（最多 2000）、
  ASCII 与 UTF-16LE 两遍、按文本去重（保留首次偏移与命中次数）、
  标记 `url` / `domain` / `ipv4` / `path` / `command` / `key`。
- 三者都对路径做**词法归一 + 工作目录前缀校验**（复用 `tools::resolve`），越界报错。

## 7. IPC 与前端

| 命令 | 参数 | 返回 |
|---|---|---|
| `dag_artifacts` | `dagId` | `ArtifactRef[]`（按 created_at 倒序） |
| `dag_artifact_get` | `dagId`, `id`, `limit?` | `{ artifact, text, truncated }` |

前端：

- `types.ts`：`ArtifactRef` / `DagArtifactText`；`DagNode` 加两个字段，`NodeRun` 加 `artifact`；
- `DagPane` 策略页：`产出制品 key`（默认 report）；
- `DagPane` 运行页：产出下方一块「制品」——key、大小、sha256 前 12 位、父制品，
  两个动作：「查看」（取全文，弹层里看前 40 KB）、「复制 id」；
- `dev-preview-fake.ts`：假运行给节点补 artifact，两个新命令返回假数据；
- 设置页工具清单：`ToolInfo` 加 `tags` / `risk`，卡片上标一行小字（不改布局）。

## 8. 错误与降级

| 场景 | 表现 |
|---|---|
| 制品超单件/DAG 配额 | `put` 报错：`制品超过单件上限（256 MiB）` / `这个 DAG 的制品已满（2 GiB）` |
| 索引损坏 | `list` 跳过并留 stderr；`get` 报 `制品索引损坏`，不返回半截数据 |
| 正文为空 | 不落制品，`artifact: None`，下游 `nodes.x.output` 显示「没有留下正文」 |
| 制品写失败 | 节点仍成功；step 里记原因；下游退回内联文本 |
| 下游要的制品不存在 | `.id` 显示「（制品不可用）」，`.summary` 用正文兜底 |
| 工具拿不到 DAG 作用域 | 制品工具不进 schema；强行调用报「只在 DAG 节点里可用」 |

## 9. 测试

**单元**

- `artifact`：put/get 往返、sha256 与内容一致、跨 DAG 取不到、超配额报错、
  父子血缘、索引跳坏行、id 穿越拒绝；
- `resolve_task`：三种 `artifacts.*`、超长截断带引用、未知别名、未闭合占位符；
- `validate`：别名重复/撞名/非祖先/key 不符（警告）；
- `executor`：跑完节点 `artifact` 非空、正文与取回内容长度一致、下游拿到摘要；
- `forensics`：PE/ELF/Mach-O 头解析（用构造的字节 + 本机真实文件）、
  三哈希对照已知向量、strings 命中 URL 与路径。

**端到端（手工）**

一张三节点图，研究节点固定输出 200 KB：

1. 运行后节点有制品，`sha256` 与本地 `shasum -a 256` 一致；
2. 下游 prompt 里该段 ≤ 8 KB，且含 `art-`；
3. 用 `prism.artifact.get` 取回，长度与原文一致；
4. 老计划（无新字段）照常跑通。

## 10. 与 P1 的接口

- `json.jq` / `text.extract` 直接写在同一个 `forensics` 兄弟模块里，拿 `DagScope` 落制品；
- Planner 工具面（P1.5）的 `prism.dag.node.update` 复用 `NodeRun`，天然能写 `artifact`；
- `bin.dyn-sandbox` 类工具只需在 `ToolMeta` 里写 `tags=["bin:dyn-sandbox"]`、`sandbox=Malware`，
  调度层不用改。
