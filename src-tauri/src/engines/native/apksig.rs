//! APK 签名结构解析：`bin.apk.sign`。
//!
//! 只回答"这个包**声称**用什么方案签的、签名者是谁"，不回答"签名有效吗"。
//! 后者需要做密码学验签与整包摘要比对，本模块刻意不做：一旦半吊子地算一遍
//! 就说"已验证"，比不做更危险。输出里会明确写清这一点。
//!
//! 解析范围：
//! - v1：`META-INF/MANIFEST.MF` + `META-INF/*.RSA|DSA|EC` 的存在性（不碰
//!   PKCS#7 签名值，证书交给 `bin.crypto.asn1.parse` 或本工具的 v2/v3 部分）；
//! - v2：APK Signing Block 里的 `0x7109871a`；
//! - v3：`0xf05368c0`；
//! - v3.1：`0x1b93ad61`。
//!
//! v4 是独立的 `.idsig` 文件，不在 APK 里，因此这里只报"未检查"。

use serde_json::{json, Value};

use super::analysis::describe_der;
use super::apkzip::{self, le32, le64};
use super::forensics::{read_head, respond, target_of};
use super::tools::{
    builtin_full, Network, Permission, Risk, ToolContext, ToolDef, ToolMeta, ToolResult, ToolScope,
};

/// 读取上限。签名块与中央目录都在文件末尾，不能只读头部；超过上限直接报错，
/// 不拿半截数据去猜"没有签名"。
const APK_SIGN_LIMIT: u64 = 256 * 1024 * 1024;

const BLOCK_MAGIC: &[u8] = b"APK Sig Block 42";
const ID_V2: u32 = 0x7109_871a;
const ID_V3: u32 = 0xf053_68c0;
const ID_V31: u32 = 0x1b93_ad61;

pub fn tools() -> Vec<std::sync::Arc<ToolDef>> {
    vec![builtin_full(
        "bin.apk.sign",
        "APK 签名结构",
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
            "name": "bin.apk.sign",
            "description": "解析 APK 签名结构：v1（META-INF）、v2 / v3 / v3.1（APK Signing Block），列出签名方案、签名者证书（主体 / 签发者 / 有效期 / SHA-256 指纹）与摘要算法。只解析结构，不做密码学验签，不执行样本。",
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
    if size > APK_SIGN_LIMIT {
        return ToolResult::fail(format!(
            "{} 有 {} MiB，超过本工具 {} MiB 的读取上限；请先裁剪样本，或在设备上用 \
             apksigner verify --print-certs 校验。",
            path.display(),
            size / (1024 * 1024),
            APK_SIGN_LIMIT / (1024 * 1024)
        ));
    }
    let blob = match read_head(&path, APK_SIGN_LIMIT) {
        Ok(bytes) => bytes,
        Err(error) => return ToolResult::fail(error),
    };
    if blob.len() as u64 != size {
        return ToolResult::fail(format!(
            "只读到 {} / {size} 字节，数据不完整；拒绝在截断的文件上判断签名",
            blob.len()
        ));
    }
    let mut payload = match inspect(&blob) {
        Ok(payload) => payload,
        Err(error) => return ToolResult::fail(format!("APK 签名结构解析失败：{error}")),
    };
    payload["path"] = json!(path.display().to_string());
    payload["size"] = json!(size);
    respond(ctx, "signature", payload)
}

/// 纯函数入口：输入完整 APK 字节，输出签名结构。单测直接吃构造的 zip。
pub fn inspect(blob: &[u8]) -> Result<Value, String> {
    let layout = apkzip::layout(blob)?;
    let v1 = v1_summary(&layout.entries);
    let block = match signing_block(blob, layout.central_offset)? {
        Some(block) => block,
        None => Vec::new(),
    };

    let v2 = scheme(&block, ID_V2, 2);
    let v3 = scheme(&block, ID_V3, 3);
    let v31 = scheme(&block, ID_V31, 31);

    let mut present = Vec::new();
    if v1["present"] == json!(true) {
        present.push("v1");
    }
    if v2["present"] == json!(true) {
        present.push("v2");
    }
    if v3["present"] == json!(true) {
        present.push("v3");
    }
    if v31["present"] == json!(true) {
        present.push("v3.1");
    }

    let verdict = if present.is_empty() {
        "没有发现 v1 / v2 / v3 / v3.1 签名：样本可能未签名，或签名文件被移除。".to_string()
    } else {
        format!(
            "发现 {} 签名。这里只解析结构，**没有做密码学验签**：没有验证签名值、\
             整包摘要与证书链，不能据此认定 APK 未被篡改或证书可信。",
            present.join(" + ")
        )
    };

    Ok(json!({
        "format": "apk-signature",
        "entryCount": layout.entries.len(),
        "v1": v1,
        "v2": v2,
        "v3": v3,
        "v3.1": v31,
        "presentSchemes": present,
        "v4": {
            "checked": false,
            "note": "v4 签名在独立的 .idsig 文件里，不在 APK 内；本工具未检查。"
        },
        "verdict": verdict,
        "note": "证书指纹由证书 DER 现算（SHA-256）；证书里的自报字段未做可信性判断。"
    }))
}

// ---------------------------------------------------------------- 签名块

/// APK Signing Block：位置在中央目录之前，尾部是 size + magic。
///
/// `None` = 没有签名块（合法：只有 v1 或未签名）；`Some(Err)` = 声称有但结构
/// 坏了。两种情况必须分开，"一律返回空"会把畸形签名伪装成未签名。
fn signing_block<'a>(
    blob: &'a [u8],
    central_offset: usize,
) -> Result<Option<Vec<(u32, &'a [u8])>>, String> {
    if central_offset < 32 {
        return Ok(None);
    }
    let magic_at = central_offset - 16;
    if &blob[magic_at..central_offset] != BLOCK_MAGIC {
        return Ok(None);
    }
    let footer = central_offset - 24;
    let size = le64(blob, footer)? as usize;
    if size < 24 {
        return Err(format!("签名块 size={size} 小于最小长度 24"));
    }
    let start = central_offset
        .checked_sub(size + 8)
        .ok_or_else(|| "签名块起点越过文件头".to_string())?;
    if le64(blob, start)? as usize != size {
        return Err(format!(
            "签名块首尾 size 不一致：{:#x} vs {size}",
            le64(blob, start)?
        ));
    }
    let pairs_start = start + 8;
    let pairs_end = footer;
    if pairs_start > pairs_end {
        return Err("签名块没有 ID-value 区".to_string());
    }

    let mut pairs = Vec::new();
    let mut at = pairs_start;
    while at < pairs_end {
        if at + 8 > pairs_end {
            return Err(format!("ID-value 长度字段在 {at:#x} 处截断"));
        }
        let length = le64(blob, at)? as usize;
        if length < 4 {
            return Err(format!("ID-value 长度 {length} 小于 4（连 ID 都放不下）"));
        }
        let pair_end = at
            .checked_add(8 + length)
            .ok_or_else(|| "ID-value 长度溢出".to_string())?;
        if pair_end > pairs_end {
            return Err(format!(
                "ID-value 越过签名块（{pair_end:#x} > {pairs_end:#x}）"
            ));
        }
        let id = le32(blob, at + 8)?;
        pairs.push((id, &blob[at + 12..pair_end]));
        at = pair_end;
    }
    if at != pairs_end {
        return Err("ID-value 区没有正好走到块尾".to_string());
    }
    Ok(Some(pairs))
}

fn scheme(block: &[(u32, &[u8])], id: u32, version: u8) -> Value {
    let Some((_, value)) = block.iter().find(|(pair_id, _)| *pair_id == id) else {
        return json!({ "present": false });
    };
    match signers(value, version) {
        Ok(list) => json!({
            "present": true,
            "blockId": format!("0x{id:08x}"),
            "signerCount": list.len(),
            "signers": list,
        }),
        Err(error) => json!({
            "present": true,
            "blockId": format!("0x{id:08x}"),
            "error": error,
            "note": "签名块存在但结构解析失败；这不是「没有签名」。",
        }),
    }
}

fn signers(value: &[u8], version: u8) -> Result<Vec<Value>, String> {
    let mut outer = Reader::new(value);
    let sequence = outer.length_prefixed()?;
    let mut reader = Reader::new(sequence);
    let mut out = Vec::new();
    while reader.remaining() > 0 {
        let signer = reader.length_prefixed()?;
        out.push(signer_fields(signer, version).unwrap_or_else(|error| {
            json!({
                "error": error,
                "note": "这个签名者的结构解析失败；其余签名者继续解析。",
            })
        }));
    }
    if out.is_empty() {
        return Err("签名者列表为空".to_string());
    }
    Ok(out)
}

fn signer_fields(signer: &[u8], version: u8) -> Result<Value, String> {
    let mut outer = Reader::new(signer);
    let signed_data = outer.length_prefixed()?;
    // v3 / v3.1 的 signed data 与签名值之间还夹着 SDK 区间；v2 没有。
    // 少了这一步，就会把 minSdk（比如 21）当成 signatures 的长度前缀。
    let outer_sdk = if version == 3 {
        Some((outer.u32()?, outer.u32()?))
    } else if version == 31 {
        Some((outer.u32()?, outer.u32()?))
    } else {
        None
    };
    let outer_rotation = if version == 31 {
        Some(outer.u32()?)
    } else {
        None
    };
    let signatures = outer.length_prefixed()?;
    let public_key = outer.length_prefixed()?;

    let mut signed = Reader::new(signed_data);
    let digests = signed.length_prefixed()?;
    let certificates = signed.length_prefixed()?;

    let mut out = serde_json::Map::new();
    out.insert("digests".to_string(), digest_records(digests)?);
    out.insert(
        "certificates".to_string(),
        certificate_records(certificates),
    );
    out.insert("signatures".to_string(), signature_records(signatures)?);
    out.insert("publicKeyBytes".to_string(), json!(public_key.len()));
    out.insert(
        "publicKeySha256".to_string(),
        json!(crate::dag::artifact::sha256_of(public_key)),
    );

    // SDK 区间在 v3 / v3.1 里存了两份（signed data 内 + signer 外层）。
    // 外层是签名覆盖到的声明，优先报外层；读不到再退回 signed data。
    if let Some((min, max)) = outer_sdk {
        out.insert("minSdk".to_string(), json!(min));
        out.insert("maxSdk".to_string(), json!(max));
    } else if version == 3 {
        if let (Ok(min), Ok(max)) = (signed.u32(), signed.u32()) {
            out.insert("minSdk".to_string(), json!(min));
            out.insert("maxSdk".to_string(), json!(max));
        }
    } else if version == 31 {
        if let (Ok(min), Ok(max)) = (signed.u32(), signed.u32()) {
            out.insert("minSdk".to_string(), json!(min));
            out.insert("maxSdk".to_string(), json!(max));
            if let Ok(rotation) = signed.u32() {
                out.insert("rotationMinSdk".to_string(), json!(rotation));
            }
        }
    }
    if let Some(rotation) = outer_rotation {
        out.insert("rotationMinSdk".to_string(), json!(rotation));
    }
    Ok(Value::Object(out))
}

fn digest_records(blob: &[u8]) -> Result<Value, String> {
    let mut reader = Reader::new(blob);
    let mut out = Vec::new();
    while reader.remaining() > 0 {
        let record = reader.length_prefixed()?;
        let mut inner = Reader::new(record);
        let algorithm = inner.u32()?;
        let digest = inner.length_prefixed()?;
        out.push(json!({
            "algorithm": algorithm_name(algorithm),
            "algorithmId": format!("0x{algorithm:04x}"),
            "digest": super::forensics::hex(digest),
        }));
    }
    Ok(Value::Array(out))
}

fn certificate_records(blob: &[u8]) -> Value {
    let mut reader = Reader::new(blob);
    let mut out = Vec::new();
    while reader.remaining() > 0 {
        let Ok(cert) = reader.length_prefixed() else {
            out.push(json!({ "error": "证书长度前缀越界，剩余证书无法继续解析" }));
            break;
        };
        match describe_der(cert) {
            Ok(described) => out.push(described),
            Err(error) => out.push(json!({
                "type": "unparsed-certificate",
                "bytes": cert.len(),
                "sha256": crate::dag::artifact::sha256_of(cert),
                "error": error,
            })),
        }
    }
    Value::Array(out)
}

fn signature_records(blob: &[u8]) -> Result<Value, String> {
    let mut reader = Reader::new(blob);
    let mut out = Vec::new();
    while reader.remaining() > 0 {
        let record = reader.length_prefixed()?;
        let mut inner = Reader::new(record);
        let algorithm = inner.u32()?;
        let signature = inner.length_prefixed()?;
        out.push(json!({
            "algorithm": algorithm_name(algorithm),
            "algorithmId": format!("0x{algorithm:04x}"),
            "signatureBytes": signature.len(),
        }));
    }
    Ok(Value::Array(out))
}

/// APK 签名算法 ID → 名字。未收录的返回裸 ID，不猜。
fn algorithm_name(id: u32) -> String {
    match id {
        0x0101 => "RSASSA-PSS with SHA-256".to_string(),
        0x0102 => "RSASSA-PSS with SHA-512".to_string(),
        0x0103 => "RSA PKCS#1 v1.5 with SHA-256".to_string(),
        0x0104 => "RSA PKCS#1 v1.5 with SHA-512".to_string(),
        0x0201 => "ECDSA with SHA-256".to_string(),
        0x0202 => "ECDSA with SHA-512".to_string(),
        0x0301 => "DSA with SHA-256".to_string(),
        0x0401 => "verity RSA PKCS#1 v1.5 with SHA-256".to_string(),
        0x0421 => "verity RSA PKCS#1 v1.5 with SHA-256".to_string(),
        0x0423 => "verity ECDSA with SHA-256".to_string(),
        0x0425 => "verity DSA with SHA-256".to_string(),
        other => format!("unknown-0x{other:04x}"),
    }
}

// ---------------------------------------------------------------- v1

fn v1_summary(entries: &[apkzip::Entry]) -> Value {
    let mut manifest = false;
    let mut signature_files = Vec::new();
    for entry in entries {
        let name = &entry.name;
        if name == "META-INF/MANIFEST.MF" {
            manifest = true;
        }
        let upper = name.to_ascii_uppercase();
        if upper.starts_with("META-INF/")
            && (upper.ends_with(".RSA") || upper.ends_with(".DSA") || upper.ends_with(".EC"))
        {
            signature_files.push(name.clone());
        }
    }
    signature_files.sort();
    json!({
        "present": manifest && !signature_files.is_empty(),
        "manifest": manifest,
        "signatureFiles": signature_files,
        "note": "v1 只看 META-INF 结构是否存在；签名值与 MANIFEST.MF 摘要未在这里验证。",
    })
}

// ---------------------------------------------------------------- 小工具

/// 小端长度前缀阅读器，所有越界都报错而不是 panic。
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Reader { bytes, at: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.at)
    }

    fn u32(&mut self) -> Result<u32, String> {
        let value = le32(self.bytes, self.at)?;
        self.at += 4;
        Ok(value)
    }

    fn length_prefixed(&mut self) -> Result<&'a [u8], String> {
        let length = self.u32()? as usize;
        let start = self.at;
        let end = start
            .checked_add(length)
            .ok_or_else(|| "长度前缀溢出".to_string())?;
        if end > self.bytes.len() {
            return Err(format!(
                "长度前缀越界：{start:#x}+{length} > {:#x}",
                self.bytes.len()
            ));
        }
        self.at = end;
        Ok(&self.bytes[start..end])
    }
}

/// 给单测用的入口：从磁盘读一个 APK 并解析。
#[cfg(test)]
fn inspect_path(path: &std::path::Path) -> Result<Value, String> {
    let blob = std::fs::read(path).map_err(|error| error.to_string())?;
    inspect(&blob)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// 最小 zip：一条 stored 条目 + 中央目录 + EOCD。签名块另加在中央目录前。
    fn zip(entries: &[(&str, &[u8])], signing: Option<(u32, Vec<u8>)>) -> Vec<u8> {
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
            body.extend_from_slice(&0u32.to_le_bytes()); // crc（本工具不校验）
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

        if let Some((id, value)) = signing {
            let pairs_len = 8 + 4 + value.len();
            let size = (pairs_len + 24) as u64;
            body.extend_from_slice(&size.to_le_bytes());
            body.extend_from_slice(&(4u64 + value.len() as u64).to_le_bytes());
            body.extend_from_slice(&id.to_le_bytes());
            body.extend_from_slice(&value);
            body.extend_from_slice(&size.to_le_bytes());
            body.extend_from_slice(BLOCK_MAGIC);
        }
        let central_offset = body.len() as u32;
        body.extend_from_slice(&central);
        let central_size = central.len() as u32;
        body.extend_from_slice(b"PK\x05\x06");
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        body.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        body.extend_from_slice(&central_size.to_le_bytes());
        body.extend_from_slice(&central_offset.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body
    }

    /// 一个 v2 签名块：signerCount=1，证书故意用非法 DER（解析失败但结构保留）。
    fn v2_block() -> Vec<u8> {
        let mut signed = Vec::new();
        signed.extend_from_slice(&0u32.to_le_bytes()); // digests 为空
        let cert = b"not-a-real-cert";
        let mut certs = Vec::new();
        certs.extend_from_slice(&(cert.len() as u32).to_le_bytes());
        certs.extend_from_slice(cert);
        signed.extend_from_slice(&(certs.len() as u32).to_le_bytes());
        signed.extend_from_slice(&certs);
        signed.extend_from_slice(&0u32.to_le_bytes()); // additional attrs 为空

        let mut signer = Vec::new();
        signer.extend_from_slice(&(signed.len() as u32).to_le_bytes());
        signer.extend_from_slice(&signed);
        signer.extend_from_slice(&0u32.to_le_bytes()); // signatures 为空
        signer.extend_from_slice(&0u32.to_le_bytes()); // public key 为空

        let mut signers = Vec::new();
        signers.extend_from_slice(&(signer.len() as u32).to_le_bytes());
        signers.extend_from_slice(&signer);
        let mut value = Vec::new();
        value.extend_from_slice(&(signers.len() as u32).to_le_bytes());
        value.extend_from_slice(&signers);
        value
    }

    /// v3 签名者比 v2 多两层 SDK：signed data 内一份、signer 外层再一份。
    /// 这个 fixture 专门守住"外层 SDK 不能被当成 signatures 的长度前缀"。
    fn v3_block() -> Vec<u8> {
        let cert = b"not-a-real-cert";
        let mut certs = Vec::new();
        certs.extend_from_slice(&(cert.len() as u32).to_le_bytes());
        certs.extend_from_slice(cert);

        let mut signed = Vec::new();
        signed.extend_from_slice(&0u32.to_le_bytes()); // digests 为空
        signed.extend_from_slice(&(certs.len() as u32).to_le_bytes());
        signed.extend_from_slice(&certs);
        signed.extend_from_slice(&24u32.to_le_bytes()); // minSdk
        signed.extend_from_slice(&35u32.to_le_bytes()); // maxSdk
        signed.extend_from_slice(&0u32.to_le_bytes()); // additional attrs 为空

        let mut signature = Vec::new();
        signature.extend_from_slice(&0x0103u32.to_le_bytes());
        signature.extend_from_slice(&2u32.to_le_bytes());
        signature.extend_from_slice(b"xx");
        let mut signatures = Vec::new();
        signatures.extend_from_slice(&(signature.len() as u32).to_le_bytes());
        signatures.extend_from_slice(&signature);

        let mut signer = Vec::new();
        signer.extend_from_slice(&(signed.len() as u32).to_le_bytes());
        signer.extend_from_slice(&signed);
        signer.extend_from_slice(&24u32.to_le_bytes()); // 外层 minSdk
        signer.extend_from_slice(&35u32.to_le_bytes()); // 外层 maxSdk
        signer.extend_from_slice(&(signatures.len() as u32).to_le_bytes());
        signer.extend_from_slice(&signatures);
        signer.extend_from_slice(&2u32.to_le_bytes()); // public key
        signer.extend_from_slice(b"pk");

        let mut signers = Vec::new();
        signers.extend_from_slice(&(signer.len() as u32).to_le_bytes());
        signers.extend_from_slice(&signer);
        let mut value = Vec::new();
        value.extend_from_slice(&(signers.len() as u32).to_le_bytes());
        value.extend_from_slice(&signers);
        value
    }

    #[test]
    fn reads_v1_and_v2_without_verifying() {
        let apk = zip(
            &[
                ("AndroidManifest.xml", b"manifest"),
                ("META-INF/MANIFEST.MF", b"manifest"),
                ("META-INF/CERT.RSA", b"pkcs7"),
            ],
            Some((ID_V2, v2_block())),
        );
        let report = inspect(&apk).expect("应当能解析");
        assert_eq!(report["v1"]["present"], true);
        assert_eq!(report["v2"]["present"], true);
        assert_eq!(report["v2"]["signerCount"], 1);
        assert!(
            report["v2"]["signers"][0]["certificates"][0]
                .get("error")
                .and_then(Value::as_str)
                .is_some(),
            "坏证书应保留为解析失败条目：{report}"
        );
        assert!(
            report["verdict"]
                .as_str()
                .unwrap()
                .contains("没有做密码学验签"),
            "必须明确说没验签：{}",
            report["verdict"]
        );
    }

    #[test]
    fn reads_v3_signer_with_outer_sdk_range() {
        let apk = zip(&[("AndroidManifest.xml", b"m")], Some((ID_V3, v3_block())));
        let report = inspect(&apk).expect("v3 应当能解析");
        assert_eq!(report["v3"]["present"], true);
        let signer = &report["v3"]["signers"][0];
        assert_eq!(signer["minSdk"], 24, "{report}");
        assert_eq!(signer["maxSdk"], 35, "{report}");
        assert_eq!(signer["publicKeyBytes"], 2, "{report}");
        assert_eq!(
            signer["signatures"][0]["algorithm"], "RSA PKCS#1 v1.5 with SHA-256",
            "{report}"
        );
        assert!(
            signer["certificates"][0]
                .get("error")
                .and_then(Value::as_str)
                .is_some(),
            "坏证书应保留为解析失败条目：{report}"
        );
    }

    #[test]
    fn unsigned_apk_reports_absence_not_an_error() {
        let apk = zip(&[("AndroidManifest.xml", b"manifest")], None);
        let report = inspect(&apk).expect("未签名不是解析失败");
        assert_eq!(report["v1"]["present"], false);
        assert_eq!(report["v2"]["present"], false);
        assert!(report["verdict"].as_str().unwrap().contains("没有发现"));
    }

    #[test]
    fn broken_signing_block_is_not_silently_unsigned() {
        let mut apk = zip(&[("AndroidManifest.xml", b"manifest")], None);
        // 在中央目录前插一个带 magic、但 size 指向文件头的坏块。
        let eocd = apkzip::find_eocd(&apk).unwrap();
        let central_offset = le32(&apk, eocd + 16).unwrap() as usize;
        let mut bad = Vec::new();
        bad.extend_from_slice(&8u64.to_le_bytes());
        bad.extend_from_slice(BLOCK_MAGIC);
        apk.splice(central_offset..central_offset, bad);
        // 修正 EOCD 里的中央目录偏移（EOCD 本身也被推后了 24 字节）。
        let fixed = eocd + 24;
        let size = le32(&apk, fixed + 16).unwrap();
        apk[fixed + 16..fixed + 20].copy_from_slice(&(size + 24).to_le_bytes());
        let error = inspect(&apk).expect_err("坏块必须报错");
        assert!(error.contains("签名块"), "{error}");
    }

    #[test]
    fn v1_needs_both_manifest_and_signature_file() {
        let only_sig = zip(
            &[("AndroidManifest.xml", b"m"), ("META-INF/CERT.RSA", b"x")],
            None,
        );
        assert_eq!(inspect(&only_sig).unwrap()["v1"]["present"], false);

        let only_manifest = zip(
            &[
                ("AndroidManifest.xml", b"m"),
                ("META-INF/MANIFEST.MF", b"x"),
            ],
            None,
        );
        assert_eq!(inspect(&only_manifest).unwrap()["v1"]["present"], false);
    }

    /// 真样本验收：v2 存在、v3 不存在（样本在开发机上；缺失时跳过）。
    #[test]
    fn real_sample_if_present() {
        let path = Path::new("/Users/edwa2d4th/Desktop/未命名文件夹 7/aerial-guard.apk");
        if !path.exists() {
            eprintln!("跳过：{} 不在本机", path.display());
            return;
        }
        let report = inspect_path(path).expect("应当能解析真实 APK");
        assert_eq!(report["v2"]["present"], true, "{report}");
        assert_eq!(report["v3"]["present"], false, "{report}");
        let fingerprints = report["v2"]["signers"][0]["certificates"][0]["fingerprintSha256"]
            .as_str()
            .unwrap_or_default();
        assert!(!fingerprints.is_empty(), "证书指纹不能为空：{report}");
    }
}
