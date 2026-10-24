# 密码学工具补全设计（`engines/native/analysis.rs`）

> 状态：**已实现**（`engines/native/analysis.rs`，34 个模块单测覆盖）。实现与草稿的差异、以及 §10 待确认项的结论见文末 §11。
> 目标：补全 prism「安全研究线」里两处零外部依赖、却一直缺位的密码分析能力——`bin.crypto.asn1.parse`（PEM/DER 证书与密钥解析）与 `bin.crypto.xor_brute`（多字节 XOR 爆破）。两者都纯 Rust 可做，不依赖 Ghidra / radare2 / 沙箱，是当前 APK/DEX 逆向线能立刻兑现的能力。

---

## 1. 背景与动机

现有 `bin.crypto.scan`（`analysis.rs::run_crypto`）已经能扫到密码常量，但它对「证书 / 密钥」这一层只做了两件粗粒度的事：

- 搜 PEM 头（`-----BEGIN ... KEY-----`），只报「有内嵌 PEM」，**不解析**里面的算法、位长、有效期、指纹；
- 搜 RSA 公钥 DER 前缀（`SIGNATURES` 表里的 `rsa-public` 条目），同样只报偏移。

逆向场景里常见的诉求它答不了：

| 诉求 | 现状 |
|---|---|
| APK 签证书是谁签的、什么算法、有效期到哪 | 只报「有 PEM」，拿不到 issuer / serial / validity |
| 内嵌公钥 / 私钥的算法与位长 | 只报偏移，不给模数 / 位长 |
| 一段被 XOR 加密的字符串，密钥不是单字节 | `xorProbe` 只试 1..=255 单字节，多字节密钥直接漏掉 |
| 「这段密文是不是用 `http` / `api` 之类已知明文 XOR 出来的」 | 单字节能猜，多字节（如 `abc`、`xyz123`）猜不到 |

本设计把这两处补成正式的 `bin.*` 工具，产出结构化 JSON（走 artifact，不进上下文），复用既有解析器范式与辅助函数，零新依赖。

---

## 2. 目标与非目标

### 目标
- 纯 Rust 解析 PEM / DER 的 X.509 证书、RSA / EC 公钥、PKCS#8 / PKCS#1 私钥，产出结构化元信息。
- 纯 Rust 做 2–N 字节 XOR 爆破，基于已知明文定位密钥与偏移。
- 走既有 `analysis.rs::static_tool` 注册范式，复用 `target_of` / `read_head` / `respond` / `find_all`，零新依赖。
- 只读、有上限、不猜。

### 非目标
- **不做密码学运算**（不解密、不签名、不验签）。解析器只读 ASN.1/DER 结构，不引 `ring` / `rsa` / `x509-parser` 之类重型依赖，也不自己实现 RSA 数学。
- 不做 CSR / CRL / OCSP / 证书链校验。
- 不做私钥导出（明文私钥绝不落盘、绝不进上下文，只给元信息）。
- 不做流式 / 增量解析，整段 DER 一次读入（有 `SCAN_LIMIT` 兜底）。

---

## 3. 工具划分

两个新工具都落在 `analysis.rs`（与 `bin.crypto.scan` 同源），沿「算法」这一条线细分：

| 工具 | wire 名 | 职责 |
|---|---|---|
| `bin.crypto.asn1.parse` | `bin_crypto_asn1_parse` | 解析 PEM/DER 证书与密钥，输出算法 / 位长 / 指纹 / 有效期 |
| `bin.crypto.xor_brute` | `bin_crypto_xor_brute` | 多字节 XOR 爆破，用已知明文反推密钥与偏移 |

`bin.crypto.scan` 里现有的单字节 `xorProbe` **保留不动**（向后兼容）；新工具是它的超集，多字节场景引导到 `xor_brute`，单字节快查仍可留在 `scan`。

---

## 4. `bin.crypto.asn1.parse` 设计

### 4.1 输入

| 参数 | 类型 | 说明 |
|---|---|---|
| `path` | string | 相对工作目录的样本路径（证书文件，或内嵌证书的二进制） |
| `offset` | string（可选） | 十六进制偏移，定位内嵌 DER/PEM 的起始；不传则从文件头解析 |

样本可能是一整份 `*.pem` / `*.der` / `*.cer` / `*.crt`，也可能是二进制里内嵌的一段。所以**两种入口**：

1. `path` 直接是证书文件 → 从头解析；
2. `path` 是二进制 + `offset` 给了内嵌位置 → 从 `offset` 起解析。

后者让「`bin.crypto.scan` 在 APK 里找到 PEM 头 → 用 `asn1.parse` 展开」形成闭环。

### 4.2 解析流程

```
输入字节
  ├─ 去空白，判断是 PEM（有 "-----BEGIN ...-----" 头）还是裸 DER
  │    PEM → 抽取 base64 主体 → decode → 得 DER
  │    DER → 直接进 ASN.1 解析
  └─ 解析 DER TLV 结构（手写递归下降，不引库）
       0x30 SEQUENCE（外层）
       0x03 BIT STRING（证书里的 subjectPublicKeyInfo 或签名）
       0x02 INTEGER（模数、序列号、指数）
       0x05 NULL、0x06 OID、0x0C UTF8String、0x13 PrintableString ...
```

### 4.3 结构识别（按最外层 TLV 内容判别类型）

| 类型 | 判别依据 | 输出字段 |
|---|---|---|
| X.509 证书 | 顶层 `SEQUENCE` → 内层含 `tbsCertificate` SEQUENCE + `signatureAlgorithm` OID + `signatureValue` BIT STRING | version / serial / issuer / subject / validity / pubkey 算法与位长 / 签名算法 / SHA-256 指纹 |
| RSA 公钥（PKCS#1 `RSAPublicKey`） | 顶层 `SEQUENCE` → 两个 INTEGER（n, e） | 算法 `rsa`、模数位长、公钥指数 e |
| RSA 私钥（PKCS#1 `RSAPrivateKey`） | 顶层 `SEQUENCE` → 九个（或更多）INTEGER，首项是 `version=0` | 算法 `rsa`、模数位长（**不导出任何私钥字段**） |
| EC 公钥（X9.62） | `subjectPublicKeyInfo` 里 OID 是 `ecPublicKey` + 曲线 OID | 算法 `ec`、曲线名（P-256 / P-384 / secp256k1） |
| PKCS#8 私钥 | 顶层 `SEQUENCE` → `version=0` INTEGER + `PrivateKeyAlgorithm` SEQUENCE(OID) + `OCTET STRING` | 算法（rsa / ec）+ 位长（从内层 OCTET STRING 再递归一层） |

判别原则与既有纪律一致——**不猜**：识别不出就报 `"type": "unknown"` 并给出顶层 TLV 的 OID（如果读到），不编算法名。

### 4.4 输出格式

```json
{
  "path": "unpack/aerial/META-INF/CERT.RSA",
  "type": "x509",
  "version": "v3",
  "serial": "0x4a2f...",
  "issuer": "C=CN, O=...",
  "subject": "C=CN, O=...",
  "validNotBefore": "2023-01-01T00:00:00Z",
  "validNotAfter": "2033-01-01T00:00:00Z",
  "signatureAlgorithm": "sha256WithRSAEncryption",
  "publicKey": { "algorithm": "rsa", "bits": 2048, "e": 65537 },
  "fingerprintSha256": "ab:cd:..."
}
```

对私钥：**只输出** `{ "type": "rsa-private", "bits": 2048 }` 这类元信息，明确写 `"note": "私钥内容不导出、不进上下文"`。

### 4.5 关键实现点（手写 DER 解析）

- **TLV 读取**：`tag`（1B，含多字节 tag 支持高 tag number）+ `length`（短形 1B / 长形 0x81–0x84）+ `value`。长度超 `SCAN_LIMIT` 直接报「超限」。
- **OID 表**：内置最小 OID → 算法名映射（rsaEncryption、id-ecPublicKey、secp256r1/384r1、secp256k1、sha256WithRSAEncryption 等十余条常用项），命中给名字，未命中给裸 OID。
- **整数解码**：DER INTEGER 是大端有符号，去掉前导 `0x00` 后按字节算位长（`(len - leading_zero_bytes) * 8` 近似，**明确标注是「位长，非精确值」**）。
- **时间解析**：UTCTime（`YYMMDDHHMMSSZ`）与 GeneralizedTime（`YYYY...Z`）都解析成 ISO 8601；解析不了就原样留字符串。
- **指纹**：对整份 DER 做 SHA-256（复用 `dag::artifact::sha256_of`，或直接喂 `sha2` 若已有），输出十六进制。**不校验签名**（那是运算，非目标）。

---

## 5. `bin.crypto.xor_brute` 设计

### 5.1 输入

| 参数 | 类型 | 说明 |
|---|---|---|
| `path` | string | 样本路径 |
| `probe` | string | 已知明文片段（`http://`、`api/`、`admin` 等） |
| `minLen` | integer（可选） | 密钥最小长度，默认 1 |
| `maxLen` | integer（可选） | 密钥最大长度，默认 8，上限 16 |

### 5.2 算法

对每个 `key_len ∈ [minLen, maxLen]`：

1. 用 `probe` 的前 `key_len` 字节做**假定密钥**：`key[i] = cipher[i] XOR probe[i]`，但真正的通用做法是——对样本里每一个可能的偏移，用 `probe` 与该处 `key_len` 字节窗口逐位 XOR 得 `key`，再拿这个 `key` 去解 `probe` 长度整段，看是否还原出 `probe` 本身。

   更精确的定义（与单字节 `xorProbe` 同构的推广）：

   ```
   对样本中每个偏移 p：
     key = bytes[p .. p+key_len] XOR probe[0 .. key_len]
     用 key 周期性地解 bytes[p .. p + probe.len]，若前 probe.len 字节 == probe
       → 命中：记录 (key, p)
   ```

2. 若 `probe.len < key_len`（明文比密钥短），则只能恢复 key 的前 `probe.len` 字节，命中标 `partial`，并提示「密钥比明文长，只能定位前缀」。

3. 输出去重：同一把 key 在多个偏移命中时，合并成一条 `offsets` 列表。

### 5.3 输出格式

```json
{
  "path": "sample.bin",
  "probe": "http://",
  "matches": [
    {
      "key": [0x61, 0x62, 0x63],
      "keyStr": "abc",
      "offset": "0x1234",
      "offsets": ["0x1234", "0x5678"],
      "confidence": "certain"
    }
  ],
  "truncated": false,
  "note": "多字节密钥可能不止一把；优先取命中位置最集中的那把"
}
```

### 5.4 与单字节 `xorProbe` 的关系

- `key_len == 1` 时，`xor_brute` 的结果与 `bin.crypto.scan` 的 `xorProbe` **等价**——不重复实现，`xor_brute` 内部 `key_len=1` 直接复用同一段 `find_all` 逻辑。
- `minLen` 默认 1，即默认也覆盖单字节，语义是 `xorProbe` 的完整超集。

---

## 6. 纪律与上限（两工具共用）

- `Permission::ReadOnly`、`Risk::Read`、`Network::None`、`ToolScope::Any`、tags `["bin:static"]`、`delegable = true`——直接复用 `analysis.rs::static_tool`。
- 读取上限沿用 `SCAN_LIMIT = 64 MiB`，超了标 `truncated`。
- **私钥字段绝不导出**：`asn1.parse` 只给元信息，`"note"` 里明确写「私钥内容不导出、不进上下文」。
- **不猜**：识别不出的算法 / 类型 / OID，报「未知」或裸 OID，不编名字；`confidence` 三档（certain / high / low），只有「明文整段还原」才是 `certain`。
- XOR 爆破的结果天然可能多解，`note` 恒提示「取命中位置最集中的那把」，避免 agent 拿错密钥下结论。

---

## 7. 接入点

1. 在 `analysis.rs` 顶部 `tools()` 的 vec 里，`bin.crypto.scan` 之后追加两行：
   ```rust
   static_tool("bin.crypto.asn1.parse", "证书与密钥解析", asn1_schema, run_asn1),
   static_tool("bin.crypto.xor_brute", "多字节 XOR 爆破", xor_schema, run_xor_brute),
   ```
2. 新增 `asn1_schema` / `xor_schema` / `run_asn1` / `run_xor_brute` 与私有 DER TLV 解析器（`tlf` / `oid` 小表），全部在 `analysis.rs` 内，**不新建模块**（体量小，不值得单独立 `asn1.rs`；若后续膨胀再拆）。
3. 复用 `target_of` / `read_head` / `respond` / `find_all` / `sha256_of`，不重复造轮子。
4. `docs/SECURITY_AGENTS.md` 第 4.1 节工具表里，把 `bin.crypto.asn1.parse` 从「⬜ 可纯 Rust 做，未开始」改为「✅ 已实现」，把 `bin.crypto.xor_brute` 的「多字节未做」补成「✅ 已实现」。

---

## 8. 测试策略（`#[cfg(test)]`）

### `asn1.parse`

- **DER TLV**：短形 / 长形长度、高 tag number、截断输入不 panic。
- **整数位长**：构造带前导 `0x00` 的 INTEGER，断言位长正确（且标注「近似」）。
- **X.509**：手工构造一个最小证书 DER（或用 `openssl` 生成后把字节内联进测试），断言 type=证书、issuer/subject、RSA 2048 位、指纹 64 个 hex 字符。
- **PEM 包裹**：同一 DER 包成 PEM，断言两种入口（裸 DER / PEM）产出一致。
- **RSA 公钥 PKCS#1**：两个 INTEGER 的 SEQUENCE，断言 `algorithm=rsa`、位长、e=65537。
- **私钥不导出**：给 PKCS#8 私钥，断言输出**不含**任何私钥字段（d / p / q），只有 `bits` 与 `note`。
- **未知 OID**：给一个表里没有的 OID，断言返回裸 OID 而非编造名字。
- **内嵌偏移**：把 DER 拼进一段随机字节，用 `offset` 指定，断言正确解析。

### `xor_brute`

- **单字节等价**：`key_len=1` 时与 `xorProbe` 命中一致（回归单字节）。
- **多字节密钥**：构造 `"secret"` 用 key `"abc"` 异或后的密文，`probe="secret"` 断言命中 `key=[0x61,0x62,0x63]`。
- **周期性密钥**：key `"abc"`、明文比密钥长（如 `"http://example.com"`），断言仍命中。
- **明文比密钥长（partial）**：`probe` 短于 key，断言标 `partial`。
- **无命中**：随机数据 + 无关 probe，断言 `matches` 为空且 `note` 提示未命中。
- **去重**：同一 key 多偏移，断言合并成一条 `offsets`。

---

## 9. 工作量估算

| 部分 | 估算 |
|---|---|
| DER TLV + OID 小表 | ~80 行 |
| X.509 / RSA / EC / PKCS#8 结构解析 | ~160 行 |
| `asn1.parse` schema + run + 出口 | ~60 行 |
| `xor_brute` schema + run（复用 find_all） | ~70 行 |
| 测试 | ~160 行 |
| **合计** | **~530 行，零新依赖** |

---

## 10. 待确认项

- [x] 工具命名 `bin.crypto.asn1.parse` / `bin.crypto.xor_brute` 是否认可？——按草稿落地。
- [x] `asn1.parse` 是否要顺带支持 `subjectPublicKeyInfo`（SPKI）这一层单独识别，还是归进「未知 OID → 裸 OID」即可？——单独识别，输出 `type=public-key`；证书里的 SPKI 复用同一段解析。
- [x] 私钥解析是否需要做到「只认 PKCS#8 + PKCS#1，其余（如 EC 私钥 SEC1）报未知」就够，还是要把 SEC1 也纳入？——SEC1 一并纳入（`type=ec-private`），因为它和 PKCS#8 里的 EC 内层是同一套结构，顺手就能覆盖。
- [x] XOR 爆破 `maxLen` 上限 16 是否够（有些逆向里密钥是 16 字节 AES key，恰好压线）？——上限 16，默认 8。

## 11. 落地记录（实现后回填）

代码全部在 `src-tauri/src/engines/native/analysis.rs`，零新依赖（`base64` 已在依赖里）。

**与草稿一致的**

- 两个工具都走 `static_tool`：`ReadOnly` / `Risk::Read` / `Network::None` / `ToolScope::Any` / `tags=["bin:static"]` / `delegable`；
- `bin.crypto.scan` 的 `xorProbe` 原样保留，新工具是其超集；
- 解析器不引 `ring` / `rsa` / `x509-parser`，手写 DER TLV 递归下降 + 最小 OID 表；
- 私钥只出元信息（`bits` / `e` / `curve` / `note`），`d` / `p` / `q` 等字段一个都不出现，单测直接断言 JSON 里没有这些键；
- 识别不出就 `type=unknown` 或裸 OID，不编算法名。

**超出草稿的**

1. **PKCS#7 signedData**：APK 的 `META-INF/CERT.RSA` / `CERT.EC` 不是裸证书，而是 PKCS#7 容器。不做这层，草稿 §4.4 里 `CERT.RSA` 那个例子根本跑不通。现在 `type=pkcs7`，证书列在 `certificates[]` 里（各自带指纹）。
2. **位长按最高有效位精确算**，不是草稿里的「字节数 × 8 近似」：RSA 2048 位仍是 2048，但 2047 位的怪模数不会被报成 2044 / 2048。
3. **`xor_brute` 的未验证候选要做重复过滤**：`probe` 不比密钥长时（`probe.len() <= key_len`），每个偏移都能"推出"一段密钥，随机数据会刷出几十万条。现在的规则是——未验证候选只保留**在同一段数据里出现 ≥2 次**的，标 `confidence=low` + `verified=false`，并在 `note` 里写明不能直接下结论。可验证的命中仍是 `certain`（整段明文还原）/ `high`（命中落在文件尾、只验证了前缀）。
4. 单字节快查没有为了「等价」去复用 `find_all`：`xor_brute` 用「第 key_len 个字节」做 O(1) 预筛再推导密钥，比 255 次全量搜索快得多，语义等价。

**已知边界**

- 加密 PEM（`Proc-Type: 4,ENCRYPTED`）明确报「不支持」，不去猜口令；
- 显式曲线参数的 EC 公钥只给点长推算的位长，不给曲线名；
- `xor_brute` 在 64 MiB 满读的样本上最坏约 O(样本 × maxLen)，属可接受的一次性开销；仍受 `SCAN_LIMIT` 约束。

