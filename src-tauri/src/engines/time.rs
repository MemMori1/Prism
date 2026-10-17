//! 通用小工具：时间解析等跨引擎共享的纯函数。

/// RFC3339 → 毫秒时间戳（宽容：缺毫秒按 0；支持 Z 与 ±HH:MM 时区）。
pub fn parse_iso_ms(text: &str) -> Option<i64> {
    if text.len() < 19 {
        return None;
    }
    let year: i64 = text.get(0..4)?.parse().ok()?;
    let month: i64 = text.get(5..7)?.parse().ok()?;
    let day: i64 = text.get(8..10)?.parse().ok()?;
    let hour: i64 = text.get(11..13)?.parse().ok()?;
    let minute: i64 = text.get(14..16)?.parse().ok()?;
    let second: i64 = text.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let millis: i64 = text
        .get(19..20)
        .filter(|sep| *sep == ".")
        .and_then(|_| text.get(20..23))
        .and_then(|fraction| fraction.parse().ok())
        .unwrap_or(0);
    let offset_minutes: i64 = match text.get(19..20) {
        Some("+") | Some("-") => {
            let sign = if text.get(19..20) == Some("+") { 1 } else { -1 };
            let hh: i64 = text.get(20..22)?.parse().ok()?;
            let mm: i64 = text.get(23..25)?.parse().ok()?;
            sign * (hh * 60 + mm)
        }
        _ => 0,
    };
    let days = days_from_civil(year, month, day);
    Some((days * 86_400 + hour * 3600 + minute * 60 + second - offset_minutes * 60) * 1000 + millis)
}

/// 天数差（Howard Hinnant days_from_civil，1970-01-01 为 0）。
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 文件修改时间 → 毫秒时间戳。
pub fn file_modified_ms(path: &std::path::Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_iso_timestamps() {
        assert_eq!(parse_iso_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso_ms("2026-06-11T13:04:07.118Z"), Some(1_781_183_047_118));
        assert_eq!(parse_iso_ms("2026-06-11T21:04:07+08:00"), Some(1_781_183_047_000));
        assert_eq!(parse_iso_ms("2026-06-11T13:04:07"), Some(1_781_183_047_000));
        assert_eq!(parse_iso_ms("bad"), None);
    }
}
