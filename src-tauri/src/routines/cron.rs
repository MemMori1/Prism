//! 五字段 cron 的解析与"下一次触发"计算（本机时区，Vixie 语义）。
//!
//! 为什么自研：定时表达式需要完全可测、边界语义明确，不引三方 cron 库。
//! 只支持标准五字段（分 时 日 月 周），语法：`*`、`a`、`a-b`、`*/n`、
//! `a-b/n`、`a/n`（a 到上界步进）、`a,b,c`；周日 = 0 或 7。
//! "日"与"周"同时被限定时按 OR 命中（与系统 cron 一致）。

use chrono::{Datelike, Duration, Local, LocalResult, NaiveDate, TimeZone, Timelike};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronSchedule {
    minutes: Vec<u8>,
    hours: Vec<u8>,
    days_of_month: Vec<u8>,
    months: Vec<u8>,
    days_of_week: Vec<u8>,
    dom_restricted: bool,
    dow_restricted: bool,
}

/// 查找上限（天）：覆盖闰年 2 月 29 日这类长周期表达式。
const SEARCH_DAYS: u32 = 1500;

/// 解析表达式；失败返回可直接展示给用户的原因。
pub fn parse(expression: &str) -> Result<CronSchedule, String> {
    let fields: Vec<&str> = expression.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(format!(
            "定时表达式需要 5 段（分 时 日 月 周），当前 {} 段",
            fields.len()
        ));
    }
    Ok(CronSchedule {
        minutes: parse_field(fields[0], 0, 59, "分")?,
        hours: parse_field(fields[1], 0, 23, "时")?,
        days_of_month: parse_field(fields[2], 1, 31, "日")?,
        months: parse_field(fields[3], 1, 12, "月")?,
        days_of_week: parse_dow(fields[4])?,
        dom_restricted: fields[2].trim() != "*",
        dow_restricted: fields[4].trim() != "*",
    })
}

impl CronSchedule {
    /// 严格晚于 `from_ms` 的下一次触发时刻（本机时区）；1500 天内无解返回 None。
    pub fn next_after(&self, from_ms: i64) -> Option<i64> {
        let from = Local.timestamp_millis_opt(from_ms).single()?;
        let start = from + Duration::minutes(1);
        let start_day = start.date_naive();
        let mut day = start_day;
        for _ in 0..SEARCH_DAYS {
            if self.months.contains(&(day.month() as u8)) && self.day_matches(day) {
                let (from_hour, from_minute) = if day == start_day {
                    (start.hour(), start.minute())
                } else {
                    (0, 0)
                };
                for hour in from_hour..24 {
                    for minute in 0..60 {
                        if hour == from_hour && minute < from_minute {
                            continue;
                        }
                        if self.hours.contains(&(hour as u8))
                            && self.minutes.contains(&(minute as u8))
                        {
                            if let Some(timestamp) = local_timestamp(day, hour, minute) {
                                return Some(timestamp);
                            }
                        }
                    }
                }
            }
            day = day.succ_opt()?;
        }
        None
    }

    fn day_matches(&self, day: NaiveDate) -> bool {
        let dom = self.days_of_month.contains(&(day.day() as u8));
        let dow = self
            .days_of_week
            .contains(&(day.weekday().num_days_from_sunday() as u8));
        match (self.dom_restricted, self.dow_restricted) {
            (true, true) => dom || dow,
            (true, false) => dom,
            (false, true) => dow,
            (false, false) => true,
        }
    }
}

/// 本地时刻 → 毫秒时间戳；夏令时跳变不存在的时刻返回 None（跳过）。
fn local_timestamp(day: NaiveDate, hour: u32, minute: u32) -> Option<i64> {
    let naive = day.and_hms_opt(hour, minute, 0)?;
    match Local.from_local_datetime(&naive) {
        LocalResult::Single(value) => Some(value.timestamp_millis()),
        // 夏令时折叠：取较早的一次。
        LocalResult::Ambiguous(early, _) => Some(early.timestamp_millis()),
        LocalResult::None => None,
    }
}

fn parse_field(text: &str, min: u32, max: u32, name: &str) -> Result<Vec<u8>, String> {
    let mut values = Vec::new();
    for part in text.split(',') {
        parse_item(part.trim(), min, max, name, &mut values)?;
    }
    values.sort_unstable();
    values.dedup();
    if values.is_empty() {
        return Err(format!("{name}字段无有效取值：{text}"));
    }
    Ok(values)
}

fn parse_item(part: &str, min: u32, max: u32, name: &str, out: &mut Vec<u8>) -> Result<(), String> {
    if part.is_empty() {
        return Err(format!("{name}字段有空项"));
    }
    let (range_text, step) = match part.split_once('/') {
        Some((range, step_text)) => {
            let step: u32 = step_text
                .trim()
                .parse()
                .map_err(|_| format!("{name}字段步进不是整数：{part}"))?;
            if step == 0 {
                return Err(format!("{name}字段步进不能为 0：{part}"));
            }
            (range.trim(), step)
        }
        None => (part, 1),
    };
    let (start, end) = if range_text == "*" {
        (min, max)
    } else if let Some((left, right)) = range_text.split_once('-') {
        (
            parse_num(left, min, max, name)?,
            parse_num(right, min, max, name)?,
        )
    } else {
        let value = parse_num(range_text, min, max, name)?;
        if part.contains('/') {
            (value, max)
        } else {
            (value, value)
        }
    };
    if start > end {
        return Err(format!("{name}字段区间颠倒：{part}"));
    }
    let mut value = start;
    while value <= end {
        out.push(value as u8);
        value += step;
    }
    Ok(())
}

fn parse_num(text: &str, min: u32, max: u32, name: &str) -> Result<u32, String> {
    let value: u32 = text
        .trim()
        .parse()
        .map_err(|_| format!("{name}字段不是数字：{text}"))?;
    if value < min || value > max {
        return Err(format!("{name}字段超出范围 {min}-{max}：{value}"));
    }
    Ok(value)
}

/// 周字段（0-7，7 → 0）。
fn parse_dow(text: &str) -> Result<Vec<u8>, String> {
    let values = parse_field(text, 0, 7, "周")?;
    let mut mapped: Vec<u8> = values
        .iter()
        .map(|value| if *value == 7 { 0 } else { *value })
        .collect();
    mapped.sort_unstable();
    mapped.dedup();
    Ok(mapped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> i64 {
        local_timestamp(
            NaiveDate::from_ymd_opt(year, month, day).unwrap(),
            hour,
            minute,
        )
        .unwrap()
    }

    fn next(schedule: &str, from: i64) -> Option<i64> {
        parse(schedule).unwrap().next_after(from)
    }

    #[test]
    fn rejects_bad_expressions() {
        assert!(parse("* * * *").is_err(), "4 段应报错");
        assert!(parse("0 9 * * * *").is_err(), "6 段应报错");
        assert!(parse("60 9 * * *").is_err(), "分钟越界应报错");
        assert!(parse("0 24 * * *").is_err(), "小时越界应报错");
        assert!(parse("0 9 0 * *").is_err(), "日越界应报错");
        assert!(parse("*/0 * * * *").is_err(), "步进 0 应报错");
        assert!(parse("a * * * *").is_err(), "非数字应报错");
        assert!(parse("0 9 * * 8").is_err(), "周越界应报错");
    }

    #[test]
    fn every_minute_advances_one() {
        let from = ts(2026, 1, 5, 10, 7);
        assert_eq!(next("* * * * *", from), Some(ts(2026, 1, 5, 10, 8)));
        // 秒级时间戳对齐到下一分钟。
        assert_eq!(next("* * * * *", from + 30_000), Some(ts(2026, 1, 5, 10, 8)));
    }

    #[test]
    fn daily_rolls_to_next_day() {
        let from = ts(2026, 1, 5, 10, 0);
        assert_eq!(next("0 9 * * *", from), Some(ts(2026, 1, 6, 9, 0)));
    }

    #[test]
    fn step_minutes_find_next_slot() {
        let from = ts(2026, 1, 5, 10, 7);
        assert_eq!(next("*/15 * * * *", from), Some(ts(2026, 1, 5, 10, 15)));
    }

    #[test]
    fn weekday_schedule() {
        // 2026-01-05 是周一；从周三 10:00 出发应到下一个周一 10:00。
        assert_eq!(
            next("0 10 * * 1", ts(2026, 1, 7, 10, 0)),
            Some(ts(2026, 1, 12, 10, 0))
        );
        // 7 等价周日：2026-01-11 是周日。
        assert_eq!(
            next("0 10 * * 7", ts(2026, 1, 7, 10, 0)),
            Some(ts(2026, 1, 11, 10, 0))
        );
    }

    #[test]
    fn dom_and_dow_both_restricted_uses_or() {
        // 13 号或周五，先到 2026-01-09（周五）。
        assert_eq!(
            next("0 0 13 * 5", ts(2026, 1, 5, 0, 0)),
            Some(ts(2026, 1, 9, 0, 0))
        );
    }

    #[test]
    fn month_restriction() {
        assert_eq!(
            next("0 12 1 3 *", ts(2026, 1, 5, 0, 0)),
            Some(ts(2026, 3, 1, 12, 0))
        );
    }

    #[test]
    fn list_and_range() {
        let from = ts(2026, 1, 5, 10, 20);
        assert_eq!(next("10,30 * * * *", from), Some(ts(2026, 1, 5, 10, 30)));
        assert_eq!(next("0 9-11 * * *", from), Some(ts(2026, 1, 5, 11, 0)));
    }

    #[test]
    fn impossible_expression_returns_none() {
        assert_eq!(next("0 0 30 2 *", ts(2026, 1, 5, 0, 0)), None);
    }
}
