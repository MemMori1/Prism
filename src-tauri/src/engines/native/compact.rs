//! `/compact`：把会话历史压成一份交接摘要，然后重写 `messages.jsonl`。
//!
//! 为什么是「重写」而不是「打个标记」：上下文是真正发给模型的那份数据，
//! 标记不会让它变小。只有把原文从会话文件里换掉，下一次请求才真的省 token。
//!
//! 代价是破坏性的，所以两道保险：
//! 1. 原文件先复制成 `messages.jsonl.bak`（只留最近一次，可手工退回）；
//! 2. 摘要本身留在时间线上（`role = compact`）——「记忆」变成了什么，用户看得见。
//!
//! 长会话分段滚动压缩：一段约 40k 字符，超出就只保留最近 8 段，并在摘要头
//! 如实写下「更早的 N 条没纳入」。宁可少一点历史，也不能悄悄丢掉还说压缩完了。

use serde::Serialize;
use serde_json::json;
use tauri::AppHandle;

use super::{agent, config, provider, store};

/// 每段塞多少字符（40k 字符 ≈ 1.5 万 token，给模型留足窗口）。
const CHUNK_CHARS: usize = 40_000;
/// 最多几段。再多就只留最近的：更早的历史价值低，但压缩成本是线性涨的。
const MAX_CHUNKS: usize = 8;
/// 少于这个量不压：压完反而更难读（摘要本身也要一千多字）。
const MIN_CHARS: usize = 2_000;
/// 单条工具结果最多带多少字符进摘要。
const TOOL_CLIP: usize = 1_500;
/// 工具调用入参最多带多少字符。
const ARGS_CLIP: usize = 400;
/// 用户额外要求最多听多少字符。
const INSTRUCTION_CLIP: usize = 500;

const SUMMARY_SYSTEM: &str = "\
你是会话压缩器。把一段编码 agent 的执行历史压成一份「交接摘要」——接手的人看不到原文，只能靠这份摘要继续干活。
规则：
1. 只写真发生过的事。文件用完整路径，命令写原样，测试写数字（通过 / 失败几条）。
2. 用户的明确要求与约束逐条保留，包括否定条件（「不要动 X」）、版本、风格与验收标准。
3. 未完成的事、卡在哪、下一步做什么，单独写清楚——这是接手的人最需要的一段。
4. 工具输出只留结论与关键行，不要整段复制。
5. 输出 Markdown，固定四节：`## 目标`、`## 已完成`、`## 关键文件与命令`、`## 待办与约束`。
不要寒暄，不要解释你在做什么，全篇不超过 1500 字。";

/// 压缩结果里前端要用的事实（同时用于 `session.compact.done` 事件）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Done {
    /// 压缩前有几条消息。
    messages: usize,
    /// 压缩前的字符数。
    before: usize,
    /// 压缩后留在会话里的字符数。
    after: usize,
    /// 摘要调用本身吃掉的 token（也记进会话累计）。
    tokens: i64,
}

/// 为什么没压成：用户叫停，还是真出错。
enum Failure {
    Cancelled,
    Failed(String),
}

impl From<String> for Failure {
    fn from(error: String) -> Self {
        Failure::Failed(error)
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
}

fn clip(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    if count <= limit {
        return text.to_string();
    }
    let head: String = text.chars().take(limit).collect();
    format!("{head}…（后 {count} 字符略）")
}

/// 一条消息在摘要请求里的样子：只留模型需要的事实，按字符量裁。
fn render(message: &store::StoredMessage) -> Option<String> {
    match message.role.as_str() {
        "user" => {
            let images = message
                .parts
                .iter()
                .filter(|part| matches!(part, store::StoredPart::Image { .. }))
                .count();
            let suffix = if images > 0 {
                format!("（附了 {images} 张图）")
            } else {
                String::new()
            };
            Some(format!("【用户】{}{suffix}", message.text.trim()))
        }
        "assistant" => {
            let mut out = String::from("【助手】");
            out.push_str(message.text.trim());
            for call in &message.tool_calls {
                out.push_str(&format!(
                    "\n【调用 {}】{}",
                    call.name,
                    clip(&call.arguments, ARGS_CLIP)
                ));
            }
            Some(out)
        }
        "tool" => Some(format!(
            "【工具结果】{}",
            clip(message.text.trim(), TOOL_CLIP)
        )),
        // 上一次压缩留下的摘要：再压时并进去，不然「摘要的摘要」会断片。
        "compact" => Some(format!("【更早的摘要】{}", message.text.trim())),
        _ => None,
    }
}

/// 时间线上的分隔标记（`role = system`）。
///
/// 这类消息**不会被发给模型**（`provider_messages` 只认 user / assistant / tool /
/// compact），所以它不占上下文，只是在会话里留一道「这里发生过什么」的分界线。
fn marker(text: &str) -> store::StoredMessage {
    store::StoredMessage {
        id: agent::new_message_id(),
        role: "system".to_string(),
        time: now_ms(),
        text: text.to_string(),
        parts: vec![store::StoredPart::Text {
            text: text.to_string(),
        }],
        tool_calls: Vec::new(),
        tool_call_id: None,
        tokens: None,
    }
}

/// 最后一条是没人回答的用户消息（上一轮失败 / 中断）就留着：
/// 那是当下的问题，把它压进摘要等于替用户把话咽了。
fn pending_user(history: &[store::StoredMessage]) -> Option<store::StoredMessage> {
    history
        .last()
        .filter(|message| message.role == "user")
        .cloned()
}

fn size_of(message: &store::StoredMessage) -> usize {
    let mut size = message.text.chars().count();
    for call in &message.tool_calls {
        size += call.name.chars().count() + call.arguments.chars().count();
    }
    size
}

/// 把渲染好的行按预算切段；超出 `MAX_CHUNKS` 时丢掉最早的，返回丢了多少行。
///
/// 单行超预算也让它自己成一段：工具输出已经裁过，真撞上极端情况也只是那一段
/// 偏长——模型按 token 计费，不会因为字数多一点就拒绝。
fn chunks_of(lines: &[String]) -> (Vec<Vec<usize>>, usize) {
    let mut chunks: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut size = 0usize;
    for (index, line) in lines.iter().enumerate() {
        let add = line.chars().count();
        if !current.is_empty() && size + add > CHUNK_CHARS {
            chunks.push(std::mem::take(&mut current));
            size = 0;
        }
        current.push(index);
        size += add;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    let mut dropped = 0usize;
    while chunks.len() > MAX_CHUNKS {
        if let Some(first) = chunks.first() {
            dropped += first.len();
        }
        chunks.remove(0);
    }
    (chunks, dropped)
}

/// 一次请求要发出去的那段正文。
fn body_of(lines: &[String], chunk: &[usize]) -> String {
    chunk
        .iter()
        .filter_map(|index| lines.get(*index))
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 摘要请求的 user 内容：第一段从零写，后面几段是「旧摘要 + 新对话」的合并。
fn prompt_body(previous: Option<&str>, body: &str, instruction: &str) -> String {
    let mut out = String::new();
    match previous {
        Some(previous) => {
            out.push_str("已有摘要（较早的对话）：\n\n");
            out.push_str(previous.trim());
            out.push_str("\n\n新增对话（较晚的对话）：\n\n");
            out.push_str(body);
            out.push_str("\n\n把两者合并成一份新摘要，规则不变：已有的结论不要丢，被后文推翻的要改掉，");
            out.push_str("已完成与未完成的界线按最新状态写。");
        }
        None => {
            out.push_str("把下面这段会话历史压成摘要：\n\n");
            out.push_str(body);
        }
    }
    if !instruction.trim().is_empty() {
        out.push_str("\n\n本次压缩的额外要求（用户提的，必须照顾）：\n");
        out.push_str(instruction.trim());
    }
    out
}

fn summarize(
    provider_cfg: &config::ProviderConfig,
    model: &str,
    prompt: &str,
    stop: &dyn Fn() -> bool,
) -> Result<(String, i64, i64, i64), Failure> {
    let messages = vec![
        json!({ "role": "system", "content": SUMMARY_SYSTEM }),
        json!({ "role": "user", "content": prompt }),
    ];
    // 摘要不需要工具：给它空清单，模型就只会写文字。
    let mut sink = |_: &str| {};
    let mut think = |_: &str| {};
    let outcome = provider::chat(provider_cfg, model, &messages, &[], &mut sink, &mut think, stop)
        .map_err(|error| match error {
            provider::ChatError::Cancelled => Failure::Cancelled,
            other => Failure::Failed(other.to_string()),
        })?;
    let text = outcome.text.trim().to_string();
    if text.is_empty() {
        return Err(Failure::Failed(
            "模型没给出摘要（返回为空）：换一个模型或再试一次".to_string(),
        ));
    }
    let usage = outcome
        .usage
        .unwrap_or(provider::Usage::default());
    Ok((text, usage.input, usage.output, usage.cached))
}

/// 把一串已渲染的文本行压成摘要，**不落盘**。
///
/// 聊天路径走 [`run`]：它有会话文件，压完要写回一条 `compact` 消息。编排节点
/// 没有会话文件，消息只是一个 `Vec<Value>`，所以只借这一步。分块、链式合并、
/// 字符上限全沿用 [`chunks_of`] 与 [`prompt_body`]——两条路压出来的摘要口径一致，
/// 换模型或调分块大小时不会出现「聊天压得动、编排压不动」。
///
/// 返回 `(摘要正文, 参与压缩的条数)`。
pub(crate) fn summarize_lines(
    lines: &[String],
    provider_cfg: &config::ProviderConfig,
    model: &str,
    stop: &dyn Fn() -> bool,
) -> Result<(String, usize), String> {
    if lines.is_empty() {
        return Err("没有可压缩的内容".to_string());
    }
    let (chunks, dropped) = chunks_of(lines);
    let mut summary: Option<String> = None;
    for chunk in &chunks {
        // 摘要自己也是一次模型调用：用户点了停止就别再烧钱。
        if stop() {
            return Err("压缩被中断".to_string());
        }
        let prompt = prompt_body(summary.as_deref(), &body_of(lines, chunk), "");
        let (text, ..) = summarize(provider_cfg, model, &prompt, stop).map_err(|failure| {
            match failure {
                Failure::Cancelled => "压缩被中断".to_string(),
                Failure::Failed(why) => why,
            }
        })?;
        summary = Some(text);
    }
    let summary = summary.ok_or_else(|| "没有可压缩的内容".to_string())?;
    // 不静默：太老的消息没进摘要时写明白，模型才知道自己没看到全貌。
    let header = if dropped > 0 {
        format!("（更早的 {dropped} 条消息超出压缩上限，未纳入摘要）\n\n")
    } else {
        String::new()
    };
    Ok((format!("{header}{summary}"), lines.len()))
}

/// 真正干活的那一段（在后台线程里跑）。
fn run(
    app: &AppHandle,
    session_id: &str,
    raw: &str,
    provider_cfg: &config::ProviderConfig,
    model: &str,
    instruction: &str,
    start: store::StoredMessage,
    slot: &agent::RunSlot,
) -> Result<Done, Failure> {
    let history = store::messages(raw);
    let before: usize = history.iter().map(size_of).sum();
    let lines: Vec<String> = history.iter().filter_map(render).collect();
    if lines.is_empty() {
        return Err(Failure::Failed("这个会话还没有可压缩的内容".to_string()));
    }
    let (chunks, dropped) = chunks_of(&lines);
    let total = chunks.len();
    let stop = || slot.stopped();

    let mut summary: Option<String> = None;
    let mut tokens_in = 0i64;
    let mut tokens_out = 0i64;
    let mut tokens_cached = 0i64;
    for (index, chunk) in chunks.iter().enumerate() {
        if slot.stopped() {
            return Err(Failure::Cancelled);
        }
        super::emit_event(
            app,
            "session.compact.progress",
            json!({
                "sessionID": session_id,
                "chunk": index + 1,
                "total": total,
            }),
        );
        let prompt = prompt_body(summary.as_deref(), &body_of(&lines, chunk), instruction);
        let (text, used_in, used_out, used_cached) = summarize(provider_cfg, model, &prompt, &stop)?;
        summary = Some(text);
        tokens_in += used_in;
        tokens_out += used_out;
        tokens_cached += used_cached;
    }
    let summary = summary.ok_or_else(|| Failure::Failed("没有可压缩的内容".to_string()))?;
    if slot.stopped() {
        return Err(Failure::Cancelled);
    }

    let header = if dropped > 0 {
        format!("（更早的 {dropped} 条消息超出压缩上限，未纳入摘要）\n\n")
    } else {
        String::new()
    };
    let text = format!("{header}{summary}");

    let summary = store::StoredMessage {
        id: agent::new_message_id(),
        role: "compact".to_string(),
        time: now_ms(),
        text: text.clone(),
        parts: vec![store::StoredPart::Text { text: text.clone() }],
        tool_calls: Vec::new(),
        tool_call_id: None,
        tokens: Some(store::StoredTokens {
            input: tokens_in,
            output: tokens_out,
            cached: tokens_cached,
        }),
    };
    let keep = pending_user(&history);
    // 「压了多少」写在结束分隔线上：这是用户唯一能核对压缩有没有白跑的地方。
    let after: usize = size_of(&summary) + keep.as_ref().map(size_of).unwrap_or(0);
    let end = marker(&format!(
        "会话压缩已结束 · {} 条消息 · {before} → {after} 字符",
        history.len()
    ));

    // 开始 / 摘要 / 结束 三段都要留在文件里：不然用户只看到一张孤零零的摘要卡，
    // 不知道它的边界在哪，也不知道中间发生了什么。
    let mut kept: Vec<store::StoredMessage> = vec![start, summary];
    kept.push(end);
    if let Some(message) = keep {
        kept.push(message);
    }
    store::replace_messages(raw, &kept, tokens_in, tokens_out, tokens_cached)?;

    Ok(Done {
        messages: history.len(),
        before,
        after,
        tokens: tokens_in + tokens_out,
    })
}

/// 自动压缩的结果：压成了 / 被叫停。
///
/// 压缩的具体数字（前后字符数、token）由 rescue 自己发 `session.compact.done`
/// 事件，调用方只需要知道「成了没」。
pub(crate) enum Rescue {
    Done,
    Cancelled,
}

/// 自动压缩：上下文超限时由回合循环同步调用，压完由调用方重载历史重试。
///
/// 与手动 `/compact` 用同一台机器（分段摘要 + 备份 + 重写），差别只在这
/// 两条：它跑在回合线程里，槽位已被这一轮占着——不 take、不发
/// execution 事件，「停止」打断压缩就是打断这一轮，语义一致；起始标记
/// 写明是自动的，时间线上能看出这条摘要不是用户敲出来的。
pub(crate) fn rescue(
    app: &AppHandle,
    session_id: &str,
    raw: &str,
    provider_cfg: &config::ProviderConfig,
    model: &str,
    slot: &agent::RunSlot,
) -> Result<Rescue, String> {
    let history = store::messages(raw);
    let before: usize = history.iter().map(size_of).sum();
    let count = history.len();
    let start = marker(&format!(
        "上下文超限，自动压缩开始 · {count} 条消息 · {before} 字符"
    ));
    store::append(raw, &start)?;
    super::emit_event(
        app,
        "session.compact.started",
        json!({ "sessionID": session_id, "messages": count, "before": before, "auto": true }),
    );
    match run(app, session_id, raw, provider_cfg, model, "", start, slot) {
        Ok(done) => {
            super::emit_event(
                app,
                "session.compact.done",
                json!({
                    "sessionID": session_id,
                    "messages": done.messages,
                    "before": done.before,
                    "after": done.after,
                    "tokens": done.tokens,
                    "auto": true,
                }),
            );
            Ok(Rescue::Done)
        }
        Err(Failure::Cancelled) => {
            let _ = store::append(raw, &marker("自动压缩已取消 · 历史原样保留"));
            Ok(Rescue::Cancelled)
        }
        Err(Failure::Failed(error)) => {
            let _ = store::append(raw, &marker(&format!("自动压缩失败：{error}")));
            Err(error)
        }
    }
}

/// 入口：`/compact [额外要求]`。
///
/// 立刻返回（摘要要跑几十秒，IPC 不该一直挂着）；过程与结果走事件。
pub fn compact(app: &AppHandle, session_id: &str, instruction: &str) -> Result<String, String> {
    let raw = super::raw_session_id(session_id)
        .ok_or_else(|| format!("不是原生会话：{session_id}"))?
        .to_string();
    let meta = store::load_meta(&raw).ok_or_else(|| "原生会话不存在".to_string())?;
    let history = store::messages(&raw);
    if history.is_empty() {
        return Err("这个会话还没有内容，不用压缩".to_string());
    }
    let before: usize = history.iter().map(size_of).sum();
    if before < MIN_CHARS {
        return Ok(format!(
            "这个会话才 {before} 字符，压缩没有意义（原样保留）"
        ));
    }
    let cfg = config::load();
    let (provider_cfg, model) = agent::resolve_pick(
        &cfg,
        meta.model_provider.as_deref(),
        meta.model.as_deref(),
    )?;

    // 占住回合槽位：压缩期间不许再跑别的，「停止」也就能打断它（同一个 cancel 标记）。
    let slot = agent::RunSlot::take(session_id)?;
    super::emit_event(
        app,
        "session.execution.started",
        json!({ "sessionID": session_id }),
    );

    // 开始这条立刻落库：会话里得看得见「它开始了」，而不是只在输入框上转个圈。
    // 中途崩了也留得下这一行——那本来就是发生过的事。
    let count = history.len();
    let start = marker(&format!("会话压缩已开始 · {count} 条消息 · {before} 字符"));
    store::append(&raw, &start)?;
    super::emit_event(
        app,
        "session.compact.started",
        json!({ "sessionID": session_id, "messages": count, "before": before }),
    );

    let worker_app = app.clone();
    let worker_session = session_id.to_string();
    let worker_raw = raw;
    let instruction: String = instruction.trim().chars().take(INSTRUCTION_CLIP).collect();
    std::thread::spawn(move || {
        match run(
            &worker_app,
            &worker_session,
            &worker_raw,
            &provider_cfg,
            &model,
            &instruction,
            start,
            &slot,
        ) {
            Ok(done) => {
                super::emit_event(
                    &worker_app,
                    "session.compact.done",
                    json!({
                        "sessionID": worker_session,
                        "messages": done.messages,
                        "before": done.before,
                        "after": done.after,
                        "tokens": done.tokens,
                    }),
                );
                super::emit_event(
                    &worker_app,
                    "session.execution.succeeded",
                    json!({ "sessionID": worker_session }),
                );
            }
            Err(Failure::Cancelled) => {
                // 历史没动过：只要补一行「已取消」，别让开始那一行悬在那儿没有下文。
                let _ = store::append(
                    &worker_raw,
                    &marker("会话压缩已取消 · 历史原样保留"),
                );
                super::emit_event(
                    &worker_app,
                    "session.execution.interrupted",
                    json!({ "sessionID": worker_session }),
                );
            }
            Err(Failure::Failed(error)) => {
                let _ = store::append(
                    &worker_raw,
                    &marker(&format!("会话压缩失败：{error}")),
                );
                super::emit_event(
                    &worker_app,
                    "session.execution.failed",
                    json!({ "sessionID": worker_session, "error": error }),
                );
            }
        }
    });

    Ok(format!("正在压缩：{count} 条消息 · {before} 字符"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: &str, text: &str) -> store::StoredMessage {
        store::StoredMessage {
            id: format!("m-{role}"),
            role: role.to_string(),
            time: 0,
            text: text.to_string(),
            parts: vec![store::StoredPart::Text {
                text: text.to_string(),
            }],
            tool_calls: Vec::new(),
            tool_call_id: None,
            tokens: None,
        }
    }

    #[test]
    fn renders_each_role_with_its_own_shape() {
        let user = message("user", "改一下 README");
        assert_eq!(render(&user).unwrap(), "【用户】改一下 README");

        let mut assistant = message("assistant", "先读一遍");
        assistant.tool_calls = vec![store::StoredToolCall {
            id: "c1".to_string(),
            name: "read_file".to_string(),
            arguments: "{\"path\":\"README.md\"}".to_string(),
        }];
        let text = render(&assistant).unwrap();
        assert!(text.starts_with("【助手】先读一遍"), "{text}");
        assert!(text.contains("【调用 read_file】"), "{text}");

        let tool = message("tool", "第一行\n第二行");
        assert!(render(&tool).unwrap().starts_with("【工具结果】"));

        // 上一次的摘要要能被再压一次，否则连续压缩会断片。
        let compact = message("compact", "## 目标\n改 README");
        assert!(render(&compact).unwrap().starts_with("【更早的摘要】"));

        // 不认识的 role 不进摘要（system 之类不该混进「对话」里）。
        assert!(render(&message("system", "噪音")).is_none());
    }

    #[test]
    fn unanswered_user_message_survives_compaction() {
        let answered = vec![message("user", "问"), message("assistant", "答")];
        assert!(pending_user(&answered).is_none(), "答完的不用留");

        let pending = vec![message("user", "问"), message("user", "追问（上一轮失败了）")];
        let kept = pending_user(&pending).expect("该留最后那条");
        assert_eq!(kept.text, "追问（上一轮失败了）");

        assert!(pending_user(&[]).is_none());
    }

    #[test]
    fn long_tool_output_is_clipped() {
        let body = "行\n".repeat(2_000);
        let text = render(&message("tool", &body)).unwrap();
        assert!(text.chars().count() < TOOL_CLIP + 60, "{}", text.chars().count());
        assert!(text.contains("略"), "截断要说清是被截断的");
    }

    #[test]
    fn chunks_fill_a_budget_then_split() {
        // 每条占 3/4 预算：两条就装不下，正好一条一段。
        let lines: Vec<String> = (0..5).map(|_| "字".repeat(CHUNK_CHARS * 3 / 4)).collect();
        let (chunks, dropped) = chunks_of(&lines);
        assert_eq!(dropped, 0);
        assert_eq!(chunks.len(), 5);
        for (index, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.len(), 1, "第 {index} 段");
        }

        // 半预算一条：两条刚好填满一段，第三段留给剩下那条。
        let halves: Vec<String> = (0..5).map(|_| "字".repeat(CHUNK_CHARS / 2)).collect();
        let (chunks, _) = chunks_of(&halves);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], vec![0, 1]);
        assert_eq!(chunks[2], vec![4]);
    }

    #[test]
    fn oversized_chunks_keep_the_recent_ones_and_report_dropped() {
        let lines: Vec<String> = (0..(MAX_CHUNKS + 3))
            .map(|index| format!("{}", "字".repeat(CHUNK_CHARS)) + &index.to_string())
            .collect();
        let (chunks, dropped) = chunks_of(&lines);
        assert_eq!(chunks.len(), MAX_CHUNKS, "只留最近几段");
        assert_eq!(dropped, 3, "被丢掉的行数要如实报出来");
        // 留下的必须是最新的，而不是最早的。
        assert_eq!(chunks.last().unwrap().last().copied(), Some(lines.len() - 1));
    }

    #[test]
    fn prompt_merges_previous_summary_and_keeps_instruction() {
        let first = prompt_body(None, "【用户】你好", "");
        assert!(first.starts_with("把下面这段会话历史压成摘要"));
        assert!(!first.contains("已有摘要"));

        let next = prompt_body(Some("## 目标\n改 README"), "【用户】继续", "只保留 API 契约");
        assert!(next.contains("已有摘要"));
        assert!(next.contains("改 README"));
        assert!(next.contains("【用户】继续"));
        assert!(next.contains("只保留 API 契约"));
    }
}
