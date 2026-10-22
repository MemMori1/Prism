//! 计划校验：环、未知依赖、重复 id、占位符引用。
//!
//! 纯函数，不碰网络也不碰磁盘——它同时服务三个入口：保存前、运行前、
//! 前端实时提示。**校验必须能给出可操作的句子**（哪条边、缺什么），而不是
//! 一句"计划无效"。

use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};

use super::model::DagPlan;
/// 单张图最多多少个节点：再多人也看不完，模型也规划不准。
pub const MAX_NODES: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Level {
    /// 拦住运行。
    Error,
    /// 能跑，但大概率不是用户想要的结果。
    Warning,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    pub level: Level,
    /// 哪个节点的问题；整张图级别的问题为 `None`。
    pub node_id: Option<String>,
    pub message: String,
}

fn error(node_id: Option<&str>, message: impl Into<String>) -> Issue {
    Issue {
        level: Level::Error,
        node_id: node_id.map(str::to_string),
        message: message.into(),
    }
}

fn warning(node_id: Option<&str>, message: impl Into<String>) -> Issue {
    Issue {
        level: Level::Warning,
        node_id: node_id.map(str::to_string),
        message: message.into(),
    }
}

/// 节点 id 的字符集：字母数字 + `-` `_`。它要能安全地出现在占位符
/// `{{nodes.<id>.output}}` 里，所以不能有空格和点。
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

/// 从 task 里抽出 `{{nodes.<id>.output|report|files}}` 引用的节点 id。
///
/// 手写解析而不是上正则：这里只需要认一种固定前缀，正则依赖反而更重。
/// 认到前缀但**没有闭合的 `}}`** 时当作没写完，直接停下——半截占位符不该
/// 变成一条"引用了节点 a"的假引用。
pub fn placeholder_refs(task: &str) -> Vec<String> {
    const PREFIX: &str = "{{nodes.";
    let mut out: Vec<String> = Vec::new();
    let mut rest = task;
    while let Some(start) = rest.find(PREFIX) {
        let after = &rest[start + PREFIX.len()..];
        let Some(close) = after.find("}}") else {
            break;
        };
        // `id.output` / `id.report`：点之前那段才是节点 id。
        let id = after[..close].split('.').next().unwrap_or("").trim();
        if !id.is_empty() && !out.iter().any(|item| item == id) {
            out.push(id.to_string());
        }
        rest = &after[close + 2..];
    }
    out
}

/// 制品占位符支持的字段。
pub const ARTIFACT_FIELDS: [&str; 3] = ["id", "summary", "meta"];

/// 从 task 里抽出 `{{artifacts.<别名>.<字段>}}` 的（别名, 字段）对。
///
/// 不写字段（`{{artifacts.notes}}`）按 `summary` 算——和渲染端一致。
pub fn artifact_refs(task: &str) -> Vec<(String, String)> {
    const PREFIX: &str = "{{artifacts.";
    let mut out: Vec<(String, String)> = Vec::new();
    let mut rest = task;
    while let Some(start) = rest.find(PREFIX) {
        let after = &rest[start + PREFIX.len()..];
        let Some(close) = after.find("}}") else {
            // 没闭合：当作没写完，不再往后找（半截占位符不该变成假引用）。
            break;
        };
        let inner = &after[..close];
        let mut parts = inner.split('.');
        let alias = parts.next().unwrap_or("").trim();
        let field = parts.next().map(str::trim).unwrap_or("summary");
        let field = if field.is_empty() { "summary" } else { field };
        if !alias.is_empty()
            && !out
                .iter()
                .any(|(known, known_field)| known == alias && known_field == field)
        {
            out.push((alias.to_string(), field.to_string()));
        }
        rest = &after[close + 2..];
    }
    out
}

/// 所有祖先（传递依赖）。占位符只能引用祖先——引用兄弟节点等于要求"同时开始
/// 还要读对方结果"，跨节点上下文是 DAG 最容易搞错的地方。
fn ancestors(plan: &DagPlan, id: &str) -> HashSet<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    if let Some(node) = plan.node(id) {
        for dep in &node.depends_on {
            queue.push_back(dep.clone());
        }
    }
    while let Some(current) = queue.pop_front() {
        if !seen.insert(current.clone()) {
            continue;
        }
        if let Some(node) = plan.node(&current) {
            for dep in &node.depends_on {
                queue.push_back(dep.clone());
            }
        }
    }
    seen
}

fn find_cycle(plan: &DagPlan) -> Option<Vec<String>> {
    let mut state: HashMap<&str, u8> = HashMap::new();
    for node in &plan.nodes {
        if state.get(node.id.as_str()).copied().unwrap_or(0) == 0 {
            let mut stack: Vec<&str> = Vec::new();
            if let Some(cycle) = walk(&node.id, plan, &mut state, &mut stack) {
                return Some(cycle);
            }
        }
    }
    None
}

fn walk<'a>(
    id: &'a str,
    plan: &'a DagPlan,
    state: &mut HashMap<&'a str, u8>,
    stack: &mut Vec<&'a str>,
) -> Option<Vec<String>> {
    state.insert(id, 1);
    stack.push(id);
    if let Some(node) = plan.node(id) {
        for dep in &node.depends_on {
            let Some(target) = plan.node(dep) else {
                continue;
            };
            match state.get(target.id.as_str()).copied().unwrap_or(0) {
                0 => {
                    if let Some(cycle) = walk(&target.id, plan, state, stack) {
                        return Some(cycle);
                    }
                }
                1 => {
                    let start = stack
                        .iter()
                        .position(|item| *item == target.id.as_str())
                        .unwrap_or(0);
                    let mut cycle: Vec<String> =
                        stack[start..].iter().map(|item| item.to_string()).collect();
                    cycle.push(target.id.clone());
                    return Some(cycle);
                }
                _ => {}
            }
        }
    }
    stack.pop();
    state.insert(id, 2);
    None
}

/// 校验整张图。`known_agents` 是注册表里的 profile id；
/// 传空切片表示"不检查 Agent 引用"（单独测图结构时用）。
pub fn validate(plan: &DagPlan, known_agents: &[String]) -> Vec<Issue> {
    let mut issues: Vec<Issue> = Vec::new();

    if plan.nodes.is_empty() {
        issues.push(error(None, "计划里还没有节点"));
        return issues;
    }
    if plan.nodes.len() > MAX_NODES {
        issues.push(error(
            None,
            format!(
                "节点太多了（{} 个），单张图上限 {MAX_NODES} 个——拆成多张计划更好维护",
                plan.nodes.len()
            ),
        ));
    }

    // ---- 节点自身 ----
    let mut seen: Vec<&str> = Vec::with_capacity(plan.nodes.len());
    for node in &plan.nodes {
        if !valid_id(&node.id) {
            issues.push(error(
                Some(&node.id),
                format!("节点 id「{}」不合法：只能用字母、数字、连字符与下划线", node.id),
            ));
        }
        if seen.contains(&node.id.as_str()) {
            issues.push(error(Some(&node.id), format!("节点 id「{}」重复了", node.id)));
        } else {
            seen.push(&node.id);
        }
        if node.title.trim().is_empty() {
            issues.push(error(Some(&node.id), "缺少标题"));
        }
        if node.task.trim().is_empty() {
            issues.push(error(
                Some(&node.id),
                "缺少任务描述：子 Agent 看不到别的节点上下文，task 必须自足",
            ));
        }
        for dep in &node.depends_on {
            if dep == &node.id {
                issues.push(error(Some(&node.id), "不能依赖自己"));
            } else if plan.node(dep).is_none() {
                issues.push(error(
                    Some(&node.id),
                    format!("依赖的节点「{dep}」不存在"),
                ));
            }
        }
    }

    // ---- 图结构 ----
    if let Some(cycle) = find_cycle(plan) {
        issues.push(error(None, format!("存在环：{}", cycle.join(" → "))));
    }

    // ---- 占位符 ----
    for node in &plan.nodes {
        let reachable = ancestors(plan, &node.id);
        for reference in placeholder_refs(&node.task) {
            if plan.node(&reference).is_none() {
                issues.push(error(
                    Some(&node.id),
                    format!("占位符引用了不存在的节点「{reference}」"),
                ));
            } else if !reachable.contains(&reference) {
                issues.push(error(
                    Some(&node.id),
                    format!("占位符引用了「{reference}」，但它不是上游（占位符只能取祖先的产物）"),
                ));
            }
        }
    }

    // ---- 制品引用 ----
    let node_ids: HashSet<&str> = plan.nodes.iter().map(|node| node.id.as_str()).collect();
    for node in &plan.nodes {
        // 产出 key：会出现在索引与占位符建议里，字符集必须收窄。
        let key = node.artifact_key();
        if !valid_id(&key) {
            issues.push(error(
                Some(&node.id),
                format!("产出制品 key「{key}」不合法：只能用字母、数字、连字符与下划线"),
            ));
        }

        let reachable = ancestors(plan, &node.id);
        let mut aliases: Vec<String> = Vec::new();
        for input in &node.input_artifacts {
            let alias = input.alias.trim();
            if !valid_id(alias) {
                issues.push(error(
                    Some(&node.id),
                    format!("制品别名「{alias}」不合法：只能用字母、数字、连字符与下划线"),
                ));
            } else if node_ids.contains(alias) {
                // 隐式别名就是上游节点 id；撞名会让人分不清取的是哪个。
                issues.push(error(
                    Some(&node.id),
                    format!("制品别名「{alias}」和节点 id 撞名：占位符会分不清该取哪个"),
                ));
            } else if aliases.iter().any(|known| known == alias) {
                issues.push(error(
                    Some(&node.id),
                    format!("制品别名「{alias}」重复了"),
                ));
            } else {
                aliases.push(alias.to_string());
            }

            match plan.node(&input.from) {
                None => issues.push(error(
                    Some(&node.id),
                    format!("制品引用的节点「{}」不存在", input.from),
                )),
                Some(source) => {
                    if !reachable.contains(&input.from) {
                        issues.push(error(
                            Some(&node.id),
                            format!(
                                "制品引用了「{}」，但它不是上游（制品只能取祖先的产出）",
                                input.from
                            ),
                        ));
                    }
                    // 主产出 key 能静态确认；工具写入的 key（strings 等）运行时才有，
                    // 拦死会让合法的图跑不起来，所以只提醒。
                    let wanted = if input.key.trim().is_empty() {
                        source.artifact_key()
                    } else {
                        input.key.trim().to_string()
                    };
                    if wanted != source.artifact_key() {
                        issues.push(warning(
                            Some(&node.id),
                            format!(
                                "要「{}」的 {} 制品，但它的主产出是 {}：这个 key 得由工具写入，运行时才确认",
                                input.from,
                                wanted,
                                source.artifact_key()
                            ),
                        ));
                    }
                }
            }
        }

        let mut known_names = reachable.clone();
        known_names.extend(aliases.iter().cloned());
        for (alias, field) in artifact_refs(&node.task) {
            if !known_names.contains(&alias) {
                issues.push(error(
                    Some(&node.id),
                    format!("占位符引用了未声明的制品「{alias}」（只能取上游产物，或在「制品」里声明）"),
                ));
            }
            if !ARTIFACT_FIELDS.contains(&field.as_str()) {
                issues.push(error(
                    Some(&node.id),
                    format!("制品占位符不支持 .{field}（只有 id / summary / meta）"),
                ));
            }
        }
    }

    // ---- Agent 引用 ----
    if !known_agents.is_empty() {
        if plan.defaults.agent.trim().is_empty() {
            issues.push(error(None, "没有指定默认 Agent"));
        } else if !known_agents.iter().any(|id| id == &plan.defaults.agent) {
            issues.push(error(
                None,
                format!("默认 Agent「{}」不在注册表里", plan.defaults.agent),
            ));
        }
        for node in &plan.nodes {
            if let Some(agent) = node.agent.as_ref() {
                if !known_agents.iter().any(|id| id == agent) {
                    issues.push(error(
                        Some(&node.id),
                        format!("Agent「{agent}」不在注册表里（可能已被删除或改名）"),
                    ));
                }
            }
        }
    }

    // ---- 提醒（不拦） ----
    let sinks: Vec<&str> = plan
        .nodes
        .iter()
        .filter(|node| {
            !plan
                .nodes
                .iter()
                .any(|other| other.depends_on.iter().any(|dep| dep == &node.id))
        })
        .map(|node| node.id.as_str())
        .collect();
    if sinks.len() > 1 && plan.nodes.len() > 2 {
        issues.push(warning(
            None,
            format!(
                "有 {} 个末端节点（{}）：确认这不是漏了汇总节点",
                sinks.len(),
                sinks.join("、")
            ),
        ));
    }
    if plan.defaults.max_parallel == 0 {
        issues.push(warning(None, "并发上限是 0，一次只能跑一个节点"));
    }

    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::model::{DagDefaults, DagNode, DagPlan, Retry};

    fn node(id: &str, deps: &[&str], task: &str) -> DagNode {
        DagNode {
            id: id.to_string(),
            title: format!("{id} 的标题"),
            task: task.to_string(),
            depends_on: deps.iter().map(|item| item.to_string()).collect(),
            agent: None,
            workspace: None,
            tools: None,
            permission: None,
            retry: Retry::default(),
            timeout_secs: None,
            gate: None,
            position: None,
            input_artifacts: Vec::new(),
            output_artifact_key: String::new(),
        }
    }

    fn plan(nodes: Vec<DagNode>) -> DagPlan {
        DagPlan {
            version: 1,
            id: "dag-test".to_string(),
            name: "测试计划".to_string(),
            goal: "验证校验器".to_string(),
            project: "/tmp".to_string(),
            defaults: DagDefaults::default(),
            nodes,
            layout: Default::default(),
            created_at: 0,
            updated_at: 0,
        }
    }

    fn messages(issues: &[Issue]) -> Vec<String> {
        issues.iter().map(|issue| issue.message.clone()).collect()
    }

    #[test]
    fn empty_plan_is_an_error() {
        let issues = validate(&plan(vec![]), &[]);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].level, Level::Error);
    }

    #[test]
    fn diamond_graph_produces_no_errors() {
        let issues = validate(
            &plan(vec![
                node("research", &[], "调研"),
                node("a", &["research"], "写 A"),
                node("b", &["research"], "写 B"),
                node("verify", &["a", "b"], "验证"),
            ]),
            &[],
        );
        assert!(
            issues.iter().all(|issue| issue.level != Level::Error),
            "{:?}",
            messages(&issues)
        );
    }

    #[test]
    fn cycle_is_reported_with_a_path() {
        let issues = validate(
            &plan(vec![
                node("a", &["c"], "a"),
                node("b", &["a"], "b"),
                node("c", &["b"], "c"),
            ]),
            &[],
        );
        let cycle = issues
            .iter()
            .find(|issue| issue.message.contains("存在环"))
            .expect("应当报环");
        assert!(cycle.message.contains("→"), "{}", cycle.message);
    }

    #[test]
    fn self_dependency_is_rejected() {
        let issues = validate(&plan(vec![node("a", &["a"], "x")]), &[]);
        assert!(messages(&issues).iter().any(|message| message.contains("依赖自己")));
    }

    #[test]
    fn unknown_dependency_is_rejected() {
        let issues = validate(&plan(vec![node("a", &["ghost"], "x")]), &[]);
        assert!(messages(&issues)
            .iter()
            .any(|message| message.contains("「ghost」不存在")));
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let issues = validate(&plan(vec![node("a", &[], "x"), node("a", &[], "y")]), &[]);
        assert!(messages(&issues).iter().any(|message| message.contains("重复")));
    }

    #[test]
    fn placeholder_must_point_at_an_ancestor() {
        // b 依赖 a，引用 a 合法；c 与 a 无依赖关系，引用 a 就是错的。
        let ok = validate(
            &plan(vec![
                node("a", &[], "写点东西"),
                node("b", &["a"], "基于 {{nodes.a.output}} 继续"),
            ]),
            &[],
        );
        assert!(ok.iter().all(|issue| issue.level != Level::Error), "{:?}", messages(&ok));

        let bad = validate(
            &plan(vec![
                node("a", &[], "写点东西"),
                node("c", &[], "偷看 {{nodes.a.report}}"),
            ]),
            &[],
        );
        assert!(messages(&bad)
            .iter()
            .any(|message| message.contains("不是上游")));
    }

    #[test]
    fn placeholder_to_missing_node_is_rejected() {
        let issues = validate(&plan(vec![node("a", &[], "看 {{nodes.ghost.output}}")]), &[]);
        assert!(messages(&issues)
            .iter()
            .any(|message| message.contains("不存在的节点")));
    }

    #[test]
    fn refs_are_deduped_and_ignore_garbage() {
        assert_eq!(
            placeholder_refs("{{nodes.a.output}} 和 {{nodes.a.report}}"),
            vec!["a".to_string()]
        );
        assert_eq!(
            placeholder_refs("{{nodes.b.output}}"),
            vec!["b".to_string()]
        );
        assert!(placeholder_refs("没有占位符 {{nodes }}").is_empty());
        assert!(placeholder_refs("{{nodes.a.output").is_empty());
    }

    #[test]
    fn unknown_agent_is_flagged_only_when_the_registry_is_known() {
        let mut target = plan(vec![node("a", &[], "x")]);
        target.nodes[0].agent = Some("ghost".to_string());

        assert!(validate(&target, &[]).iter().all(|issue| issue.level != Level::Error));
        assert!(messages(&validate(&target, &["general".to_string()]))
            .iter()
            .any(|message| message.contains("不在注册表里")));
    }

    #[test]
    fn multiple_sinks_warn_but_do_not_block() {
        let issues = validate(
            &plan(vec![
                node("root", &[], "x"),
                node("a", &["root"], "y"),
                node("b", &["root"], "z"),
            ]),
            &[],
        );
        assert!(issues.iter().any(|issue| issue.level == Level::Warning));
        assert!(issues.iter().all(|issue| issue.level != Level::Error));
    }
}
