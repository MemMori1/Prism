//! APK / ZIP 的中央目录读取：签名块与清单解析共用的最小实现。
//!
//! 只做三件事：找 EOCD、读中央目录拿到条目表、按条目解压出**一个**文件。
//! 不做 zip64、不碰加密条目、不跟随数据描述符——APK 用不到这些；真遇到就
//! 报错，而不是猜着往下读。
//!
//! 为什么单独成模块：`bin.apk.sign` 要中央目录的偏移来定位 Signing Block，
//! `bin.apk.manifest` 要从条目里取出 `AndroidManifest.xml`。两边各写一遍
//! EOCD 扫描，迟早会在「畸形尾部」上给出两套结论。

use std::io::Read;

/// 中央目录里的一条记录。字段全是 APK 里实际会用到的。
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    /// 0 = stored，8 = deflate。其余方法不支持。
    pub method: u16,
    pub compressed_size: usize,
    pub uncompressed_size: usize,
    /// 本地文件头在文件里的偏移。
    pub local_offset: usize,
}

#[derive(Debug)]
pub struct Layout {
    pub entries: Vec<Entry>,
    /// 中央目录起点：APK Signing Block 就在它前面。
    pub central_offset: usize,
}

impl Layout {
    pub fn entry(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.name == name)
    }
}

/// 定位 EOCD → 中央目录 → 条目表。
///
/// 与 `forensics::apk_entries` 的区别：这边保留解压所需的字段，并且对
/// 越界 / zip64 / 偏移不自洽一律报错。壳检测那边拿不到目录就降级成"没有
/// 目录信息"是合理的；签名与清单不能降级——"读不出"和"没有"必须分开。
pub fn layout(blob: &[u8]) -> Result<Layout, String> {
    let eocd = find_eocd(blob).ok_or_else(|| "找不到 ZIP EOCD（不是 zip / APK？）".to_string())?;
    let count = le16(blob, eocd + 10)? as usize;
    let central_size = le32(blob, eocd + 12)? as usize;
    let central_offset = le32(blob, eocd + 16)? as usize;
    if count == u16::MAX as usize
        || central_size == u32::MAX as usize
        || central_offset == u32::MAX as usize
    {
        return Err("ZIP64 APK 暂不支持（EOCD 字段是 0xffff / 0xffffffff）".to_string());
    }
    let central_end = central_offset
        .checked_add(central_size)
        .ok_or_else(|| "中央目录偏移溢出".to_string())?;
    if central_end > eocd {
        return Err(format!(
            "中央目录越过 EOCD（{central_offset:#x}+{central_size} > {eocd:#x}）"
        ));
    }

    let mut entries = Vec::with_capacity(count);
    let mut at = central_offset;
    for index in 0..count {
        if at + 46 > central_end {
            return Err(format!("中央目录第 {index} 条在 {at:#x} 处截断"));
        }
        if &blob[at..at + 4] != b"PK\x01\x02" {
            return Err(format!("中央目录条目 {at:#x} 的签名不是 PK\\x01\\x02"));
        }
        let method = le16(blob, at + 10)?;
        let compressed = le32(blob, at + 20)? as usize;
        let uncompressed = le32(blob, at + 24)? as usize;
        let name_len = le16(blob, at + 28)? as usize;
        let extra_len = le16(blob, at + 30)? as usize;
        let comment_len = le16(blob, at + 32)? as usize;
        let local_offset = le32(blob, at + 42)? as usize;
        let name_start = at + 46;
        let name_end = name_start
            .checked_add(name_len)
            .ok_or_else(|| "文件名长度溢出".to_string())?;
        if name_end > central_end {
            return Err(format!("文件名越过中央目录（{name_end:#x}）"));
        }
        entries.push(Entry {
            name: String::from_utf8_lossy(&blob[name_start..name_end]).to_string(),
            method,
            compressed_size: compressed,
            uncompressed_size: uncompressed,
            local_offset,
        });
        at = name_end
            .checked_add(extra_len)
            .and_then(|value| value.checked_add(comment_len))
            .ok_or_else(|| "中央目录条目长度溢出".to_string())?;
    }
    Ok(Layout {
        entries,
        central_offset,
    })
}

/// 从尾部找一个"注释长度正好补到文件末"的 EOCD；找不到再退回最后一个魔数。
///
/// 带注释的 zip 尾部可能有假的 EOCD 魔数，先认自洽的那个。退回最后一个
/// 魔数是给"注释长度写错、但目录本身没问题"的样本留的活路。
pub fn find_eocd(blob: &[u8]) -> Option<usize> {
    if blob.len() < 22 {
        return None;
    }
    let start = blob.len().saturating_sub(22 + 65535);
    let mut fallback = None;
    for at in (start..=blob.len() - 22).rev() {
        if &blob[at..at + 4] != b"PK\x05\x06" {
            continue;
        }
        let comment = u16::from_le_bytes([blob[at + 20], blob[at + 21]]) as usize;
        if at + 22 + comment == blob.len() {
            return Some(at);
        }
        if fallback.is_none() {
            fallback = Some(at);
        }
    }
    fallback
}

/// 取出一个条目的原始字节（stored 直接切片，deflate 现解压）。
///
/// `limit` 是解压后的硬上限：清单只有几十 KB，一个声称"解压后 4 GB"的条目
/// 不该把执行线程拖死。先看中央目录声明的大小，再在解压时按 `limit + 1`
/// 截断，两道闸都留着——中央目录本身也可能是假的。
pub fn read_entry(blob: &[u8], entry: &Entry, limit: usize) -> Result<Vec<u8>, String> {
    if entry.uncompressed_size > limit {
        return Err(format!(
            "{} 解压后 {} 字节，超过本工具 {} 字节上限",
            entry.name, entry.uncompressed_size, limit
        ));
    }
    let header = entry.local_offset;
    if header + 30 > blob.len() {
        return Err(format!("{} 的本地文件头越界（{header:#x}）", entry.name));
    }
    if &blob[header..header + 4] != b"PK\x03\x04" {
        return Err(format!(
            "{} 的本地文件头签名不是 PK\\x03\\x04（{header:#x}）",
            entry.name
        ));
    }
    let name_len = le16(blob, header + 26)? as usize;
    let extra_len = le16(blob, header + 28)? as usize;
    let data_start = header
        .checked_add(30 + name_len + extra_len)
        .ok_or_else(|| format!("{} 的数据偏移溢出", entry.name))?;
    let data_end = data_start
        .checked_add(entry.compressed_size)
        .ok_or_else(|| format!("{} 的压缩长度溢出", entry.name))?;
    if data_end > blob.len() {
        return Err(format!(
            "{} 的数据越过文件末尾（{data_end:#x} > {:#x}）",
            entry.name,
            blob.len()
        ));
    }
    let data = &blob[data_start..data_end];

    let bytes = match entry.method {
        0 => data.to_vec(),
        8 => {
            let mut out = Vec::new();
            flate2::read::DeflateDecoder::new(data)
                .take(limit as u64 + 1)
                .read_to_end(&mut out)
                .map_err(|error| format!("{} 解压失败：{error}", entry.name))?;
            out
        }
        other => {
            return Err(format!(
                "{} 用了不支持的压缩方法 {other}（只认 stored / deflate）",
                entry.name
            ))
        }
    };
    if bytes.len() > limit {
        return Err(format!(
            "{} 解压后超过 {} 字节上限（疑似压缩炸弹）",
            entry.name, limit
        ));
    }
    // 中央目录声明的大小与实际解压结果对不上：宁可报错，也不拿半截 XML
    // 去解析——那种"解析成功但字段缺失"最难发现。
    if !bytes.is_empty() && bytes.len() != entry.uncompressed_size {
        return Err(format!(
            "{} 解压后 {} 字节，与中央目录声明的 {} 不一致",
            entry.name,
            bytes.len(),
            entry.uncompressed_size
        ));
    }
    Ok(bytes)
}

pub(crate) fn le16(bytes: &[u8], at: usize) -> Result<u16, String> {
    let slice = bytes
        .get(at..at + 2)
        .ok_or_else(|| format!("读取 u16 越界：{at:#x}"))?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

pub(crate) fn le32(bytes: &[u8], at: usize) -> Result<u32, String> {
    let slice = bytes
        .get(at..at + 4)
        .ok_or_else(|| format!("读取 u32 越界：{at:#x}"))?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

pub(crate) fn le64(bytes: &[u8], at: usize) -> Result<u64, String> {
    let slice = bytes
        .get(at..at + 8)
        .ok_or_else(|| format!("读取 u64 越界：{at:#x}"))?;
    let mut raw = [0u8; 8];
    raw.copy_from_slice(slice);
    Ok(u64::from_le_bytes(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个最小 zip：每条 stored，中央目录 + EOCD 齐全。
    fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
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

    /// 造一个 deflate 条目，size 字段按压缩后 / 原始长度写。
    fn deflated_zip(name: &str, data: &[u8]) -> Vec<u8> {
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, data).expect("压缩");
        let packed = encoder.finish().expect("收尾");
        let mut body = Vec::new();
        body.extend_from_slice(b"PK\x03\x04");
        body.extend_from_slice(&20u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes()); // flags
        body.extend_from_slice(&8u16.to_le_bytes()); // method = deflate
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(&(name.len() as u16).to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(name.as_bytes());
        body.extend_from_slice(&packed);

        let mut central = Vec::new();
        central.extend_from_slice(b"PK\x01\x02");
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // flags
        central.extend_from_slice(&8u16.to_le_bytes()); // method = deflate
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(name.as_bytes());

        let central_offset = body.len() as u32;
        body.extend_from_slice(&central);
        body.extend_from_slice(b"PK\x05\x06");
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&1u16.to_le_bytes());
        body.extend_from_slice(&1u16.to_le_bytes());
        body.extend_from_slice(&(central.len() as u32).to_le_bytes());
        body.extend_from_slice(&central_offset.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body
    }

    #[test]
    fn reads_stored_and_deflated_entries() {
        let apk = zip(&[("AndroidManifest.xml", b"<manifest/>")]);
        let read = layout(&apk).expect("应当能读中央目录");
        let entry = read.entry("AndroidManifest.xml").expect("清单条目");
        assert_eq!(read_entry(&apk, entry, 1024).unwrap(), b"<manifest/>");

        let packed = deflated_zip("classes.dex", b"dex\n035\0payload");
        let read = layout(&packed).unwrap();
        let entry = read.entry("classes.dex").unwrap();
        assert_eq!(entry.method, 8);
        assert_eq!(
            read_entry(&packed, entry, 1024).unwrap(),
            b"dex\n035\0payload"
        );
    }

    #[test]
    fn decompression_bomb_hits_the_limit() {
        let big = vec![0u8; 4096];
        let apk = deflated_zip("AndroidManifest.xml", &big);
        let read = layout(&apk).unwrap();
        let entry = read.entry("AndroidManifest.xml").unwrap();
        let error = read_entry(&apk, entry, 128).expect_err("必须挡住超限条目");
        assert!(error.contains("上限"), "{error}");
    }

    #[test]
    fn zip64_is_rejected_instead_of_guessed() {
        let mut apk = zip(&[("a", b"x")]);
        let eocd = find_eocd(&apk).unwrap();
        apk[eocd + 16..eocd + 20].copy_from_slice(&u32::MAX.to_le_bytes());
        let error = layout(&apk).expect_err("zip64 应当报错");
        assert!(error.contains("ZIP64"), "{error}");
    }

    #[test]
    fn truncated_central_directory_is_an_error() {
        let mut apk = zip(&[("a", b"x")]);
        let eocd = find_eocd(&apk).unwrap();
        // 把中央目录大小吹大，让它越过 EOCD。
        apk[eocd + 12..eocd + 16].copy_from_slice(&9999u32.to_le_bytes());
        let error = layout(&apk).expect_err("越界应当报错");
        assert!(error.contains("越过 EOCD"), "{error}");
    }
}
