//! 用量统计：跨引擎 token 汇总与成本估算（全部来自引擎数据的只读视图）。
//!
//! 数据源与口径：
//! - OpenCode：`session_v2` 的 tokens_* 列（输入 / 输出 / 思考 / 缓存读写天然分列）；
//! - Codex：`~/.codex/sessions/**/rollout-*.jsonl` 尾部的最后一次 token_count
//!   （会话累计值；OpenAI 语义里缓存命中是输入的子集，这里归一为「未缓存输入 + 缓存读」）；
//! - Claude：`~/.claude/projects/**/*.jsonl` 的 message.usage 累加（含子代理，真实计费口径）。
//!
//! 归一后各列口径互不重叠，可直接相加。成本为**估算**：内置常见模型价格表
//! （美元 / 百万 token），未收录的模型只报 token（前端显示「未收录」）。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use chrono::TimeZone;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::Value;

use crate::engines::claude;
use crate::engines::codex::{locator as codex_locator, rollout};
use crate::engines::opencode::locator as opencode_locator;
use crate::engines::time::{file_modified_ms, parse_iso_ms};

/// 引擎展示顺序与名称。
const ENGINES: [(&str, &str); 3] = [
    ("opencode", "OpenCode"),
    ("codex", "Codex"),
    ("claude", "Claude Code"),
];

/// token 分项：各列口径互不重叠，可直接相加。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenTotals {
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl TokenTotals {
    fn add(&mut self, other: &TokenTotals) {
        self.input += other.input;
        self.output += other.output;
        self.reasoning += other.reasoning;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }

    /// 合计（口径不重叠，直接相加）。
    pub fn total(&self) -> u64 {
        self.input + self.output + self.reasoning + self.cache_read + self.cache_write
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    pub model: String,
    pub tokens: TokenTotals,
    pub sessions: u32,
    /// 估算成本（美元）；价格表未收录时为 None。
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineUsage {
    pub engine: String,
    pub label: String,
    pub tokens: TokenTotals,
    pub sessions: u32,
    /// 已收录模型的估算成本合计（美元）。
    pub cost_usd: f64,
    /// 价格表未收录的模型名（成本显示「未收录」）。
    pub unpriced_models: Vec<String>,
    pub models: Vec<ModelUsage>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyUsage {
    /// 本地日期 `YYYY-MM-DD`。
    pub date: String,
    pub tokens: TokenTotals,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    /// 统计窗口天数；0 = 全部。
    pub days: u32,
    pub sessions: u32,
    pub totals: TokenTotals,
    /// 估算成本合计（仅已收录模型）。
    pub cost_usd: f64,
    pub engines: Vec<EngineUsage>,
    pub daily: Vec<DailyUsage>,
    /// 数据源说明 / 缺数提示。
    pub notes: Vec<String>,
}

/// 单会话用量（收集阶段产出；at_ms 用于窗口过滤与每日归档）。
#[derive(Debug, Clone)]
struct SessionUsage {
    engine: &'static str,
    model: String,
    at_ms: i64,
    tokens: TokenTotals,
}

// ---------------------------------------------------------------- 收集（各引擎）

/// OpenCode：session_v2 会话级汇总列（归档时间取会话创建时间）。
fn opencode_sessions(db: &Path) -> Result<Vec<SessionUsage>, String> {
    let connection = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|cause| format!("打开 opencode.db 失败：{cause}"))?;
    let mut statement = connection
        .prepare(
            "SELECT model, tokens_input, tokens_output, tokens_reasoning,
                    tokens_cache_read, tokens_cache_write, time_created
             FROM session_v2",
        )
        .map_err(|cause| format!("查询会话用量失败：{cause}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })
        .map_err(|cause| format!("查询会话用量失败：{cause}"))?;

    let mut out = Vec::new();
    for row in rows {
        let (model, input, output, reasoning, cache_read, cache_write, at_ms) =
            row.map_err(|cause| format!("读取会话用量失败：{cause}"))?;
        let tokens = TokenTotals {
            input: input.max(0) as u64,
            output: output.max(0) as u64,
            reasoning: reasoning.max(0) as u64,
            cache_read: cache_read.max(0) as u64,
            cache_write: cache_write.max(0) as u64,
        };
        if tokens.total() == 0 {
            continue;
        }
        out.push(SessionUsage {
            engine: "opencode",
            model: parse_opencode_model(model.as_deref()),
            at_ms,
            tokens,
        });
    }
    Ok(out)
}

/// session_v2.model 是 JSON（`{"id":"...","providerID":"..."}`），取 id。
fn parse_opencode_model(raw: Option<&str>) -> String {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return "未知".to_string();
    };
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|value| {
            value
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| text.to_string())
}

/// Codex：遍历 rollout 文件，取每个文件尾部最后一次 token_count（累计值）。
fn codex_sessions(home: &Path) -> Vec<SessionUsage> {
    let mut paths = Vec::new();
    collect_rollouts(&home.join("sessions"), &mut paths);

    let mut out = Vec::new();
    for path in paths {
        let Some(count) = rollout::tail_token_count(&path) else {
            continue;
        };
        let model = rollout::head_session_info(&path)
            .map(|(_, model)| model)
            .unwrap_or_else(|| "未知".to_string());
        let at_ms = if count.at_ms > 0 {
            count.at_ms
        } else {
            file_modified_ms(&path)
        };
        let tokens = codex_token_totals(&count);
        if tokens.total() == 0 {
            continue;
        }
        out.push(SessionUsage {
            engine: "codex",
            model,
            at_ms,
            tokens,
        });
    }
    out
}

/// 归一 Codex 口径：缓存读是输入的子集（OpenAI 语义），
/// 拆成「未缓存输入 + 缓存读」后与其他引擎可加总。
fn codex_token_totals(count: &rollout::TokenCount) -> TokenTotals {
    TokenTotals {
        input: count.input.saturating_sub(count.cached_input).max(0) as u64,
        output: count.output.max(0) as u64,
        reasoning: count.reasoning.max(0) as u64,
        cache_read: count.cached_input.max(0) as u64,
        cache_write: 0,
    }
}

fn collect_rollouts(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rollouts(&path, out);
            continue;
        }
        let is_rollout = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
            .unwrap_or(false);
        if is_rollout {
            out.push(path);
        }
    }
}

/// Claude：projects 下一层的会话档案，逐行累加 message.usage（含子代理）。
fn claude_sessions(projects: &Path) -> Vec<SessionUsage> {
    let mut out = Vec::new();
    let Ok(dirs) = fs::read_dir(projects) else {
        return out;
    };
    for dir in dirs.flatten() {
        let dir_path = dir.path();
        if !dir_path.is_dir() {
            continue;
        }
        let Ok(files) = fs::read_dir(&dir_path) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            if let Some(session) = claude_file_usage(&path) {
                out.push(session);
            }
        }
    }
    out
}

/// 解析单个 Claude 会话档案的用量（与浏览层口径一致：含 sidechain 子代理调用）。
fn claude_file_usage(path: &Path) -> Option<SessionUsage> {
    let text = fs::read_to_string(path).ok()?;
    let mut tokens = TokenTotals::default();
    let mut model = String::new();
    let mut at_ms: i64 = 0;
    for line in text.lines() {
        if !line.contains("\"usage\"") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(usage) = value.pointer("/message/usage") else {
            continue;
        };
        let get = |key: &str| usage.get(key).and_then(Value::as_i64).unwrap_or(0);
        tokens.input += get("input_tokens").max(0) as u64;
        tokens.output += get("output_tokens").max(0) as u64;
        tokens.cache_write += get("cache_creation_input_tokens").max(0) as u64;
        tokens.cache_read += get("cache_read_input_tokens").max(0) as u64;
        if let Some(name) = value
            .pointer("/message/model")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
        {
            model = name.to_string();
        }
        if let Some(ts) = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_iso_ms)
        {
            at_ms = at_ms.max(ts);
        }
    }
    if tokens.total() == 0 {
        return None;
    }
    Some(SessionUsage {
        engine: "claude",
        model: if model.is_empty() {
            "未知".to_string()
        } else {
            model
        },
        at_ms: if at_ms > 0 { at_ms } else { file_modified_ms(path) },
        tokens,
    })
}

// ---------------------------------------------------------------- 成本估算

/// 价格（美元 / 百万 token）：输入、输出、缓存读、缓存写。
#[derive(Debug, Clone, Copy)]
struct Price {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
}

/// 内置价格表（估算用，最后核对：2026-10）。未收录返回 None。
fn price_for(model: &str) -> Option<Price> {
    let name = model.to_ascii_lowercase();
    if name.ends_with("-free") || name.contains("-free-") {
        return Some(Price {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        });
    }
    if name.contains("claude-opus-4") {
        return Some(Price {
            input: 15.0,
            output: 75.0,
            cache_read: 1.5,
            cache_write: 18.75,
        });
    }
    if name.contains("claude-sonnet-4") {
        return Some(Price {
            input: 3.0,
            output: 15.0,
            cache_read: 0.3,
            cache_write: 3.75,
        });
    }
    if name.contains("claude-haiku-4") {
        return Some(Price {
            input: 1.0,
            output: 5.0,
            cache_read: 0.1,
            cache_write: 1.25,
        });
    }
    if name.contains("gpt-5-mini") {
        return Some(Price {
            input: 0.25,
            output: 2.0,
            cache_read: 0.025,
            cache_write: 0.25,
        });
    }
    if name.contains("gpt-5") {
        return Some(Price {
            input: 1.25,
            output: 10.0,
            cache_read: 0.125,
            cache_write: 1.25,
        });
    }
    None
}

/// 估算单模型用量成本（思考按输出价计）；未收录返回 None。
fn estimate_cost(model: &str, tokens: &TokenTotals) -> Option<f64> {
    let price = price_for(model)?;
    let per = |count: u64, rate: f64| count as f64 / 1_000_000.0 * rate;
    Some(
        per(tokens.input, price.input)
            + per(tokens.output, price.output)
            + per(tokens.reasoning, price.output)
            + per(tokens.cache_read, price.cache_read)
            + per(tokens.cache_write, price.cache_write),
    )
}

// ---------------------------------------------------------------- 汇总

/// 窗口起点（本地时区今天 0 点往前 days-1 天）；days=0 表示不限。
fn window_from_ms(days: u32) -> Option<i64> {
    if days == 0 {
        return None;
    }
    let start = chrono::Local::now().date_naive() - chrono::Days::new(u64::from(days - 1));
    let naive = start.and_hms_opt(0, 0, 0)?;
    chrono::Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|moment| moment.timestamp_millis())
}

/// 时间戳 → 本地日期键 `YYYY-MM-DD`。
fn day_key(ms: i64) -> String {
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|moment| moment.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// 生成每日序列：窗口内逐日补齐；「全部」最多回看 60 天（更早的仍计入总量）。
fn daily_series(daily: &BTreeMap<String, TokenTotals>, days: u32) -> Vec<DailyUsage> {
    const MAX_SPAN: i64 = 60;
    let today = chrono::Local::now().date_naive();
    let start = if days > 0 {
        today - chrono::Days::new(u64::from(days - 1))
    } else {
        let earliest = daily
            .keys()
            .next()
            .and_then(|key| chrono::NaiveDate::parse_from_str(key, "%Y-%m-%d").ok())
            .unwrap_or(today);
        let span = (today - earliest).num_days();
        if span >= MAX_SPAN {
            today - chrono::Days::new((MAX_SPAN - 1) as u64)
        } else {
            earliest
        }
    };

    let mut out = Vec::new();
    let mut cursor = start;
    while cursor <= today {
        let key = cursor.format("%Y-%m-%d").to_string();
        out.push(DailyUsage {
            date: key.clone(),
            tokens: daily.get(&key).copied().unwrap_or_default(),
        });
        cursor += chrono::TimeDelta::days(1);
    }
    out
}

/// 把逐会话用量汇成面板数据（纯函数，便于测试）。
fn build_summary(sessions: &[SessionUsage], days: u32, notes: Vec<String>) -> UsageSummary {
    let from_ms = window_from_ms(days);
    let filtered: Vec<&SessionUsage> = sessions
        .iter()
        .filter(|session| from_ms.map_or(true, |from| session.at_ms >= from))
        .collect();

    let mut totals = TokenTotals::default();
    let mut daily_map: BTreeMap<String, TokenTotals> = BTreeMap::new();
    type ModelAcc = BTreeMap<String, (TokenTotals, u32)>;
    type EngineAcc = (TokenTotals, u32, ModelAcc);
    let mut engine_acc: BTreeMap<&'static str, EngineAcc> = BTreeMap::new();

    for session in &filtered {
        totals.add(&session.tokens);
        daily_map
            .entry(day_key(session.at_ms))
            .or_default()
            .add(&session.tokens);
        let acc = engine_acc.entry(session.engine).or_default();
        acc.0.add(&session.tokens);
        acc.1 += 1;
        let model_acc = acc.2.entry(session.model.clone()).or_default();
        model_acc.0.add(&session.tokens);
        model_acc.1 += 1;
    }

    let mut engines = Vec::new();
    for (engine_id, label) in ENGINES {
        let Some((tokens, session_count, models)) = engine_acc.remove(engine_id) else {
            continue;
        };
        let mut cost_usd = 0.0;
        let mut unpriced: BTreeSet<String> = BTreeSet::new();
        let mut model_list: Vec<ModelUsage> = models
            .into_iter()
            .map(|(model, (tokens, sessions))| {
                let cost = estimate_cost(&model, &tokens);
                match cost {
                    Some(value) => cost_usd += value,
                    None => {
                        unpriced.insert(model.clone());
                    }
                }
                ModelUsage {
                    model,
                    tokens,
                    sessions,
                    cost_usd: cost,
                }
            })
            .collect();
        model_list.sort_by(|a, b| b.tokens.total().cmp(&a.tokens.total()));
        engines.push(EngineUsage {
            engine: engine_id.to_string(),
            label: label.to_string(),
            tokens,
            sessions: session_count,
            cost_usd,
            unpriced_models: unpriced.into_iter().collect(),
            models: model_list,
        });
    }

    UsageSummary {
        days,
        sessions: filtered.len() as u32,
        totals,
        cost_usd: engines.iter().map(|engine| engine.cost_usd).sum(),
        engines,
        daily: daily_series(&daily_map, days),
        notes,
    }
}

/// 汇总各引擎本地用量（days=0 表示全部时间；只读引擎数据源）。
pub fn summary(days: u32) -> Result<UsageSummary, String> {
    let mut sessions = Vec::new();
    let mut notes = Vec::new();

    match opencode_locator::database_path() {
        Some(db) => match opencode_sessions(&db) {
            Ok(list) => sessions.extend(list),
            Err(cause) => notes.push(format!("OpenCode 用量读取失败：{cause}")),
        },
        None => notes.push("OpenCode：未找到本地数据库，用量暂缺。".to_string()),
    }

    match codex_locator::codex_home() {
        Some(home) => sessions.extend(codex_sessions(&home)),
        None => notes.push("Codex：未找到 ~/.codex，用量暂缺。".to_string()),
    }

    match claude::locator::projects_dir() {
        Some(dir) => {
            let list = claude_sessions(&dir);
            if list.is_empty() {
                notes.push("Claude Code：档案里暂无可统计的用量。".to_string());
            }
            sessions.extend(list);
        }
        None => notes.push("Claude Code：本机未发现 ~/.claude 档案，用量暂缺。".to_string()),
    }

    Ok(build_summary(&sessions, days, notes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(engine: &'static str, model: &str, at_ms: i64, input: u64, output: u64) -> SessionUsage {
        SessionUsage {
            engine,
            model: model.to_string(),
            at_ms,
            tokens: TokenTotals {
                input,
                output,
                ..TokenTotals::default()
            },
        }
    }

    #[test]
    fn build_summary_splits_engines_and_estimates_cost() {
        let now = chrono::Local::now().timestamp_millis();
        let sessions = vec![
            session("opencode", "deepseek-v4.1-flash-free", now, 1_000, 100),
            session("opencode", "custom-unknown-model", now, 2_000, 200),
            session("codex", "gpt-5-codex", now, 3_000, 300),
        ];
        let summary = build_summary(&sessions, 7, Vec::new());
        assert_eq!(summary.sessions, 3);
        assert_eq!(summary.totals.input, 6_000);
        assert_eq!(summary.engines.len(), 2);

        let opencode = &summary.engines[0];
        assert_eq!(opencode.engine, "opencode");
        assert_eq!(opencode.sessions, 2);
        assert_eq!(opencode.unpriced_models, vec!["custom-unknown-model".to_string()]);

        let codex = &summary.engines[1];
        assert_eq!(codex.engine, "codex");
        // gpt-5-codex：3000/1e6*1.25 + 300/1e6*10 = 0.00675
        assert!((codex.cost_usd - 0.00675).abs() < 1e-9);
        assert!((summary.cost_usd - 0.00675).abs() < 1e-9);
    }

    #[test]
    fn codex_totals_normalize_cached_input() {
        let count = rollout::TokenCount {
            at_ms: 1,
            input: 1_000,
            cached_input: 400,
            output: 50,
            reasoning: 5,
        };
        let tokens = codex_token_totals(&count);
        assert_eq!(tokens.input, 600);
        assert_eq!(tokens.cache_read, 400);
        assert_eq!(tokens.total(), 1_055);

        // 异常数据（缓存读大于输入）不 panic、不为负。
        let weird = rollout::TokenCount {
            at_ms: 1,
            input: 10,
            cached_input: 99,
            output: 0,
            reasoning: 0,
        };
        let tokens = codex_token_totals(&weird);
        assert_eq!(tokens.input, 0);
        assert_eq!(tokens.cache_read, 99);
    }

    #[test]
    fn price_table_covers_known_models() {
        assert!(matches!(price_for("deepseek-v4.1-flash-free"), Some(price) if price.input == 0.0));
        assert!(price_for("claude-sonnet-4-5").is_some());
        assert!(price_for("claude-opus-4-1").is_some());
        assert!(price_for("gpt-5-codex").is_some());
        assert!(price_for("deepseek-v4-flash").is_none());
        let zero = estimate_cost(
            "deepseek-v4.1-flash-free",
            &TokenTotals {
                input: 1_000_000,
                ..Default::default()
            },
        );
        assert_eq!(zero, Some(0.0));
        assert_eq!(estimate_cost("deepseek-v4-flash", &TokenTotals::default()), None);
    }

    #[test]
    fn daily_series_fills_every_day_in_window() {
        let today = chrono::Local::now().date_naive();
        let now_ms = chrono::Local::now().timestamp_millis();
        let key = day_key(now_ms);
        let mut daily: BTreeMap<String, TokenTotals> = BTreeMap::new();
        daily.insert(
            key.clone(),
            TokenTotals {
                input: 42,
                ..Default::default()
            },
        );
        let series = daily_series(&daily, 3);
        assert_eq!(series.len(), 3);
        assert_eq!(
            series[0].date,
            (today - chrono::Days::new(2)).format("%Y-%m-%d").to_string()
        );
        assert_eq!(series[2].date, key);
        assert_eq!(series[2].tokens.input, 42);
        assert_eq!(series[0].tokens.total(), 0);
    }

    #[test]
    fn claude_file_usage_sums_assistant_usage() {
        let dir = std::env::temp_dir().join(format!("prism-usage-claude-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("临时目录");
        let path = dir.join("session.jsonl");
        let fixture = concat!(
            r#"{"type":"assistant","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":10,"output_tokens":21,"cache_creation_input_tokens":100,"cache_read_input_tokens":1000}},"timestamp":"2026-10-01T10:00:03.000Z"}"#,
            "\n",
            r#"{"type":"assistant","isSidechain":true,"message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":5,"output_tokens":30,"cache_creation_input_tokens":0,"cache_read_input_tokens":50}},"timestamp":"2026-10-01T10:00:09.000Z"}"#,
            "\n",
            r#"{"type":"user","message":{"content":"hi"}}"#,
            "\n"
        );
        std::fs::write(&path, fixture).expect("写 fixture");

        let usage = claude_file_usage(&path).expect("用量");
        assert_eq!(usage.tokens.input, 15);
        assert_eq!(usage.tokens.output, 51);
        assert_eq!(usage.tokens.cache_read, 1050);
        assert_eq!(usage.tokens.cache_write, 100);
        assert_eq!(usage.model, "claude-sonnet-4-5");
        assert!(usage.at_ms > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_opencode_model_reads_json_id() {
        assert_eq!(
            parse_opencode_model(Some(r#"{"id":"deepseek-v4.1-flash-free","providerID":"opencode"}"#)),
            "deepseek-v4.1-flash-free"
        );
        assert_eq!(parse_opencode_model(None), "未知");
        assert_eq!(parse_opencode_model(Some("   ")), "未知");
        assert_eq!(parse_opencode_model(Some("raw-name")), "raw-name");
    }
}
