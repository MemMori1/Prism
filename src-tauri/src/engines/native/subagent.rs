//! 子代理：一个「隔离上下文的子任务执行器」。
//!
//! 主代理把大范围探索类的活交给它：子代理带着**自己的消息历史**与**受限工具集**
//! 独立跑，翻多少文件、搜多少关键词都塞不进主对话；跑完只把一份最终报告交回
//! 主上下文。这就是它的全部意义——上下文隔离，报告是唯一交付物。
//!
//! 编排刻意是扁平的：子代理的工具清单里没有 `task`，它开不出孙代理。

use super::{approval, config, provider, tools};
use crate::engines::native::emit_event;
use serde::Serialize;
use serde_json::{json, Value};
use tauri::AppHandle;

/// 子代理最多跑几轮（模型 → 工具 → 模型 算一轮）。
///
/// 探索类任务「grep 定位 → 定点读几处 → 汇总」轻松就过 6 轮，上限给到 16；
/// 真到顶也不会白跑：最后一轮不给工具，强收一份报告出来。
const MAX_TURNS: u32 = 16;
/// 子代理累计 token 的软预算：到顶就强收报告，防止一轮探索烧穿成本。
/// 轮次上限管「跑多久」，这个管「花多少」——两个都到才是真的失控。
const TOKEN_BUDGET: i64 = 200_000;
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

    /// 这类子代理的权限上限：探索只能拿只读工具，通用可写。
    ///
    /// 原来这里是一份按工具名硬编码的白名单（`&["read_file", ...]`），
    /// 加工具就得记得改它。改成权限档之后，工具集由工具注册表按
    /// `Permission` 推导——新加的只读工具会自动进入探索子代理的能力集，
    /// 写类工具则天然被挡在外面（默认更安全）。
    fn max_permission(self) -> tools::Permission {
        match self {
            AgentType::Explore => tools::Permission::ReadOnly,
            AgentType::General => tools::Permission::WriteWorkspace,
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
                 2. 命令要短、可复现；不要执行需要交互输入的命令。长命令用 run_command 的 background=true 转后台，再用 task_status 看结果。\n\
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

/// 子代理自己的工具清单：由工具注册表按权限档推导，`task` 天然落选
/// （它 `delegable: false`，扁平编排靠这个保证）。
/// 子代理的工具清单（测试用；生产路径由 `run` 直接按 `EnabledTools` 推导）。
#[cfg(test)]
pub fn specs_for_with(enabled: &tools::EnabledTools, agent: AgentType) -> Vec<Value> {
    tools::specs_for(enabled, agent.max_permission(), false)
}

/// `task` 工具的执行入口：从统一的 `ToolContext` 取出子代理需要的全部输入。
///
/// 这一层存在的意义是把"子代理"降级成一个普通工具——它有和其他工具
/// 相同的签名和返回类型，所以能进同一张注册表。原来它走的是分发处的
/// `call.name == "task"` 特判。
pub fn run_as_tool(ctx: &tools::ToolContext<'_>, args: &Value) -> tools::ToolResult {
    let Some(app) = ctx.app else {
        return tools::ToolResult::fail("子代理需要 Tauri 上下文");
    };
    let Some(provider) = ctx.provider else {
        return tools::ToolResult::fail("子代理缺少模型服务");
    };
    let Some(model) = ctx.model else {
        return tools::ToolResult::fail("子代理缺少模型");
    };
    let outcome = run(
        SubagentContext {
            app,
            session_id: ctx.session_id,
            call_id: ctx.call_id,
            directory: ctx.directory,
        },
        SubagentRequest {
            provider,
            model,
            input: args,
            permission: approval::parse(ctx.permission),
            should_stop: ctx.should_stop,
        },
    );
    fold(outcome)
}

/// 把一次子代理的产出收成工具结果：报告转正 / 失败转红，token 折进消息。
///
/// 抽成纯函数是因为它正对「task 调用 → 子代理结果 → 回填」这条最该有测试
/// 的路径——`run_turns` 里嵌着的话，`provider::chat` 没法 mock，这条路就永远
/// 没有单测。这里只看 outcome，不碰网络。
pub fn fold(outcome: SubagentOutcome) -> tools::ToolResult {
    let tokens = if outcome.tokens_input == 0 && outcome.tokens_output == 0 {
        None
    } else {
        Some(crate::engines::native::store::StoredTokens {
            input: outcome.tokens_input,
            output: outcome.tokens_output,
        })
    };
    let result = match &outcome.error {
        Some(error) => tools::ToolOutcome::fail(format!(
            "{error}{}",
            if outcome.report.is_empty() {
                String::new()
            } else {
                format!("\n\n已产出的部分报告：\n{}", outcome.report)
            }
        )),
        None => tools::ToolOutcome::ok(outcome.report.clone()),
    };
    tools::ToolResult::with_parts(result, Some(outcome.metadata()), tokens)
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
    pub trace: SubagentTrace,
    /// (input, output)：子代理自己烧掉的 token，计入会话总量。
    pub tokens_input: i64,
    pub tokens_output: i64,
}

impl SubagentOutcome {
    /// 过程元数据序列化（挂进工具卡的 `metadata.subagent`）。
    pub fn metadata(&self) -> Value {
        json!({ "subagent": self.trace })
    }
}

/// 一条过程步骤：文本片段或一次工具调用。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TraceStep {
    /// 子代理的一段过程说明（截断过的正文片段）。
    Text { text: String },
    /// 一次工具调用（只留摘要，输出不算交付物）。
    Tool {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        status: String,
    },
}

/// 子代理的过程轨迹：类型 + 轮数 + 是否被预算/轮次强收 + 步骤列表。
///
/// 强类型替代裸 `Value`，前后端契约有约束可循；将来加耗时、加 diff 只需加字段。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentTrace {
    pub agent_type: &'static str,
    pub label: &'static str,
    pub turns: u32,
    /// 报告是「轮次用尽 / token 预算到顶」被强收出来的，而非自然收尾。
    #[serde(default, skip_serializing_if = "is_false")]
    pub forced: bool,
    pub steps: Vec<TraceStep>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl Default for SubagentTrace {
    fn default() -> Self {
        SubagentTrace {
            agent_type: "explore",
            label: "探索",
            turns: 0,
            forced: false,
            steps: Vec::new(),
        }
    }
}

fn truncate(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    let mut out: String = trimmed.chars().take(limit).collect();
    if trimmed.chars().count() > limit {
        out.push('…');
    }
    out
}

/// 一条文本步骤（截断过的过程说明）。
fn text_step(text: &str) -> TraceStep {
    TraceStep::Text {
        text: truncate(text, STEP_TEXT_CHARS),
    }
}

/// 一条工具步骤（只留摘要）。
fn tool_step(name: &str, summary: Option<String>, status: &str) -> TraceStep {
    TraceStep::Tool {
        name: name.to_string(),
        summary,
        status: status.to_string(),
    }
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

/// 子代理运行所依附的父级上下文：事件通道、会话、工具卡定位。
pub struct SubagentContext<'a> {
    pub app: &'a AppHandle,
    pub session_id: &'a str,
    /// 父级工具调用的 id：进度事件与权限卡片都挂它名下。
    pub call_id: &'a str,
    pub directory: &'a str,
}

/// 一次子代理运行的全部输入。
pub struct SubagentRequest<'a> {
    pub provider: &'a config::ProviderConfig,
    pub model: &'a str,
    /// `task` 工具的原始入参（description / prompt / agent_type）。
    pub input: &'a Value,
    pub permission: approval::Mode,
    /// 沿用父级回合的停止标记：用户点「停止」时父子一起收手。
    pub should_stop: &'a dyn Fn() -> bool,
}

/// 跑一个子代理。
///
/// `ctx.should_stop` 直接沿用父级回合的停止标记；子代理自己的写类工具
/// 照样过权限闸门（explore 没有写工具，天然不触发）。
pub fn run(ctx: SubagentContext, req: SubagentRequest) -> SubagentOutcome {
    let mut outcome = SubagentOutcome::default();
    let (agent, prompt) = match parse_request(req.input) {
        Ok(parsed) => parsed,
        Err(error) => {
            outcome.error = Some(error);
            outcome.trace = SubagentTrace {
                agent_type: agent_of(req.input).key(),
                label: agent_of(req.input).label(),
                ..Default::default()
            };
            return outcome;
        }
    };

    let enabled = tools::EnabledTools::from_config();
    let specs = tools::specs_for(&enabled, agent.max_permission(), false);
    let mut messages = vec![
        json!({ "role": "system", "content": agent.system_prompt(ctx.directory) }),
        json!({ "role": "user", "content": prompt }),
    ];
    let mut steps: Vec<TraceStep> = Vec::new();
    let mut turns_used = 0u32;

    // 过程推进就把当前轨迹推给前端（挂在父级工具卡上，随跑随显）。
    let push_progress = |steps: &Vec<TraceStep>, turns: u32, forced: bool| {
        let trace = SubagentTrace {
            agent_type: agent.key(),
            label: agent.label(),
            turns,
            forced,
            steps: steps.clone(),
        };
        emit_event(
            ctx.app,
            "session.tool.progress",
            json!({
                "sessionID": ctx.session_id,
                "id": ctx.call_id,
                "metadata": { "subagent": trace },
            }),
        );
    };

    let stop = req.should_stop;
    for turn in 0..MAX_TURNS {
        if stop() {
            outcome.error = Some("已被用户停止，子代理没有跑完".to_string());
            break;
        }
        // 预算到顶也强收：轮次上限管「跑多久」，token 预算管「花多少」。
        let over_budget = outcome.tokens_input + outcome.tokens_output >= TOKEN_BUDGET;
        // 最后一轮 / 预算到顶都不给工具：逼它把已有的信息写成报告，
        // 否则整趟调查的结果会随 report 为空一起丢掉。
        let final_turn = final_turn_is(turn);
        let force_report = final_turn || over_budget;
        if force_report {
            messages.push(json!({
                "role": "user",
                "content": final_turn_prompt(),
            }));
        }
        let turn_specs: &[Value] = turn_specs_for(&specs, force_report);
        let mut on_text = |_delta: &str| {};
        let mut on_reasoning = |_delta: &str| {};
        let chat = match provider::chat(
            req.provider,
            req.model,
            &messages,
            turn_specs,
            &mut on_text,
            &mut on_reasoning,
            stop,
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
        // 强收轮即使模型还硬发工具调用也不执行了：正文有什么就当报告交什么。
        if chat.tool_calls.is_empty() || force_report {
            outcome.report = chat.text.trim().to_string();
            if outcome.report.is_empty() {
                outcome.report = if force_report {
                    "（轮次用尽，子代理没能给出报告）".to_string()
                } else {
                    "（子代理没有给出报告）".to_string()
                };
                // 兜底文案不是真报告：标记成降级，工具卡标红，别让用户以为查到了东西。
                outcome.error = Some("子代理没能产出有效报告（模型未返回正文）".to_string());
            }
            // 报告被强收（轮次 / 预算），打上标记：主代理与用户都能看出它没跑完。
            if force_report && outcome.error.is_none() {
                outcome.trace.forced = true;
            }
            if !chat.text.trim().is_empty() {
                steps.push(text_step(&chat.text));
            }
            push_progress(&steps, turn + 1, outcome.trace.forced);
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
            if stop() {
                outcome.error = Some("已被用户停止，子代理没有跑完".to_string());
                break;
            }
            let arguments: Value = serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
            // 与主代理同一套权限闸门：explore 没有写工具，天然不触发。
            let denied = approval::action_of(&call.name).and_then(|action| {
                let verdict = approval::gate(
                    ctx.app,
                    ctx.session_id,
                    req.permission,
                    action,
                    approval::resources_of(&call.name, &arguments),
                    approval::message_of(&call.name),
                    ctx.call_id,
                    stop,
                );
                match verdict {
                    approval::Verdict::Allow => None,
                    approval::Verdict::Deny(reason) => Some(reason),
                }
            });
            let result = match denied {
                Some(reason) => tools::ToolOutcome::fail(reason),
                None => {
                    tools::execute(
                        &tools::ToolContext {
                            app: Some(ctx.app),
                            session_id: ctx.session_id,
                            directory: ctx.directory,
                            call_id: &call.id,
                            provider: Some(req.provider),
                            model: Some(req.model),
                            permission: Some(req.permission.as_str()),
                            enabled: &enabled,
                            dag: None,
                            should_stop: stop,
                        },
                        &call.name,
                        &call.arguments,
                    )
                    .outcome
                }
            };
            if steps.len() < MAX_STEPS {
                steps.push(tool_step(
                    &call.name,
                    summarize(&arguments),
                    if result.error.is_some() {
                        "error"
                    } else {
                        "completed"
                    },
                ));
            }
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call.id,
                "content": result.text,
            }));
        }
        push_progress(&steps, turn + 1, false);
        if outcome.error.is_some() {
            break;
        }
    }

    outcome.trace = SubagentTrace {
        agent_type: agent.key(),
        label: agent.label(),
        turns: turns_used,
        forced: outcome.trace.forced,
        steps,
    };
    outcome
}

/// 入参里认不出的类型按「探索」返回（错误路径兜底用）。
fn agent_of(input: &Value) -> AgentType {
    AgentType::parse(input.get("agent_type").and_then(Value::as_str))
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
        let explore = specs_for_with(&tools::EnabledTools::all(), AgentType::Explore);
        let names: Vec<&str> = explore
            .iter()
            .filter_map(|spec| spec["function"]["name"].as_str())
            .collect();
        // 只读工具自动进探索档：新加的 bin.* 样本分析工具也是只读，所以一起出现。
        assert_eq!(
            names,
            vec![
                "read_file",
                "list_files",
                "grep",
                "bin.meta.identify",
                "bin.hash.compute",
                "bin.strings.extract",
            ]
        );

        let general = specs_for_with(&tools::EnabledTools::all(), AgentType::General);
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
        match step {
            TraceStep::Text { text } => {
                assert!(text.chars().count() <= STEP_TEXT_CHARS + 1);
                assert!(text.ends_with('…'));
            }
            other => panic!("应为文本步骤：{other:?}"),
        }

        let tool = tool_step("grep", Some("needle".to_string()), "completed");
        match tool {
            TraceStep::Tool {
                name,
                summary,
                status,
            } => {
                assert_eq!(name, "grep");
                assert_eq!(summary.as_deref(), Some("needle"));
                assert_eq!(status, "completed");
            }
            other => panic!("应为工具步骤：{other:?}"),
        }
    }

    #[test]
    fn metadata_shape_carries_agent_type_and_steps() {
        let trace = SubagentTrace {
            agent_type: "general",
            label: "通用",
            turns: 2,
            forced: false,
            steps: vec![text_step("看一下"), tool_step("grep", None, "completed")],
        };
        let outcome = SubagentOutcome {
            report: String::new(),
            error: None,
            trace,
            tokens_input: 0,
            tokens_output: 0,
        };
        let meta = outcome.metadata();
        let sub = &meta["subagent"];
        assert_eq!(sub["agentType"], "general");
        assert_eq!(sub["label"], "通用");
        assert_eq!(sub["turns"], 2);
        assert_eq!(sub["steps"].as_array().unwrap().len(), 2);
        // 未强收时 forced 不序列化（默认 false，前端不读也不会误解）。
        assert!(sub.get("forced").is_none(), "{sub}");
    }

    #[test]
    fn forced_report_is_serialized() {
        let trace = SubagentTrace {
            agent_type: "explore",
            label: "探索",
            turns: 16,
            forced: true,
            steps: Vec::new(),
        };
        let outcome = SubagentOutcome {
            trace,
            ..Default::default()
        };
        let meta = outcome.metadata();
        assert_eq!(meta["subagent"]["forced"], true);
    }

    #[test]
    fn missing_prompt_fails_fast() {
        let error =
            parse_request(&json!({ "description": "空任务" })).expect_err("缺 prompt 要失败");
        assert!(error.contains("prompt"), "{error}");
        // 有 prompt 就能解析出类型（认不出的类型默认「探索」）。
        let (agent, prompt) =
            parse_request(&json!({ "prompt": " 查一下登录流程 ", "agent_type": "general" }))
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
        let specs = specs_for_with(&tools::EnabledTools::all(), AgentType::Explore);
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
