//! 子代理：一个「隔离上下文的子任务执行器」。
//!
//! 主代理把大范围探索类的活交给它：子代理带着**自己的消息历史**与**受限工具集**
//! 独立跑，翻多少文件、搜多少关键词都塞不进主对话；跑完只把一份最终报告交回
//! 主上下文。这就是它的全部意义——上下文隔离，报告是唯一交付物。
//!
//! 编排刻意是扁平的：子代理的工具清单里没有 `task`，它开不出孙代理。

use super::{approval, config, provider, tools};
use crate::engines::native::emit_event;
use serde_json::{json, Value};
use tauri::AppHandle;

/// 子代理最多跑几轮（模型 → 工具 → 模型 算一轮）。
///
/// 探索类任务「grep 定位 → 定点读几处 → 汇总」轻松就过 6 轮，上限给到 16；
/// 真到顶也不会白跑：最后一轮不给工具，强收一份报告出来。
const MAX_TURNS: u32 = 16;
/// 界面上保留的步骤上限（看个大概就够，报告才是交付物）。
const MAX_STEPS: usize = 30;
/// 步骤里文本片段的截断长度。
const STEP_TEXT_CHARS: usize = 800;

/// 子代理类型：决定它能用哪些工具、系统提示怎么写。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentType {
    /// 只读调查：找代码、读文档、汇总信息——最常用，也最安全。
    Explore,
    /// 通用执行：能改文件、跑命令，写动作照样走权限闸门。
    General,
}

impl AgentType {
    /// 认不出一律按「探索」：宁可少一分权限，也不默认开写。
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.unwrap_or("").trim().to_ascii_lowercase().as_str() {
            "general" | "full" | "code" => AgentType::General,
            _ => AgentType::Explore,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            AgentType::Explore => "explore",
            AgentType::General => "general",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AgentType::Explore => "探索",
            AgentType::General => "通用",
        }
    }

    /// 这类子代理能用的工具名（永远不含 `task`——扁平编排）。
    fn tool_names(self) -> &'static [&'static str] {
        match self {
            AgentType::Explore => &["read_file", "list_files", "grep"],
            AgentType::General => {
                &["read_file", "list_files", "grep", "write_file", "edit_file", "run_command"]
            }
        }
    }

    fn system_prompt(self, directory: &str) -> String {
        match self {
            AgentType::Explore => format!(
                "你是 Prism 的探索子代理，工作目录是 {directory}。\n\
                 职责：完成交办的调查任务，最后输出一份简明报告。\n\
                 规则：\n\
                 1. 只读调查：不修改任何文件、不跑命令。\n\
                 2. 报告要自足：结论先行，证据带「文件:行号」。\n\
                 3. 不要复述过程，只交结论与证据；查不到就明说查不到。"
            ),
            AgentType::General => format!(
                "你是 Prism 的执行子代理，工作目录是 {directory}。\n\
                 职责：完成交办的子任务，最后输出一份简明报告。\n\
                 规则：\n\
                 1. 改文件前先 read_file 读原文，edit_file 的 old_string 必须与原文逐字一致（含缩进）。\n\
                 2. 命令要短、可复现；不要执行需要交互输入的命令。\n\
                 3. 报告格式：先说结论，再列改动（文件与要点），最后列遗留问题（如有）。"
            ),
        }
    }
}

/// 模型看到的 `task` 工具声明（挂进主代理的工具清单）。
pub fn spec() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "task",
            "description": "启动一个子代理独立完成一个子任务（隔离上下文），返回它的最终报告。适合大范围探索 / 调研：子代理翻多少文件都塞不进主对话，只交一份结论回来。",
            "parameters": {
                "type": "object",
                "properties": {
                    "description": { "type": "string", "description": "3-6 个词的短描述，显示在过程卡片上" },
                    "prompt": { "type": "string", "description": "给子代理的完整任务指令：要查什么 / 做什么、报告要覆盖什么" },
                    "agent_type": { "type": "string", "enum": ["explore", "general"], "description": "explore=只读调研（默认）；general=可改文件、跑命令" }
                },
                "required": ["description", "prompt"]
            }
        }
    })
}

/// 子代理自己的工具清单：全量声明按白名单过滤，`task` 天然落选。
pub fn specs_for(agent: AgentType) -> Vec<Value> {
    let allowed = agent.tool_names();
    tools::specs()
        .into_iter()
        .filter(|spec| {
            spec["function"]["name"]
                .as_str()
                .map(|name| name != "task" && allowed.contains(&name))
                .unwrap_or(false)
        })
        .collect()
}

/// 最后一轮要不要收掉工具、逼模型交报告。
///
/// 独立成纯函数便于单测：轮次用尽时不喂工具声明，还要追加一条「别再调工具」
/// 的用户消息，让模型把手里已有的信息写成报告。
pub fn final_turn_is(ordinal: u32) -> bool {
    ordinal + 1 == MAX_TURNS
}

/// 某一轮要发给模型的工具清单（最后一轮为空）。
pub fn turn_specs_for<'a>(full: &'a [Value], final_turn: bool) -> &'a [Value] {
    if final_turn {
        &[]
    } else {
        full
    }
}

/// 最后一轮追加的那句「别再调工具、直接交报告」。
pub fn final_turn_prompt() -> &'static str {
    "轮次用尽。现在不要再调用任何工具，直接基于已经拿到的信息输出最终报告；\
     没查到的部分在报告里写明「未覆盖」，不要编造。"
}

/// 子代理跑完的交付物。
#[derive(Debug, Default)]
pub struct SubagentOutcome {
    /// 最终报告（子代理最后一条无工具调用的正文）。
    pub report: String,
    /// 失败 / 被停止时的原因。
    pub error: Option<String>,
    /// 过程元数据（含步骤轨迹），挂到父级工具卡上展示。
    pub metadata: Value,
    /// (input, output)：子代理自己烧掉的 token，计入会话总量。
    pub tokens_input: i64,
    pub tokens_output: i64,
}

fn truncate(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    let mut out: String = trimmed.chars().take(limit).collect();
    if trimmed.chars().count() > limit {
        out.push('…');
    }
    out
}

/// 一条过程步骤：文本片段或一次工具调用（只留摘要，输出不算交付物）。
fn text_step(text: &str) -> Value {
    json!({ "kind": "text", "text": truncate(text, STEP_TEXT_CHARS) })
}

fn tool_step(name: &str, summary: Option<String>, status: &str) -> Value {
    json!({
        "kind": "tool",
        "name": name,
        "summary": summary,
        "status": status,
    })
}

fn metadata_of(agent: AgentType, steps: &[Value], turns: u32) -> Value {
    json!({
        "subagent": {
            "agentType": agent.key(),
            "label": agent.label(),
            "turns": turns,
            "steps": steps,
        }
    })
}

/// 工具卡摘要（与 agent.rs 的 `summarize_input` 同一取值顺序）。
fn summarize(input: &Value) -> Option<String> {
    const KEYS: [&str; 6] = ["command", "path", "pattern", "query", "url", "description"];
    for key in KEYS {
        if let Some(value) = input.get(key).and_then(Value::as_str) {
            let first = value.lines().next().unwrap_or("").trim();
            if !first.is_empty() {
                return Some(truncate(first, 96));
            }
        }
    }
    None
}

/// 解析 `task` 入参：类型 + 任务指令。缺 prompt 是最常见的错，早失败早说清。
fn parse_request(input: &Value) -> Result<(AgentType, String), String> {
    let agent = AgentType::parse(input.get("agent_type").and_then(Value::as_str));
    let prompt = input
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "缺少 prompt：子代理需要一段完整的任务指令".to_string())?
        .to_string();
    Ok((agent, prompt))
}

/// 跑一个子代理。
///
/// `should_stop` 直接沿用父级回合的停止标记：用户点「停止」时父子一起收手。
#[allow(clippy::too_many_arguments)]
pub fn run(
    app: &AppHandle,
    session_id: &str,
    call_id: &str,
    directory: &str,
    provider: &config::ProviderConfig,
    model: &str,
    input: &Value,
    permission: approval::Mode,
    should_stop: &dyn Fn() -> bool,
) -> SubagentOutcome {
    let mut outcome = SubagentOutcome::default();
    let (agent, prompt) = match parse_request(input) {
        Ok(parsed) => parsed,
        Err(error) => {
            outcome.error = Some(error);
            outcome.metadata = metadata_of(AgentType::parse(None), &[], 0);
            return outcome;
        }
    };

    let specs = specs_for(agent);
    let mut messages = vec![
        json!({ "role": "system", "content": agent.system_prompt(directory) }),
        json!({ "role": "user", "content": prompt }),
    ];
    let mut steps: Vec<Value> = Vec::new();
    let mut turns_used = 0u32;

    // 过程推进就把当前轨迹推给前端（挂在父级工具卡上，随跑随显）。
    let push_progress = |steps: &Vec<Value>, turns: u32| {
        emit_event(
            app,
            "session.tool.progress",
            json!({
                "sessionID": session_id,
                "id": call_id,
                "metadata": metadata_of(agent, steps, turns),
            }),
        );
    };

    for turn in 0..MAX_TURNS {
        if should_stop() {
            outcome.error = Some("已被用户停止，子代理没有跑完".to_string());
            break;
        }
        // 最后一轮不给工具：轮次用尽时逼它把已有的信息写成报告，
        // 否则整趟调查的结果会随 outcome.report 为空一起丢掉。
        let final_turn = final_turn_is(turn);
        if final_turn {
            messages.push(json!({
                "role": "user",
                "content": final_turn_prompt(),
            }));
        }
        let turn_specs: &[Value] = turn_specs_for(&specs, final_turn);
        let mut on_text = |_delta: &str| {};
        let mut on_reasoning = |_delta: &str| {};
        let chat = match provider::chat(
            provider,
            model,
            &messages,
            turn_specs,
            &mut on_text,
            &mut on_reasoning,
            should_stop,
        ) {
            Ok(chat) => chat,
            Err(provider::ChatError::Cancelled) => {
                outcome.error = Some("已被用户停止，子代理没有跑完".to_string());
                break;
            }
            Err(error) => {
                outcome.error = Some(error.to_string());
                break;
            }
        };
        if let Some(usage) = chat.usage {
            outcome.tokens_input += usage.input;
            outcome.tokens_output += usage.output;
        }
        turns_used = turn + 1;

        // 没有工具调用＝正文就是最终报告，收工。
        // 最后一轮即使模型还硬发工具调用也不执行了：正文有什么就当报告交什么。
        if chat.tool_calls.is_empty() || final_turn {
            outcome.report = chat.text.trim().to_string();
            if outcome.report.is_empty() {
                outcome.report = if final_turn {
                    "（轮次用尽，子代理没能给出报告）".to_string()
                } else {
                    "（子代理没有给出报告）".to_string()
                };
                // 兜底文案不是真报告：标记成降级，工具卡标红，别让用户以为查到了东西。
                outcome.error = Some("子代理没能产出有效报告（模型未返回正文）".to_string());
            }
            if !chat.text.trim().is_empty() {
                steps.push(text_step(&chat.text));
            }
            push_progress(&steps, turn + 1);
            break;
        }

        // 带工具调用：正文只是一段过程说明，记进步骤后继续。
        if !chat.text.trim().is_empty() && steps.len() < MAX_STEPS {
            steps.push(text_step(&chat.text));
        }
        let mut assistant = json!({ "role": "assistant", "content": chat.text });
        assistant["tool_calls"] = Value::Array(
            chat.tool_calls
                .iter()
                .map(|call| {
                    json!({
                        "id": call.id,
                        "type": "function",
                        "function": { "name": call.name, "arguments": call.arguments }
                    })
                })
                .collect(),
        );
        messages.push(assistant);

        for call in &chat.tool_calls {
            if should_stop() {
                outcome.error = Some("已被用户停止，子代理没有跑完".to_string());
                break;
            }
            let arguments: Value =
                serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
            // 与主代理同一套权限闸门：explore 没有写工具，天然不触发。
            let denied = approval::action_of(&call.name).and_then(|action| {
                let verdict = approval::gate(
                    app,
                    session_id,
                    permission,
                    action,
                    approval::resources_of(&call.name, &arguments),
                    approval::message_of(&call.name),
                    call_id,
                    should_stop,
                );
                match verdict {
                    approval::Verdict::Allow => None,
                    approval::Verdict::Deny(reason) => Some(reason),
                }
            });
            let result = match denied {
                Some(reason) => tools::ToolOutcome::fail(reason),
                None => tools::execute_with_stop(directory, &call.name, &call.arguments, should_stop),
            };
            if steps.len() < MAX_STEPS {
                steps.push(tool_step(
                    &call.name,
                    summarize(&arguments),
                    if result.error.is_some() { "error" } else { "completed" },
                ));
            }
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call.id,
                "content": result.text,
            }));
        }
        push_progress(&steps, turn + 1);
        if outcome.error.is_some() {
            break;
        }
    }

    outcome.metadata = metadata_of(agent, &steps, turns_used);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_type_defaults_to_explore() {
        assert_eq!(AgentType::parse(None), AgentType::Explore);
        assert_eq!(AgentType::parse(Some("")), AgentType::Explore);
        assert_eq!(AgentType::parse(Some("EXPLORE")), AgentType::Explore);
        assert_eq!(AgentType::parse(Some("什么鬼")), AgentType::Explore);
        assert_eq!(AgentType::parse(Some("general")), AgentType::General);
        assert_eq!(AgentType::parse(Some("General")), AgentType::General);
        assert_eq!(AgentType::parse(Some("full")), AgentType::General);
    }

    #[test]
    fn explore_cannot_write_and_nobody_can_spawn_grandchildren() {
        let explore = specs_for(AgentType::Explore);
        let names: Vec<&str> = explore
            .iter()
            .filter_map(|spec| spec["function"]["name"].as_str())
            .collect();
        assert_eq!(names, vec!["read_file", "list_files", "grep"]);

        let general = specs_for(AgentType::General);
        let names: Vec<&str> = general
            .iter()
            .filter_map(|spec| spec["function"]["name"].as_str())
            .collect();
        assert!(names.contains(&"write_file"), "{names:?}");
        assert!(names.contains(&"run_command"), "{names:?}");
        assert!(!names.contains(&"task"), "子代理不能再开子代理：{names:?}");
    }

    #[test]
    fn task_spec_is_recognizable() {
        let spec = spec();
        assert_eq!(spec["function"]["name"], "task");
        let required = spec["function"]["parameters"]["required"]
            .as_array()
            .expect("required 列表");
        assert!(required.iter().any(|item| item == "prompt"));
        assert!(required.iter().any(|item| item == "description"));
    }

    #[test]
    fn steps_are_truncated_not_dumped() {
        let long = "字".repeat(STEP_TEXT_CHARS + 50);
        let step = text_step(&long);
        let text = step["text"].as_str().unwrap();
        assert!(text.chars().count() <= STEP_TEXT_CHARS + 1);
        assert!(text.ends_with('…'));

        let tool = tool_step("grep", Some("needle".to_string()), "completed");
        assert_eq!(tool["kind"], "tool");
        assert_eq!(tool["status"], "completed");
    }

    #[test]
    fn metadata_shape_carries_agent_type_and_steps() {
        let meta = metadata_of(
            AgentType::General,
            &[text_step("看一下"), tool_step("grep", None, "completed")],
            2,
        );
        let sub = &meta["subagent"];
        assert_eq!(sub["agentType"], "general");
        assert_eq!(sub["label"], "通用");
        assert_eq!(sub["turns"], 2);
        assert_eq!(sub["steps"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn missing_prompt_fails_fast() {
        let error = parse_request(&json!({ "description": "空任务" })).expect_err("缺 prompt 要失败");
        assert!(error.contains("prompt"), "{error}");
        // 有 prompt 就能解析出类型（认不出的类型默认「探索」）。
        let (agent, prompt) = parse_request(&json!({ "prompt": " 查一下登录流程 ", "agent_type": "general" }))
            .expect("合法入参");
        assert_eq!(agent, AgentType::General);
        assert_eq!(prompt, "查一下登录流程");
    }

    #[test]
    fn final_turn_is_only_true_on_last_round() {
        assert!(!final_turn_is(0));
        assert!(!final_turn_is(MAX_TURNS - 2));
        assert!(final_turn_is(MAX_TURNS - 1));
    }

    #[test]
    fn final_turn_strips_tools_and_appends_a_wrap_up_prompt() {
        let specs = specs_for(AgentType::Explore);
        assert!(!specs.is_empty(), "探索子代理至少该有读类工具");

        // 非末轮：工具原样发。
        let before = turn_specs_for(&specs, false);
        assert_eq!(before.len(), specs.len());

        // 末轮：工具清空，逼模型交报告。
        let after = turn_specs_for(&specs, true);
        assert!(after.is_empty(), "末轮不能再发工具声明");

        let prompt = final_turn_prompt();
        assert!(prompt.contains("不要再调用任何工具"), "{prompt}");
        assert!(prompt.contains("未覆盖"), "{prompt}");
    }
}
