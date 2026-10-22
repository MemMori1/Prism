//! Planner 的七工具：让 Planner 从"一次性生成器"变成"能改图的 Agent"。
//!
//! ```text
//! prism.dag.plan.read    读当前计划（节点、边、校验）
//! prism.dag.node.add     加节点
//! prism.dag.node.update  改节点
//! prism.dag.node.remove  删节点（连带清理引用）
//! prism.dag.edge.add     加依赖
//! prism.dag.edge.remove  删依赖
//! prism.dag.validate     校验
//! ```
//!
//! 三条纪律：
//! - **只能改自己这张图**：`dag_id` 来自执行器注入的 [`DagScope`]，模型没有
//!   "改哪张图"这个参数。想改别人的图，连入口都没有。
//! - **有错就不落盘**：每次改动都跑一遍 `validate`，出现 Error 级问题就整体回滚，
//!   把问题原样带回去让模型自己修。宁可这一步白做，也不给用户留一张跑不起来的图。
//! - **运行中不许改**：执行中的调度器手里握着节点快照，这时候改图会让它去跑一个
//!   已经不存在（或刚变形）的节点。

use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};

use super::tools::{
    builtin_full, Network, Permission, Risk, ToolContext, ToolDef, ToolMeta, ToolOutcome,
    ToolResult, ToolRun, ToolScope,
};
use crate::agents;
use crate::dag::model::{DagNode, DagPlan, Gate, Position};
use crate::dag::run::RunStatus;
use crate::dag::store;
use crate::dag::validate::{self, Level};

pub fn tools() -> Vec<Arc<ToolDef>> {
    vec![
        dag_tool("prism.dag.plan.read", "读计划", plan_read_schema, Risk::Read, run_plan_read),
        dag_tool("prism.dag.node.add", "加节点", node_add_schema, Risk::Write, run_node_add),
        dag_tool(
            "prism.dag.node.update",
            "改节点",
            node_update_schema,
            Risk::Write,
            run_node_update,
        ),
        dag_tool(
            "prism.dag.node.remove",
            "删节点",
            node_remove_schema,
            Risk::Write,
            run_node_remove,
        ),
        dag_tool("prism.dag.edge.add", "加依赖", edge_add_schema, Risk::Write, run_edge_add),
        dag_tool(
            "prism.dag.edge.remove",
            "删依赖",
            edge_remove_schema,
            Risk::Write,
            run_edge_remove,
        ),
        dag_tool(
            "prism.dag.validate",
            "校验计划",
            validate_schema,
            Risk::Read,
            run_validate,
        ),
    ]
}

/// 七个工具共用：只在 DAG 节点里出现，标签 `dag:planner`。
fn dag_tool(
    name: &'static str,
    blurb: &'static str,
    schema: fn() -> Value,
    risk: Risk,
    run: ToolRun,
) -> Arc<ToolDef> {
    builtin_full(
        name,
        blurb,
        schema,
        // 改的是 `~/.prism` 里的计划文件，不是工作目录；给 WorkspaceWrite 只是
        // 为了让"只读 Agent 不能改图"这条规则由 permission 层统一挡住。
        Permission::WriteWorkspace,
        None,
        None,
        "",
        // `delegable` 在这里的实际含义是"能进 schema"。子代理拿不到改图权，
        // 是因为 `subagent::specs_for_with` 一律传 `dag: false`，而不是靠这个开关。
        true,
        ToolMeta {
            tags: &["dag:planner"],
            risk,
            network: Network::None,
            sandbox: None,
            scope: ToolScope::Dag,
        },
        run,
    )
}

// ---------------------------------------------------------------- 声明

fn plan_read_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "prism.dag.plan.read",
            "description": "读当前这张 DAG 的节点、依赖边与校验结果。改图前先读一次，避免凭记忆改。",
            "parameters": {
                "type": "object",
                "properties": {
                    "includeTasks": { "type": "boolean", "description": "是否带回节点提示词全文，默认 false" }
                }
            }
        }
    })
}

fn node_add_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "prism.dag.node.add",
            "description": "往当前 DAG 加一个节点。id 用小写字母数字与连字符（如 sample-prep），dependsOn 里只能填已存在的节点。",
            "parameters": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "节点 id，小写字母数字与 - _" },
                    "title": { "type": "string", "description": "节点标题" },
                    "task": { "type": "string", "description": "提示词，可用 {{nodes.<id>.output}} / {{artifacts.<别名>.summary}}" },
                    "agent": { "type": "string", "description": "用哪个 Agent（如 static-analysis）；省略则用计划默认" },
                    "dependsOn": { "type": "array", "items": { "type": "string" }, "description": "上游节点 id" },
                    "gate": { "type": "boolean", "description": "开跑前是否要人工确认" },
                    "artifactKey": { "type": "string", "description": "产出制品 key，默认 report" }
                },
                "required": ["id", "title", "task"]
            }
        }
    })
}

fn node_update_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "prism.dag.node.update",
            "description": "改一个已存在的节点。只传要改的字段，没传的保持原样。",
            "parameters": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "要改的节点 id" },
                    "title": { "type": "string" },
                    "task": { "type": "string" },
                    "agent": { "type": "string" },
                    "dependsOn": { "type": "array", "items": { "type": "string" } },
                    "gate": { "type": "boolean" },
                    "artifactKey": { "type": "string" }
                },
                "required": ["id"]
            }
        }
    })
}

fn node_remove_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "prism.dag.node.remove",
            "description": "删一个节点，并自动清掉别的节点对它的 dependsOn 与制品引用。提示词里若还有 {{nodes.<id>.output}} 会报错——那需要你先把上游任务改掉。",
            "parameters": {
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }
        }
    })
}

fn edge_add_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "prism.dag.edge.add",
            "description": "加一条依赖：from 先跑，to 后跑。成环会被拒绝。",
            "parameters": {
                "type": "object",
                "properties": {
                    "from": { "type": "string", "description": "上游节点 id" },
                    "to": { "type": "string", "description": "下游节点 id" }
                },
                "required": ["from", "to"]
            }
        }
    })
}

fn edge_remove_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "prism.dag.edge.remove",
            "description": "删一条依赖边（不会删节点）。",
            "parameters": {
                "type": "object",
                "properties": {
                    "from": { "type": "string" },
                    "to": { "type": "string" }
                },
                "required": ["from", "to"]
            }
        }
    })
}

fn validate_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "prism.dag.validate",
            "description": "跑一遍校验，返回 error / warning 列表。error 会让图跑不起来。",
            "parameters": { "type": "object", "properties": {} }
        }
    })
}

// ---------------------------------------------------------------- 公共

/// 取出当前计划，并确认它现在可以被改。
fn load(ctx: &ToolContext<'_>) -> Result<DagPlan, ToolResult> {
    let plan_id = ctx
        .dag
        .map(|scope| scope.dag_id.clone())
        .ok_or_else(|| ToolResult::fail("这些工具只在 DAG 节点里可用"))?;
    // 运行中改图 = 让调度器去跑一个刚变形或已消失的节点。
    if let Ok(Some(run)) = store::latest_run_of(&plan_id) {
        if run.status == RunStatus::Running {
            return Err(ToolResult::fail(format!(
                "计划 {plan_id} 正在运行，改图不会生效：先停下再改。"
            )));
        }
    }
    store::get(&plan_id).map_err(ToolResult::fail)
}

fn known_agents() -> Vec<String> {
    agents::list()
        .into_iter()
        .map(|profile| profile.id)
        .collect()
}

fn issues_json(issues: &[validate::Issue]) -> Value {
    json!(
        issues
            .iter()
            .map(|issue| json!({
                "level": match issue.level {
                    Level::Error => "error",
                    Level::Warning => "warning",
                },
                "node": issue.node_id,
                "message": issue.message,
            }))
            .collect::<Vec<Value>>()
    )
}

/// 计划摘要：不带提示词正文，够模型判断"现在图长什么样"。
fn summary(plan: &DagPlan) -> Value {
    let nodes: Vec<Value> = plan
        .nodes
        .iter()
        .map(|node| {
            json!({
                "id": node.id,
                "title": node.title,
                "agent": node.agent,
                "dependsOn": node.depends_on,
                "gate": node.needs_gate(),
                "artifactKey": node.artifact_key(),
            })
        })
        .collect();
    let edges: Vec<Value> = plan
        .nodes
        .iter()
        .flat_map(|node| {
            node.depends_on.iter().map(move |upstream| {
                json!({ "from": upstream, "to": node.id })
            })
        })
        .collect();
    json!({
        "id": plan.id,
        "name": plan.name,
        "goal": plan.goal,
        "nodeCount": plan.nodes.len(),
        "nodes": nodes,
        "edges": edges,
    })
}

/// 校验 → 有错就整体不落盘 → 没问题才写。
///
/// 回滚而不是"边改边修"：留一张非法图给用户，比这一步白做糟得多。
fn commit(plan: DagPlan, action: &str) -> ToolResult {
    let Some(dir) = store::plans_dir() else {
        return ToolResult::fail("找不到 ~/.prism 目录，无法保存计划");
    };
    commit_at(&dir, plan, action)
}

/// 落盘前的最后一道：有 Error 就整体不写。
fn commit_at(dir: &Path, plan: DagPlan, action: &str) -> ToolResult {
    let agents = known_agents();
    let issues = validate::validate(&plan, &agents);
    let errors = issues.iter().filter(|issue| issue.level == Level::Error).count();
    if errors > 0 {
        let payload = json!({
            "action": action,
            "applied": false,
            "reason": "校验没通过，这次改动已回滚",
            "issues": issues_json(&issues),
            "plan": summary(&plan),
        });
        return ToolResult::plain(ToolOutcome::fail(
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()),
        ));
    }
    match store::save_at(dir, plan.clone()) {
        Ok(saved) => respond(action, true, &saved, &issues),
        Err(error) => ToolResult::fail(format!("保存计划失败：{error}")),
    }
}

fn respond(action: &str, applied: bool, plan: &DagPlan, issues: &[validate::Issue]) -> ToolResult {
    let payload = json!({
        "action": action,
        "applied": applied,
        "issues": issues_json(issues),
        "plan": summary(plan),
    });
    ToolResult::plain(ToolOutcome::ok(
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()),
    ))
}

fn text_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn list_arg(args: &Value, key: &str) -> Option<Vec<String>> {
    args.get(key).and_then(Value::as_array).map(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect()
    })
}

/// 新节点的落点：按已有节点数排成网格，避开已被占用的坐标。
///
/// 只按 `len()` 算是不够的：删过节点之后再加，新节点会正好落在别人的格子上。
fn auto_position(plan: &DagPlan) -> Position {
    let taken: Vec<(i64, i64)> = plan
        .nodes
        .iter()
        .filter_map(|node| node.position.as_ref())
        .map(|spot| (spot.x as i64, spot.y as i64))
        .collect();
    let mut index = plan.nodes.len();
    loop {
        let x = 120 + (index % 4) as i64 * 220;
        let y = 100 + (index / 4) as i64 * 150;
        if !taken.contains(&(x, y)) {
            return Position {
                x: x as f64,
                y: y as f64,
            };
        }
        index += 1;
    }
}

// ---------------------------------------------------------------- 读

fn run_plan_read(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    let plan = match load(ctx) {
        Ok(plan) => plan,
        Err(failure) => return failure,
    };
    let include_tasks = args
        .get("includeTasks")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let agents = known_agents();
    let issues = validate::validate(&plan, &agents);
    let mut payload = summary(&plan);
    payload["issues"] = issues_json(&issues);
    if include_tasks {
        payload["tasks"] = json!(
            plan.nodes
                .iter()
                .map(|node| json!({ "id": node.id, "task": node.task }))
                .collect::<Vec<Value>>()
        );
    }
    ToolResult::plain(ToolOutcome::ok(
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()),
    ))
}

fn run_validate(ctx: &ToolContext<'_>, _args: &Value) -> ToolResult {
    let plan = match load(ctx) {
        Ok(plan) => plan,
        Err(failure) => return failure,
    };
    let issues = validate::validate(&plan, &known_agents());
    let errors = issues
        .iter()
        .filter(|issue| issue.level == Level::Error)
        .count();
    let payload = json!({
        "planId": plan.id,
        "ok": errors == 0,
        "errorCount": errors,
        "issues": issues_json(&issues),
    });
    ToolResult::plain(ToolOutcome::ok(
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string()),
    ))
}

// ---------------------------------------------------------------- 修改

/// 一次改动的结果：(新计划, 用于回报的动作描述)。
type Edit = Result<(DagPlan, String), String>;

/// 五个写工具的共同外壳：读图 → 改 → 校验落盘。
///
/// 把"改"抽成纯函数（`apply_*`）而不是混在入口里，是为了让单测能直接喂一张
/// 内存里的图：工具的错大多出在"改"这一步，而 `~/.prism` 不该被测试碰。
fn run_edit(ctx: &ToolContext<'_>, args: &Value, apply: fn(DagPlan, &Value) -> Edit) -> ToolResult {
    let plan = match load(ctx) {
        Ok(plan) => plan,
        Err(failure) => return failure,
    };
    match apply(plan, args) {
        Ok((plan, action)) => commit(plan, &action),
        Err(error) => ToolResult::fail(error),
    }
}

fn run_node_add(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    run_edit(ctx, args, apply_node_add)
}

fn run_node_update(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    run_edit(ctx, args, apply_node_update)
}

fn run_node_remove(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    run_edit(ctx, args, apply_node_remove)
}

fn run_edge_add(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    run_edit(ctx, args, apply_edge_add)
}

fn run_edge_remove(ctx: &ToolContext<'_>, args: &Value) -> ToolResult {
    run_edit(ctx, args, apply_edge_remove)
}

fn apply_node_add(mut plan: DagPlan, args: &Value) -> Edit {
    let (Some(id), Some(title), Some(task)) = (
        text_arg(args, "id"),
        text_arg(args, "title"),
        text_arg(args, "task"),
    ) else {
        return Err("加节点需要 id / title / task".to_string());
    };
    if plan.node(&id).is_some() {
        return Err(format!("节点 {id} 已经存在；要改它请用 prism.dag.node.update"));
    }
    let position = auto_position(&plan);
    plan.nodes.push(DagNode {
        id: id.clone(),
        title,
        task,
        depends_on: list_arg(args, "dependsOn").unwrap_or_default(),
        agent: text_arg(args, "agent"),
        workspace: None,
        tools: None,
        permission: None,
        retry: Default::default(),
        timeout_secs: None,
        gate: args.get("gate").and_then(Value::as_bool).map(|before| Gate {
            before,
            after: false,
        }),
        position: Some(position),
        input_artifacts: Vec::new(),
        output_artifact_key: text_arg(args, "artifactKey").unwrap_or_default(),
    });
    Ok((plan, format!("加节点 {id}")))
}

fn apply_node_update(mut plan: DagPlan, args: &Value) -> Edit {
    let Some(id) = text_arg(args, "id") else {
        return Err("改节点需要 id".to_string());
    };
    let Some(index) = plan.nodes.iter().position(|node| node.id == id) else {
        return Err(format!("没有节点 {id}"));
    };
    // 只改传了的字段：没传 ≠ 清空。模型经常只报一个字段，不能把其余字段一起抹掉。
    let node = &mut plan.nodes[index];
    if let Some(title) = text_arg(args, "title") {
        node.title = title;
    }
    if let Some(task) = text_arg(args, "task") {
        node.task = task;
    }
    if let Some(agent) = text_arg(args, "agent") {
        node.agent = Some(agent);
    }
    if let Some(deps) = list_arg(args, "dependsOn") {
        node.depends_on = deps;
    }
    if let Some(before) = args.get("gate").and_then(Value::as_bool) {
        node.gate = Some(Gate {
            before,
            after: false,
        });
    }
    if let Some(key) = text_arg(args, "artifactKey") {
        node.output_artifact_key = key;
    }
    Ok((plan, format!("改节点 {id}")))
}

fn apply_node_remove(mut plan: DagPlan, args: &Value) -> Edit {
    let Some(id) = text_arg(args, "id") else {
        return Err("删节点需要 id".to_string());
    };
    if plan.node(&id).is_none() {
        return Err(format!("没有节点 {id}"));
    }
    plan.nodes.retain(|node| node.id != id);
    // 连带清理：留着悬空依赖，图立刻就是非法的，而模型多半不会记得回头改。
    // 提示词里的占位符也一并换成一句人话——不删（那会悄悄丢掉上下文），
    // 而是标出来，让写这段提示的模型/人知道这里断了。
    let marker = format!("（上游节点 {id} 已删除，这段需要改写）");
    let placeholder = format!("{{{{nodes.{id}.output}}}}");
    for node in &mut plan.nodes {
        node.depends_on.retain(|upstream| upstream != &id);
        node.input_artifacts.retain(|input| input.from != id);
        if node.task.contains(&placeholder) {
            node.task = node.task.replace(&placeholder, &marker);
        }
    }
    plan.layout.remove(&id);
    Ok((plan, format!("删节点 {id}")))
}

fn apply_edge_add(mut plan: DagPlan, args: &Value) -> Edit {
    let (Some(from), Some(to)) = (text_arg(args, "from"), text_arg(args, "to")) else {
        return Err("加依赖需要 from 与 to".to_string());
    };
    if plan.node(&from).is_none() || plan.node(&to).is_none() {
        return Err(format!("{from} 或 {to} 不存在"));
    }
    if from == to {
        return Err("不能让节点依赖自己".to_string());
    }
    let Some(index) = plan.nodes.iter().position(|node| node.id == to) else {
        return Err(format!("没有节点 {to}"));
    };
    if plan.nodes[index].depends_on.iter().any(|item| item == &from) {
        return Err(format!("{to} 已经依赖 {from}"));
    }
    plan.nodes[index].depends_on.push(from.clone());
    Ok((plan, format!("加依赖 {from} → {to}")))
}

fn apply_edge_remove(mut plan: DagPlan, args: &Value) -> Edit {
    let (Some(from), Some(to)) = (text_arg(args, "from"), text_arg(args, "to")) else {
        return Err("删依赖需要 from 与 to".to_string());
    };
    let Some(index) = plan.nodes.iter().position(|node| node.id == to) else {
        return Err(format!("没有节点 {to}"));
    };
    let before = plan.nodes[index].depends_on.len();
    plan.nodes[index].depends_on.retain(|item| item != &from);
    if plan.nodes[index].depends_on.len() == before {
        return Err(format!("{to} 本来就不依赖 {from}"));
    }
    Ok((plan, format!("删依赖 {from} → {to}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::model::VERSION;
    use std::path::PathBuf;

    fn node(id: &str, deps: &[&str]) -> DagNode {
        DagNode {
            id: id.to_string(),
            title: format!("{id} 标题"),
            task: if deps.is_empty() {
                "做第一步".to_string()
            } else {
                format!("基于 {{{{nodes.{}.output}}}} 继续", deps[0])
            },
            depends_on: deps.iter().map(|item| item.to_string()).collect(),
            agent: Some("general".to_string()),
            workspace: None,
            tools: None,
            permission: None,
            retry: Default::default(),
            timeout_secs: None,
            gate: None,
            position: None,
            input_artifacts: Vec::new(),
            output_artifact_key: String::new(),
        }
    }

    fn sample_plan() -> DagPlan {
        DagPlan {
            version: VERSION,
            id: "dag-test".to_string(),
            name: "样例计划".to_string(),
            goal: "拆一条安全分析流水线".to_string(),
            project: "/tmp".to_string(),
            defaults: Default::default(),
            nodes: vec![node("research", &[])],
            layout: Default::default(),
            created_at: 0,
            updated_at: 0,
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prism-dagtools-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn read_back(dir: &Path) -> DagPlan {
        store::get_at(dir, "dag-test").unwrap()
    }

    #[test]
    fn node_add_lands_on_disk() {
        let dir = temp_dir("add");
        store::save_at(&dir, sample_plan()).unwrap();

        let (plan, action) = apply_node_add(
            read_back(&dir),
            &json!({
                "id": "static",
                "title": "静态分析",
                "task": "看导入表",
                "agent": "static-analysis",
                "dependsOn": ["research"],
                "gate": true,
            }),
        )
        .unwrap();
        assert_eq!(action, "加节点 static");
        let result = commit_at(&dir, plan, &action);
        assert!(result.outcome.error.is_none(), "{}", result.outcome.text);

        let saved = read_back(&dir);
        assert_eq!(saved.nodes.len(), 2);
        let added = saved.node("static").unwrap();
        assert_eq!(added.agent.as_deref(), Some("static-analysis"));
        assert_eq!(added.depends_on, vec!["research"]);
        assert!(added.needs_gate());
    }

    #[test]
    fn node_add_refuses_duplicate_id() {
        let error = apply_node_add(
            sample_plan(),
            &json!({ "id": "research", "title": "重复", "task": "x" }),
        )
        .unwrap_err();
        assert!(error.contains("已经存在"), "{error}");
    }

    #[test]
    fn cycle_is_rejected_and_nothing_is_written() {
        let dir = temp_dir("cycle");
        let mut base = sample_plan();
        base.nodes.push(node("second", &["research"]));
        store::save_at(&dir, base).unwrap();

        // 反向再加一条：second → research 会成环。
        let (plan, action) = apply_edge_add(
            read_back(&dir),
            &json!({ "from": "second", "to": "research" }),
        )
        .unwrap();
        let result = commit_at(&dir, plan, &action);
        assert!(result.outcome.error.is_some(), "成环必须被拒：{}", result.outcome.text);
        assert!(result.outcome.text.contains("回滚"), "{}", result.outcome.text);

        let saved = read_back(&dir);
        assert!(
            saved.node("research").unwrap().depends_on.is_empty(),
            "盘上不该留下这条边"
        );
    }

    #[test]
    fn node_remove_cleans_dangling_references() {
        let dir = temp_dir("remove");
        let mut base = sample_plan();
        base.nodes.push(node("second", &["research"]));
        store::save_at(&dir, base).unwrap();

        let (plan, action) = apply_node_remove(read_back(&dir), &json!({ "id": "research" })).unwrap();
        let result = commit_at(&dir, plan, &action);
        assert!(result.outcome.error.is_none(), "{}", result.outcome.text);

        let saved = read_back(&dir);
        assert!(saved.node("research").is_none());
        assert!(
            saved.node("second").unwrap().depends_on.is_empty(),
            "悬空依赖应该被一起清掉"
        );
    }

    #[test]
    fn node_update_touches_only_given_fields() {
        let plan = sample_plan();
        let (updated, _) = apply_node_update(plan, &json!({ "id": "research", "title": "新标题" }))
            .unwrap();
        let node = updated.node("research").unwrap();
        assert_eq!(node.title, "新标题");
        assert_eq!(node.task, "做第一步", "没传 task 就不能被清空");
        assert_eq!(node.agent.as_deref(), Some("general"));
    }

    #[test]
    fn edge_remove_rejects_missing_edge() {
        let error = apply_edge_remove(
            sample_plan(),
            &json!({ "from": "research", "to": "research" }),
        )
        .unwrap_err();
        assert!(error.contains("本来就不依赖"), "{error}");
    }

    #[test]
    fn edge_add_rejects_self_and_unknown() {
        let plan = sample_plan();
        assert!(apply_edge_add(plan.clone(), &json!({ "from": "research", "to": "research" })).is_err());
        assert!(apply_edge_add(plan, &json!({ "from": "ghost", "to": "research" })).is_err());
    }

    #[test]
    fn added_node_gets_its_own_canvas_spot() {
        // 真实计划里节点都带坐标（Planner 与画布都会写），所以这里也带上。
        let mut plan = sample_plan();
        plan.nodes[0].position = Some(Position {
            x: 120.0,
            y: 100.0,
        });
        let mut second = node("second", &["research"]);
        second.position = Some(Position {
            x: 340.0,
            y: 100.0,
        });
        plan.nodes.push(second);

        let (grown, _) = apply_node_add(
            plan,
            &json!({ "id": "third", "title": "第三", "task": "x" }),
        )
        .unwrap();
        let third = grown.node("third").unwrap().position.clone().unwrap();
        let clash = grown
            .nodes
            .iter()
            .filter(|node| node.id != "third")
            .filter_map(|node| node.position.as_ref())
            .any(|spot| (spot.x as i64, spot.y as i64) == (third.x as i64, third.y as i64));
        assert!(!clash, "新节点不能叠在旧节点上：{:?}", (third.x, third.y));
    }

    #[test]
    fn planner_tools_are_hidden_outside_a_dag() {
        let enabled = super::super::tools::EnabledTools::all();
        let names = |dag: bool| -> Vec<String> {
            super::super::tools::specs_for(&enabled, Permission::WriteWorkspace, dag)
                .iter()
                .filter_map(|spec| spec["function"]["name"].as_str().map(str::to_string))
                .collect()
        };
        let chat = names(false);
        assert!(
            !chat.iter().any(|name| name.starts_with("prism.dag.")),
            "聊天里不该出现改图工具：{chat:?}"
        );
        let inside = names(true);
        for expected in [
            "prism.dag.plan.read",
            "prism.dag.node.add",
            "prism.dag.node.update",
            "prism.dag.node.remove",
            "prism.dag.edge.add",
            "prism.dag.edge.remove",
            "prism.dag.validate",
        ] {
            assert!(
                inside.iter().any(|name| name == expected),
                "DAG 作用域里应有 {expected}"
            );
        }
    }

    #[test]
    fn every_planner_tool_is_dag_scoped() {
        for tool in tools() {
            assert_eq!(tool.meta.scope, ToolScope::Dag, "{} 必须在 DAG 作用域", tool.name);
            assert_eq!(tool.meta.tags, vec!["dag:planner"], "{} 标签不对", tool.name);
            assert_eq!(tool.meta.network, Network::None, "{} 不该出网", tool.name);
            assert!(
                tool.delegable,
                "{} 必须能进 schema（子代理那一层由 dag=false 挡住）",
                tool.name
            );
        }
    }
}
