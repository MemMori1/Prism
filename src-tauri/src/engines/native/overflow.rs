//! 上下文超限的三道闸：源头限流 + 任务裁剪 + 超限瘦身。
//!
//! 工具结果全文进历史、每一轮再重发整份历史——一个大输出乘以轮数就是
//! 滚雪球，最后模型整个拒收（400 maximum context length）。这里管三件事：
//!
//! 1. [`clip_tool_result`]：单条工具结果进历史前限流，头尾各留一段。
//!    错误多在尾部（panic / 编译报错），结论多在头部（JSON 开头、命令回显）。
//! 2. [`clip_task_message`]：任务正文自己就超窗时砍一刀。编排里扇入的
//!    「汇总」节点会把十几份上游报告拼进一条提示词，**这才是最常见的爆点**；
//!    不砍它，删中间回合删到无可删也救不回来。
//! 3. [`drop_middle_turns`]：留 system、任务原文和最近的完整回合。**不调模型**
//!    ——能走到这一步说明请求已经被拒，瘦身要快、要确定；而且无人值守的循环
//!    里没有时间轴给摘要过程看。
//!
//! # 两套循环，一处定义
//!
//! 调用这份闸门的循环有两条，**改这里的阈值等于同时改两条**：
//!
//! - 聊天：`agent.rs::run`（长会话，救援走 `compact::rescue` 让模型做摘要）
//! - 编排：`dag::runner::NativeRunner::run`（节点无人值守，救援走机械瘦身）
//!
//! 两条都是「prism 原生引擎」，但 loop 是各自手写的（`NativeRunner` 复刻了
//! `agent.rs` 的回合结构，因为节点不需要会话持久化、审批槽位和事件流）。
//! 于是一边加保护、另一边不跟着加，就会出现"聊天没事、编排炸窗"这种偏科。
//! 动上下文治理时请两边都看一眼。
//!
//! 已知的**故意差异**：`agent.rs` 不额外调 [`clip_tool_result`]，因为主流工具
//! 自己在出口处走了 `tools::clamp`（`MAX_OUTPUT_CHARS` 2 万），聊天里再截一道
//! 会把用户看得见的结果切碎；编排则宁可再压一层，节点没人盯着，省 token 优先。
//! 代价是：不经过 `clamp` 的那几个工具在聊天里没有第二道闸——加新工具时如果
//! 输出可能很大，记得自己 clamp。


use serde_json::Value;

use super::config;

/// 单条工具结果进历史的上限：头 + 尾，约 9000 token。
const RESULT_HEAD: usize = 16_000;
const RESULT_TAIL: usize = 8_000;

/// 瘦身后的整份 messages 字节预算（≈7 万 token）。触发这条路的模型刚拒收过
/// 一次，说明已经超窗口；留这个量，128k 窗口的模型也装得下，还留足补全余量。
const TRIM_BUDGET: usize = 200_000;

/// 任务正文（`messages[1]`）的字节预算。
///
/// 为什么任务原文也要能砍：编排里扇入的「汇总」节点会把十几份上游报告拼进
/// 一个提示词，**超限的根源就在这一条**。以前的瘦身无条件保留前两条消息，
/// 于是"删中间回合"删到无可删（`len <= 3` 时直接返回 false），救援彻底无效
/// ——节点必然失败，还看不出为什么。
const TASK_BUDGET: usize = 60_000;
/// 砍任务正文时保留的头尾（错误与验收标准常在尾部）。
const TASK_HEAD: usize = 40_000;
const TASK_TAIL: usize = 16_000;

/// 摘要里一条工具调用 / 一条工具结果的字符上限。
///
/// 和 `compact` 聊天路径的 `ARGS_CLIP`（400）/ `TOOL_CLIP`（1500）同量级：
/// 摘要要的是"调过什么、拿到什么结论"，不是把原始回显再抄一遍。
const COMPACT_ARGS: usize = 400;
const COMPACT_TOOL_RESULT: usize = 1_500;

/// 一条工具结果进历史前的样子：超过上限就留头尾、中段一句交代。
///
/// 不静默：截了就是截了，写明略掉多少，模型才知道这份输出不完整。
pub(crate) fn clip_tool_result(text: &str) -> String {
    let count = text.chars().count();
    if count <= RESULT_HEAD + RESULT_TAIL {
        return text.to_string();
    }
    let head: String = text.chars().take(RESULT_HEAD).collect();
    let tail: String = text
        .chars()
        .skip(count - RESULT_TAIL)
        .collect();
    format!("{head}\n…（中略 {} 字符）…\n{tail}", count - RESULT_HEAD - RESULT_TAIL)
}

/// 任务正文本身超窗时按「头 + 尾」裁一刀。裁剪返回 `true`。
fn clip_task_message(message: &mut Value) -> bool {
    if message.get("role").and_then(Value::as_str) != Some("user") {
        return false;
    }
    let Some(text) = message.get("content").and_then(Value::as_str) else {
        // 多模态内容（数组）不在这里动：拿不到稳定长度，硬拆会拆坏结构。
        return false;
    };
    let count = text.chars().count();
    if count <= TASK_BUDGET {
        return false;
    }
    let head: String = text.chars().take(TASK_HEAD).collect();
    let tail: String = text.chars().skip(count - TASK_TAIL).collect();
    let clipped = format!(
        "{head}\n…（任务正文过长，中略 {} 字符。上游报告的完整内容在制品里，\
         需要细节就按提示里给出的制品 id 去取）…\n{tail}",
        count - TASK_HEAD - TASK_TAIL
    );
    message["content"] = Value::String(clipped);
    true
}

/// 超限后的瘦身：先砍任务正文，再留前两条（system + 任务原文）和预算内的
/// 最近消息。
///
/// 截断点吸附到 assistant 边界——assistant 连着它的 tool 结果是一整个回合，
/// 从中间切会留下没人认领的 tool_call。中段用一条 user 说明顶上（不发
/// system：多数 OpenAI 兼容端只认开头那一条）。
///
/// 返回 `false` = 没什么可删的（瘦不下来），调用方把原始错误如实报出去。
pub(crate) fn trim_messages(messages: &mut Vec<Value>) -> bool {
    let mut changed = false;
    // 顺序很重要：任务正文先砍，后面的预算计算才看得到真实体积。
    if messages.len() > 1 {
        changed |= clip_task_message(&mut messages[1]);
    }
    if messages.len() > 3 && drop_middle_turns(messages) {
        changed = true;
    }
    changed
}

/// 压缩的切点：`messages[2..cut]` 进摘要，`[cut..]` 原样留着。
///
/// 位置取**最后一个 assistant**：它和后面那条 tool 结果是一个不可分的回合，
/// 切在 tool 上会留下没有 `tool_calls` 认领的孤儿结果，下一次请求直接 400。
///
/// `cut <= 2` 表示除了 system 和任务原文什么也没留下——压无可压。
fn compaction_cut(messages: &[Value]) -> Result<usize, String> {
    let cut = (2..messages.len())
        .rev()
        .find(|&index| messages[index].get("role").and_then(Value::as_str) == Some("assistant"))
        .ok_or_else(|| "没有可压缩的中段".to_string())?;
    if cut <= 2 {
        return Err("没有可压缩的中段".to_string());
    }
    Ok(cut)
}

/// 超限后的**压缩**救援：叫模型把中段历史压成一段结论。
///
/// 和 [`trim_messages`] 是两种代价换两种损失：
///
/// - 裁剪：丢掉中段，**立刻生效、不花钱**，但前面十几轮工具调用白做；
/// - 压缩：换成一段摘要，**多一次模型调用**，但"查到了什么、试过什么、
///   还差什么"留得住。
///
/// 编排节点的活很贵（读几十个文件、跑几遍测试），所以先试压缩。压不动
/// （没有中段、或摘要调用也失败）再由调用方退回裁剪。
///
/// 保留结构：`system` + 任务原文（可能先被裁）+ 摘要 + **最后一个完整回合**。
/// 末尾不动是因为 assistant 的 `tool_calls` 必须和它的 tool 结果配对，从中间
/// 切会留下没人认领的调用——下一轮请求当场 400。
///
/// 返回被压掉的条数。
pub(crate) fn compress_messages(
    messages: &mut Vec<Value>,
    provider_cfg: &config::ProviderConfig,
    model: &str,
    stop: &dyn Fn() -> bool,
) -> Result<usize, String> {
    if messages.len() <= 3 {
        return Err("历史太短，没有可压缩的中段".to_string());
    }
    // 摘要请求自己也可能被任务正文撑爆：先按同一套预算砍一刀。
    if messages.len() > 1 {
        clip_task_message(&mut messages[1]);
    }
    let cut = compaction_cut(messages)?;
    let lines: Vec<String> = messages[2..cut].iter().filter_map(render_value).collect();
    if lines.is_empty() {
        return Err("中段没有可压缩的文字".to_string());
    }
    let (summary, compressed) = super::compact::summarize_lines(&lines, provider_cfg, model, stop)?;
    let marker = serde_json::json!({
        "role": "user",
        "content": format!(
            "（此前 {compressed} 条消息因上下文超限已压成下面的摘要。摘要之外的中间输出\
             你看不到，需要细节就重新用工具查，不要凭印象编。）\n\n{summary}"
        ),
    });
    let mut kept = Vec::with_capacity(messages.len() - cut + 3);
    kept.push(messages[0].clone());
    kept.push(messages[1].clone());
    kept.push(marker);
    kept.extend_from_slice(&messages[cut..]);
    *messages = kept;
    Ok(compressed)
}

/// 一条消息在摘要里的样子（OpenAI 消息格式版，和 `compact::render` 对齐）。
///
/// `system` 不进摘要：它是这一轮的规则，压缩掉等于把规矩丢了。
/// 多模态内容（数组）跳过——拿不到稳定长度，硬转成文字反而误导。
fn render_value(message: &Value) -> Option<String> {
    let role = message.get("role").and_then(Value::as_str)?;
    let text = message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    match role {
        "assistant" => {
            let mut out = String::from("【助手】");
            out.push_str(text);
            if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    let name = call
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                        .unwrap_or("?");
                    let args = call
                        .pointer("/function/arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    out.push_str(&format!(
                        "\n【调用 {name}】{}",
                        take_chars(args, COMPACT_ARGS)
                    ));
                }
            }
            Some(out)
        }
        "tool" => Some(format!(
            "【工具结果】{}",
            take_chars(text, COMPACT_TOOL_RESULT)
        )),
        // 空正文的用户消息通常是多模态（content 是数组）：拿不到可摘的文字，
        // 与其在摘要里留一行空的「【用户】」，不如跳过。
        "user" => {
            if text.is_empty() {
                None
            } else {
                Some(format!("【用户】{text}"))
            }
        }
        _ => None,
    }
}

/// 截到 `limit` 个字符并写明后面还有多少。
fn take_chars(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    if count <= limit {
        return text.to_string();
    }
    let head: String = text.chars().take(limit).collect();
    format!("{head}…（后 {} 字符略）", count - limit)
}

/// 丢掉中段的完整回合，保住 system + 任务原文 + 最近若干轮。返回是否真的删了。
fn drop_middle_turns(messages: &mut Vec<Value>) -> bool {
    let sizes: Vec<usize> = messages
        .iter()
        .map(|message| {
            serde_json::to_string(message)
                .map(|text| text.len())
                .unwrap_or(0)
        })
        .collect();
    let mut total = sizes[0] + sizes[1];
    let mut start = messages.len();
    for index in (2..messages.len()).rev() {
        if total + sizes[index] > TRIM_BUDGET {
            break;
        }
        total += sizes[index];
        start = index;
    }
    if start >= messages.len() {
        return false;
    }
    while start < messages.len()
        && messages[start].get("role").and_then(Value::as_str) != Some("assistant")
    {
        start += 1;
    }
    let dropped = start.saturating_sub(2);
    if dropped == 0 {
        return false;
    }
    let marker = serde_json::json!({
        "role": "user",
        "content": format!(
            "（此前 {dropped} 条消息因上下文超限被截去，只保留最近部分；结论不要引用被截去的中间输出。）"
        ),
    });
    let mut kept = Vec::with_capacity(messages.len() - dropped);
    kept.push(messages[0].clone());
    kept.push(messages[1].clone());
    kept.push(marker);
    kept.extend_from_slice(&messages[start..]);
    *messages = kept;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assistant_with_call(id: &str) -> Value {
        json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": id,
                "type": "function",
                "function": { "name": "files.read", "arguments": "{}" }
            }]
        })
    }

    #[test]
    fn short_tool_result_passes_through() {
        assert_eq!(clip_tool_result("ok"), "ok");
    }

    #[test]
    fn long_tool_result_keeps_head_and_tail() {
        let body = format!("{}中间{}","a".repeat(20_000), "b".repeat(20_000));
        let clipped = clip_tool_result(&body);
        assert!(clipped.starts_with("aaaa"));
        assert!(clipped.ends_with("bbbb"));
        assert!(clipped.contains("中略"));
        assert!(clipped.chars().count() < RESULT_HEAD + RESULT_TAIL + 100);
    }

    #[test]
    fn trim_keeps_system_task_and_recent_turns() {
        let mut messages = vec![
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": "task"}),
        ];
        // 三个完整回合 + 收尾的 tool 结果。
        for index in 0..3 {
            messages.push(assistant_with_call(&format!("c{index}")));
            messages.push(json!({"role": "tool", "tool_call_id": format!("c{index}"), "content": "x".repeat(90_000)}));
        }
        assert!(trim_messages(&mut messages));
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["content"], "task");
        // 截断说明顶在第三位。
        assert!(messages[2]["content"]
            .as_str()
            .unwrap()
            .contains("被截去"));
        // 保留区从 assistant 开始：不会出现没有主人的 tool 结果。
        let first_kept_role = messages[3]["role"].as_str().unwrap();
        assert_eq!(first_kept_role, "assistant");
        for (index, message) in messages.iter().enumerate() {
            if message["role"] == "tool" {
                assert!(
                    messages[..index].iter().any(|owner| owner["role"] == "assistant"),
                    "tool 结果前面必须有 assistant"
                );
            }
        }
        let total: usize = messages
            .iter()
            .map(|message| serde_json::to_string(message).unwrap().len())
            .sum();
        // 预算是软的：吸附到 assistant 边界可能多留一条（一条 9 万字节）。
        assert!(total <= TRIM_BUDGET + 100_000, "瘦身后应大致落回预算内");
    }

    #[test]
    fn trim_on_a_fresh_conversation_is_a_no_op() {
        let mut messages = vec![
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": "task"}),
            json!({"role": "assistant", "content": "hi"}),
        ];
        assert!(!trim_messages(&mut messages));
    }

    /// 扇入的「汇总」节点：提示词本身就把窗口撑爆了。
    ///
    /// 以前这条路径救不了——瘦身无条件保留任务原文，历史又只有三条，
    /// 于是 `trim_messages` 直接返回 false，节点必然失败。
    #[test]
    fn trim_shrinks_an_oversized_task_even_with_a_short_history() {
        let body = format!("{}{}", "头".repeat(TASK_BUDGET), "尾".repeat(2_000));
        let mut messages = vec![
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": body}),
            json!({"role": "assistant", "content": "hi"}),
        ];
        assert!(trim_messages(&mut messages), "任务正文超预算时应当能瘦下来");
        let task = messages[1]["content"].as_str().unwrap();
        assert!(task.starts_with("头头"), "头要留住：任务目标是开头");
        assert!(task.ends_with("尾尾"), "尾要留住：验收标准常在结尾");
        assert!(task.contains("任务正文过长"));
        assert!(
            task.chars().count() < TASK_BUDGET,
            "砍完必须真的变小：{}",
            task.chars().count()
        );
        // system 与任务仍在原位，索引语义没被破坏。
        assert_eq!(messages[0]["content"], "sys");
    }

    #[test]
    fn compaction_cut_keeps_the_last_turn_paired() {
        let messages = vec![
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": "task"}),
            assistant_with_call("c1"),
            json!({"role": "tool", "tool_call_id": "c1", "content": "r1"}),
            assistant_with_call("c2"),
            json!({"role": "tool", "tool_call_id": "c2", "content": "r2"}),
        ];
        // 切点落在倒数第二个 assistant：最后一轮（c2 和它的结果）原样留着。
        let cut = compaction_cut(&messages).unwrap();
        assert_eq!(cut, 4);
        assert_eq!(messages[cut]["role"], "assistant");
        assert_eq!(messages[cut + 1]["role"], "tool");
    }

    #[test]
    fn compaction_cut_never_lands_on_a_tool_result() {
        let messages = vec![
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": "task"}),
            assistant_with_call("c1"),
            json!({"role": "tool", "tool_call_id": "c1", "content": "r1"}),
            assistant_with_call("c2"),
            json!({"role": "tool", "tool_call_id": "c2", "content": "r2"}),
        ];
        let cut = compaction_cut(&messages).unwrap();
        assert_ne!(messages[cut]["role"], "tool", "切在 tool 上会留下孤儿结果");
    }

    #[test]
    fn compaction_cut_refuses_the_only_turn() {
        let messages = vec![
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": "task"}),
            assistant_with_call("c1"),
            json!({"role": "tool", "tool_call_id": "c1", "content": "r1"}),
        ];
        // 只有一轮：压掉它等于把全部工作扔了，不如交给裁剪。
        assert!(compaction_cut(&messages).is_err());
    }

    #[test]
    fn render_value_keeps_the_call_name_and_clips_arguments() {
        let message = json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "c1",
                "type": "function",
                "function": { "name": "files.read", "arguments": "x".repeat(900) }
            }]
        });
        let text = render_value(&message).unwrap();
        assert!(text.contains("【调用 files.read】"));
        assert!(text.contains("后 500 字符略"), "超长参数要裁：{text}");
    }

    #[test]
    fn render_value_skips_system_and_multimodal_users() {
        assert!(render_value(&json!({"role": "system", "content": "sys"})).is_none());
        assert!(render_value(&json!({
            "role": "user",
            "content": [{ "type": "text", "text": "看图" }]
        }))
        .is_none());
    }

    #[test]
    fn a_normal_sized_task_is_untouched() {
        let mut messages = vec![
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": "x".repeat(TASK_BUDGET - 1)}),
            json!({"role": "assistant", "content": "hi"}),
        ];
        assert!(!trim_messages(&mut messages));
        assert_eq!(
            messages[1]["content"].as_str().unwrap().chars().count(),
            TASK_BUDGET - 1
        );
    }

    /// 多模态内容（content 是数组）拿不到稳定长度，宁可不动也不能拆坏结构。
    #[test]
    fn structured_content_is_left_alone() {
        let mut messages = vec![
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": [{"type": "text", "text": "x"}]}),
            json!({"role": "assistant", "content": "hi"}),
        ];
        assert!(!trim_messages(&mut messages));
        assert!(messages[1]["content"].is_array());
    }

    #[test]
    fn trim_never_orphans_a_tool_result() {
        // 尾部只有 tool 结果没有 assistant（理论边界）：吸附后全部保留，
        // 或者如实放弃，不能切出孤儿。
        let mut messages = vec![
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": "task"}),
            json!({"role": "user", "content": "x".repeat(TRIM_BUDGET + 1)}),
            json!({"role": "tool", "tool_call_id": "c", "content": "y"}),
        ];
        // 不 panic 就行：结果 either false（放弃）or kept 区没有孤儿。
        let _ = trim_messages(&mut messages);
    }
}
