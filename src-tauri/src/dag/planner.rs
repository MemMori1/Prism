//! LLM 规划器：一句目标 → 一张 DAG 草案。
//!
//! 流程（`docs/DAG_PLANNER.md` §7.2）：
//!
//! ```text
//! 提示词（目标 + 项目 + Agent 目录 + 严格 JSON schema + 规划规则）
//!        │
//!        ▼
//!   模型输出 JSON ──► 解析 ──► 校验
//!        ▲                      │ 有错
//!        └──── 把错误原文喂回去 ──┘（最多一次）
//! ```
//!
//! 两次都不成，就**报错而不再降级**：一句"降级成单节点计划"听起来友好，实际是
//! 把"模型没看懂"伪装成"计划就这样"，用户会在运行之后才发现跑歪了。
//!
//! 规划只产出**草案**：不落盘、不运行。落盘与运行由用户点确认后走 `dag_save`。

use serde_json::{json, Value};

use crate::agents::AgentProfile;

use super::model::{
    DagDefaults, DagNode, DagPlan, Gate, Permission, Retry, Workspace,
};
use super::validate::{self, Level};

/// 规划期默认节点上限：超过 8 个的图人 review 不动，模型也容易漏依赖。
pub const DEFAULT_MAX_NODES: usize = 8;

/// 单个 task 的字符上限：子 Agent 看不到别的节点上下文，prompt 太短不自足，
/// 太长又会挤爆下游的上下文预算。
const TASK_CHARS: usize = 2_000;

fn system_prompt(max_nodes: usize) -> String {
    format!(
        r#"你是 Prism 的 DAG 规划器。把用户目标拆成有依赖的任务图，每个节点交给一个 Agent 独立执行。

只输出一个 JSON 对象，不要解释，不要 markdown 围栏。形状：
{{
  "name": "计划名（不超过 20 字）",
  "nodes": [
    {{
      "id": "短英文 id，如 research / frontend / verify",
      "title": "动词开头的短语",
      "task": "自足的任务说明：做什么、边界、验收标准",
      "agent": "下面目录里的 agent id，必须存在",
      "dependsOn": ["上游节点 id"],
      "workspace": "shared | worktree | scratch",
      "gate": false
    }}
  ]
}}

规则：
1. 节点数 2 到 {max_nodes} 个；每个节点原子、可独立验证。
2. 只读调研的节点用 shared；会改文件的节点用 worktree。
3. 下游只依赖它真正需要的上游，不要串成一条长链；能并行的就并行。
4. 最后一个节点是验证/汇总，依赖所有写节点，gate 设为 true。
5. task 必须自足：子 Agent 看不到别的节点上下文。需要上游结论时用占位符
   {{{{nodes.<id>.output}}}}，且只能引用自己的上游（祖先）。
6. 不要生成"安装依赖"这类环境节点，除非目标里明确提到。
7. 绝不产生环。"#
    )
}

fn agent_catalog(agents: &[AgentProfile]) -> String {
    let mut out = String::new();
    for agent in agents {
        out.push_str(&format!(
            "- {} | {} | 执行器={} | 权限={} | 标签={}\n",
            agent.id,
            agent.name,
            agent.executor,
            agent.permission.as_str(),
            if agent.tags.is_empty() {
                "无".to_string()
            } else {
                agent.tags.join("/")
            }
        ));
        if !agent.description.is_empty() {
            out.push_str(&format!("    {}\n", agent.description));
        }
    }
    out
}

fn user_prompt(goal: &str, project: &str, catalog: &str) -> String {
    format!(
        "目标：{goal}\n项目目录：{project}\n\n可用的 Agent（agent 字段只能取这里的 id）：\n{catalog}"
    )
}

/// 从模型输出里捞出 JSON 对象。
///
/// 模型经常加 ```json 围栏或前后废话；这里取最外层的一对花括号，不再往
/// 更聪明的方向做（比如修尾逗号）——那种"宽容"会让真正的格式错误被吞掉。
pub fn extract_json(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return value.is_object().then_some(value);
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if end <= start {
        return None;
    }
    let slice = &trimmed[start..=end];
    serde_json::from_str::<Value>(slice)
        .ok()
        .filter(Value::is_object)
}

/// 把 id 收成能安全进占位符的形状（字母数字与连字符）。
fn slug(raw: &str, fallback: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        fallback.to_string()
    } else {
        out.chars().take(32).collect()
    }
}

fn text_of<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str).map(str::trim)
}

fn workspace_of(value: &Value) -> Option<Workspace> {
    match text_of(value, "workspace")? {
        "shared" => Some(Workspace::Shared),
        "worktree" => Some(Workspace::Worktree),
        "scratch" => Some(Workspace::Scratch),
        _ => None,
    }
}

/// 从模型给的 JSON 构造计划。**所有**外部输入在这里被收口：id 洗成 slug、
/// 未知 Agent 退回默认、越界字段截断。
pub(crate) fn from_json(
    value: &Value,
    goal: &str,
    project: &str,
    agents: &[AgentProfile],
    max_nodes: usize,
) -> Result<DagPlan, String> {
    let raw_nodes = value
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| "输出里没有 nodes 数组".to_string())?;
    if raw_nodes.is_empty() {
        return Err("nodes 是空的".to_string());
    }

    let known: Vec<String> = agents.iter().map(|agent| agent.id.clone()).collect();
    let fallback_agent = if known.iter().any(|id| id == "general") {
        "general".to_string()
    } else {
        known.first().cloned().unwrap_or_else(|| "general".to_string())
    };

    let mut nodes: Vec<DagNode> = Vec::new();
    for (index, raw) in raw_nodes.iter().take(max_nodes).enumerate() {
        let id = slug(text_of(raw, "id").unwrap_or(""), &format!("node-{}", index + 1));
        // 洗完之后可能撞名：加后缀而不是丢掉节点。
        let id = if nodes.iter().any(|node| node.id == id) {
            let mut counter = 2;
            loop {
                let candidate = format!("{id}-{counter}");
                if !nodes.iter().any(|node| node.id == candidate) {
                    break candidate;
                }
                counter += 1;
            }
        } else {
            id
        };

        let title = text_of(raw, "title")
            .filter(|text| !text.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("节点 {}", index + 1));
        let task: String = text_of(raw, "task")
            .unwrap_or("")
            .chars()
            .take(TASK_CHARS)
            .collect();
        if task.trim().is_empty() {
            return Err(format!("节点「{id}」没有 task"));
        }

        let agent = text_of(raw, "agent")
            .filter(|name| known.iter().any(|id| id == name))
            .map(str::to_string);

        let depends_on: Vec<String> = raw
            .get("dependsOn")
            .or_else(|| raw.get("depends_on"))
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(Value::as_str)
                    .map(|item| slug(item, ""))
                    .filter(|item| !item.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        let gate = raw
            .get("gate")
            .and_then(Value::as_bool)
            .filter(|flag| *flag)
            .map(|_| Gate {
                before: true,
                after: false,
            });

        nodes.push(DagNode {
            id,
            title,
            task,
            depends_on,
            agent,
            workspace: workspace_of(raw),
            tools: None,
            permission: None,
            retry: Retry::default(),
            timeout_secs: None,
            gate,
            position: None,
            input_artifacts: Vec::new(),
            output_artifact_key: String::new(),
        });
    }

    // 模型偶尔引用不存在的上游（包括被截断的节点）：删掉这条边比整张图作废好，
    // 但要在最后让校验器再扫一遍——被删边的节点可能变成"孤立读上游结论"的空转。
    let ids: Vec<String> = nodes.iter().map(|node| node.id.clone()).collect();
    for node in nodes.iter_mut() {
        node.depends_on.retain(|dep| ids.iter().any(|id| id == dep) && dep != &node.id);
        let mut seen: Vec<String> = Vec::new();
        node.depends_on.retain(|dep| {
            if seen.contains(dep) {
                return false;
            }
            seen.push(dep.clone());
            true
        });
    }

    let name = text_of(value, "name")
        .filter(|text| !text.is_empty())
        .map(|text| text.chars().take(40).collect::<String>())
        .unwrap_or_else(|| goal.chars().take(20).collect());

    Ok(DagPlan {
        version: super::model::VERSION,
        id: super::store::new_id(),
        name,
        goal: goal.to_string(),
        project: project.to_string(),
        defaults: DagDefaults {
            agent: fallback_agent,
            permission: Permission::WorkspaceWrite,
            max_parallel: 2,
            failure_policy: Default::default(),
            token_budget: None,
        },
        nodes,
        layout: Default::default(),
        created_at: 0,
        updated_at: 0,
    })
}

/// 空白草稿：手动搭图的起点。默认值刻意是可跑的（一个调研节点），
/// 而不是一张空图——空图连校验都过不去，用户第一眼就是红的。
pub fn blank(project: &str) -> DagPlan {
    let agent = "general".to_string();
    DagPlan {
        version: super::model::VERSION,
        id: super::store::new_id(),
        name: "未命名计划".to_string(),
        goal: String::new(),
        project: project.to_string(),
        defaults: DagDefaults {
            agent: agent.clone(),
            permission: Permission::WorkspaceWrite,
            max_parallel: 2,
            failure_policy: Default::default(),
            token_budget: None,
        },
        nodes: vec![DagNode {
            id: "research".to_string(),
            title: "调研现状".to_string(),
            task: "调研现状并输出结论，带「文件:行号」证据。".to_string(),
            depends_on: Vec::new(),
            agent: None,
            workspace: Some(Workspace::Shared),
            tools: None,
            permission: None,
            retry: Retry::default(),
            timeout_secs: None,
            gate: None,
            position: None,
            input_artifacts: Vec::new(),
            output_artifact_key: String::new(),
        }],
        layout: Default::default(),
        created_at: 0,
        updated_at: 0,
    }
}

/// 调一次模型（阻塞）。
fn ask(system: &str, prompt: &str) -> Result<String, String> {
    let config = crate::engines::native::config::load();
    let (provider, model) = config.selection().ok_or_else(|| {
        "没有可用的模型服务。Planner 要用原生引擎的服务来拆图——先去设置里填一个"
            .to_string()
    })?;
    let messages = vec![
        json!({ "role": "system", "content": system }),
        json!({ "role": "user", "content": prompt }),
    ];
    let outcome = crate::engines::native::provider::chat(
        provider,
        &model.id,
        &messages,
        &[],
        &mut |_delta| {},
        &mut |_delta| {},
        &|| false,
    )
    .map_err(|error| error.to_string())?;
    Ok(outcome.text)
}

/// 生成草案。失败时返回**人能看懂**的原因（含模型最后一次的输出片段）。
pub fn draft(
    goal: &str,
    project: &str,
    agents: &[AgentProfile],
    max_nodes: usize,
) -> Result<DagPlan, String> {
    let goal = goal.trim();
    if goal.is_empty() {
        return Err("先写一句目标：Planner 需要知道要达成什么".to_string());
    }
    if agents.is_empty() {
        return Err("Agent 注册表是空的，没有可指派的 Agent".to_string());
    }

    let max_nodes = max_nodes.clamp(2, validate::MAX_NODES);
    let system = system_prompt(max_nodes);
    let catalog = agent_catalog(agents);
    let base = user_prompt(goal, project, &catalog);
    let known: Vec<String> = agents.iter().map(|agent| agent.id.clone()).collect();

    let mut last_error = String::new();
    for attempt in 0..2 {
        let prompt = if attempt == 0 {
            base.clone()
        } else {
            format!(
                "{base}\n\n上一次的输出不能用：{last_error}\n只输出修正后的 JSON，不要解释。"
            )
        };
        let text = ask(&system, &prompt)?;
        let Some(value) = extract_json(&text) else {
            last_error = format!(
                "输出里找不到 JSON 对象（开头是：{}）",
                text.chars().take(120).collect::<String>()
            );
            continue;
        };
        let plan = match from_json(&value, goal, project, agents, max_nodes) {
            Ok(plan) => plan,
            Err(error) => {
                last_error = error;
                continue;
            }
        };
        let issues = validate::validate(&plan, &known);
        let errors: Vec<String> = issues
            .iter()
            .filter(|issue| issue.level == Level::Error)
            .map(|issue| issue.message.clone())
            .collect();
        if errors.is_empty() {
            return Ok(plan);
        }
        last_error = errors.join("；");
    }

    Err(format!("Planner 没能产出可用的计划：{last_error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agents() -> Vec<AgentProfile> {
        crate::agents::builtin()
    }

    #[test]
    fn extracts_json_from_chatty_output() {
        let text = "好的，这是计划：\n```json\n{\"name\":\"x\",\"nodes\":[]}\n```\n需要我调整吗？";
        let value = extract_json(text).expect("应当能捞出 JSON");
        assert_eq!(value["name"], "x");
    }

    #[test]
    fn rejects_output_without_json() {
        assert!(extract_json("我不会").is_none());
        assert!(extract_json("[1,2,3]").is_none());
    }

    #[test]
    fn slugs_are_safe_for_placeholders() {
        assert_eq!(slug("Frontend UI!", "n"), "frontend-ui");
        assert_eq!(slug("研究", "node-1"), "node-1");
        assert_eq!(slug("a  b", "n"), "a-b");
    }

    #[test]
    fn builds_a_plan_from_model_json() {
        let value = json!({
            "name": "加自动更新",
            "nodes": [
                { "id": "research", "title": "调研", "task": "查更新链路", "agent": "explore" },
                { "id": "impl", "title": "实现", "task": "写代码\n{{nodes.research.output}}",
                  "agent": "backend", "dependsOn": ["research"], "workspace": "worktree" },
                { "id": "verify", "title": "验证", "task": "跑测试", "agent": "verifier",
                  "dependsOn": ["impl"], "gate": true }
            ]
        });
        let plan = from_json(&value, "加自动更新", "/tmp/p", &agents(), 8).unwrap();
        assert_eq!(plan.nodes.len(), 3);
        assert_eq!(plan.name, "加自动更新");
        assert_eq!(plan.nodes[1].workspace, Some(Workspace::Worktree));
        assert!(plan.nodes[2].needs_gate());
        // 生成出来的计划本身必须是干净的（Agent 目录要跟注册表一致）。
        let known: Vec<String> = agents().into_iter().map(|agent| agent.id).collect();
        let issues = validate::validate(&plan, &known);
        assert!(
            issues.iter().all(|issue| issue.level != Level::Error),
            "{:?}",
            issues.iter().map(|issue| &issue.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn unknown_agent_falls_back_and_unknown_deps_are_dropped() {
        let value = json!({
            "nodes": [
                { "id": "a", "title": "a", "task": "x", "agent": "ghost", "dependsOn": ["nope"] }
            ]
        });
        let plan = from_json(&value, "目标", "/tmp", &agents(), 8).unwrap();
        assert!(plan.nodes[0].agent.is_none(), "未知 Agent 应当退回默认");
        assert!(plan.nodes[0].depends_on.is_empty(), "悬空依赖应当被删掉");
    }

    /// 真机验证：拿真实目标让 Planner 调一次模型，断言产出的图能过校验。
    ///
    /// `cargo test --lib -- --ignored plans_a_real_goal`（会消耗少量额度，不走 CI）。
    /// 单测只能证明"给定 JSON 能拼成图"，证明不了"模型真的会给这个形状"。
    #[test]
    #[ignore = "真机：会调用配置里的模型服务，消耗额度"]
    fn plans_a_real_goal() {
        let goal = "给设置页加自动更新：检查新版本、显示下载进度、下载完重启安装";
        let project = env!("CARGO_MANIFEST_DIR");
        let plan = draft(goal, project, &agents(), 6).expect("Planner 应当能拆出计划");
        println!("计划名：{}", plan.name);
        for node in &plan.nodes {
            println!(
                "  {} | {} | agent={:?} | deps={:?} | gate={}",
                node.id,
                node.title,
                node.agent,
                node.depends_on,
                node.needs_gate()
            );
        }
        assert!(
            (2..=6).contains(&plan.nodes.len()),
            "节点数越界：{}",
            plan.nodes.len()
        );
        // 节点上限是"请求的上限"，不是"必须凑满"；只要求至少能分出一个下游。
        assert!(
            plan.nodes.iter().any(|node| !node.depends_on.is_empty()),
            "一个依赖都没有，等于没拆图"
        );
        let known: Vec<String> = agents().into_iter().map(|agent| agent.id).collect();
        let issues = validate::validate(&plan, &known);
        assert!(
            issues.iter().all(|issue| issue.level != Level::Error),
            "模型给的图没通过校验：{:?}",
            issues.iter().map(|issue| &issue.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn node_count_is_capped() {
        let nodes: Vec<Value> = (0..12)
            .map(|index| json!({ "id": format!("n{index}"), "title": "t", "task": "x" }))
            .collect();
        let plan = from_json(&json!({ "nodes": nodes }), "目标", "/tmp", &agents(), 8).unwrap();
        assert_eq!(plan.nodes.len(), 8);
    }

    #[test]
    fn duplicate_ids_are_renamed_not_dropped() {
        let value = json!({
            "nodes": [
                { "id": "same", "title": "a", "task": "x" },
                { "id": "same", "title": "b", "task": "y" }
            ]
        });
        let plan = from_json(&value, "目标", "/tmp", &agents(), 8).unwrap();
        assert_eq!(plan.nodes.len(), 2);
        assert_ne!(plan.nodes[0].id, plan.nodes[1].id);
    }

    #[test]
    fn node_without_task_is_rejected() {
        let value = json!({ "nodes": [{ "id": "a", "title": "a" }] });
        assert!(from_json(&value, "目标", "/tmp", &agents(), 8).is_err());
    }

    #[test]
    fn empty_goal_is_rejected_before_any_network_call() {
        let error = draft("   ", "/tmp", &agents(), 8).unwrap_err();
        assert!(error.contains("目标"), "{error}");
    }
}
