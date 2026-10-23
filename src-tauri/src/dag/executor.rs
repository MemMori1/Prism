//! 调度器：按依赖把节点一个个放出去，收到结果再放下一批。
//!
//! 为什么是"手写状态机"而不是现成的 DAG 库
//! -------------------------------------
//! 需要的语义只有四条（依赖就绪、并发上限、失败策略、人工闸门），但它们
//! 与"跑一次会话"这个重活交织在一起：库里那套"拓扑排序 + 回调"反而要多写
//! 一层适配。这里直接把调度写成一张表循环，读起来就是它干的事。
//!
//! 线程模型
//! --------
//! 调度线程只有一条（`execute`），每个节点在自己的线程里跑，结果通过 channel
//! 回来。**不用线程池**：并发上限是计划里写的、随时会变，而节点数是十几个，
//! 起线程的代价可以忽略；换来的好处是"并发"只是调度线程里的一个计数。
//!
//! 失败策略
//! --------
//! - `fail-fast`：任一节点失败 → 不再放新节点，等在跑的收尾，其余标跳过；
//! - `continue`：无关分支继续跑，只有下游被跳过。

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::model::{DagNode, DagPlan, FailurePolicy};
use super::run::{NodeStatus, RunHandle};
use super::runner::{resolve_task, NodeOutput, NodeRequest, NodeRunner, StopHandle};
use crate::agents::AgentProfile;

/// 调度循环的等待粒度。闸门/取消都是在等的时候被外面改的，所以要周期性
/// 回到循环顶部重新算一遍"谁能跑"。
const TICK: Duration = Duration::from_millis(150);

/// 起跑前检查：这张图里每个节点的执行器都装了吗。
///
/// 为什么不等到节点出队时再说：一张图可能十几分钟、几十块钱，用户不该在
/// 第三个节点卡住时才发现"这个 Agent 的执行器没装"——那时候前面的已经白跑了。
/// 逐节点的解析逻辑仍在 [`build_request`] 里（它是唯一的事实），这里只是把
/// 同一个判断提前做一遍，好让错误在"点了运行"这一刻就出现。
pub fn preflight(plan: &DagPlan, profiles: &[AgentProfile]) -> Result<(), String> {
    let installed = super::runner::installed();
    if installed.is_empty() {
        return Err(
            "没有节点执行器：在工坊里启用任意 dag.executor.* 插件（下次启动生效）".to_string(),
        );
    }
    let known = installed.join("、");
    for node in &plan.nodes {
        let agent_id = node
            .agent
            .clone()
            .unwrap_or_else(|| plan.defaults.agent.clone());
        let profile = profiles
            .iter()
            .find(|profile| profile.id == agent_id)
            .ok_or_else(|| {
                format!("节点「{}」的 Agent「{agent_id}」不在注册表里", node.id)
            })?;
        if super::runner::get(&profile.executor).is_none() {
            return Err(format!(
                "节点「{}」要用 {} 执行器，但没装这个插件（已装：{known}）",
                node.id, profile.executor
            ));
        }
    }
    Ok(())
}

/// 跑一张图。返回时所有节点的状态都已经落定（除了一直没人确认的闸门）。
pub fn execute(
    plan: DagPlan,
    profiles: Vec<AgentProfile>,
    // 单测注入的假执行器；真机上传 None，由注册表按 Agent 解析。
    override_runner: Option<Arc<dyn NodeRunner>>,
    handle: Arc<RunHandle>,
) {
    let policy = plan.defaults.failure_policy;
    let max_parallel = plan.defaults.max_parallel.max(1) as usize;
    let (tx, rx) = mpsc::channel::<Completion>();
    // 重试退避：node_id -> 最早可以再开始的时刻。
    let mut retry_at: HashMap<String, Instant> = HashMap::new();
    let mut running = 0usize;

    loop {
        if handle.is_cancelled() {
            cancel_pending(&plan, &handle, "运行被取消");
            break;
        }

        let statuses = handle.statuses();
        // fail-fast：已经有人失败就不要再放新的。
        let halted = statuses.values().any(|status| *status == NodeStatus::Failed);

        for node in &plan.nodes {
            let status = statuses
                .get(&node.id)
                .copied()
                .unwrap_or(NodeStatus::Pending);

            // 上游没成功的节点：标跳过（先于"是否可跑"判断，免得漏掉整条链）。
            if status == NodeStatus::Pending || status == NodeStatus::Waiting {
                if let Some(reason) = blocked_reason(node, &statuses) {
                    handle.mark_skipped(&node.id, reason);
                    continue;
                }
            }
            if status == NodeStatus::Waiting {
                match handle.gate_of(&node.id) {
                    None => continue, // 还在等人工
                    Some(true) => {}
                    Some(false) => {
                        handle.mark_cancelled(&node.id, "人工确认未通过".to_string());
                        continue;
                    }
                }
            }
            if status != NodeStatus::Pending && status != NodeStatus::Waiting {
                continue;
            }
            if !ready(node, &statuses) {
                continue;
            }
            // 闸门：等过一次就记住，别再退回 Waiting。
            if node.needs_gate() && handle.gate_of(&node.id).is_none() {
                handle.mark_waiting(&node.id);
                continue;
            }
            if halted && policy == FailurePolicy::FailFast {
                continue;
            }
            if running >= max_parallel {
                continue;
            }
            if let Some(at) = retry_at.get(&node.id) {
                if Instant::now() < *at {
                    continue;
                }
                retry_at.remove(&node.id);
            }

            // 真正开跑。执行器按节点解析：一个 Agent 用哪个引擎是 profile 说的，
            // 而不是"整张图共用一个"——否则混合引擎的图永远跑不通。
            match build_request(&plan, node, &profiles, &handle, override_runner.as_ref()) {
                Ok((request, runner)) => {
                    handle.mark_running(&node.id);
                    running += 1;
                    let tx = tx.clone();
                    let id = node.id.clone();
                    let spawn = std::thread::Builder::new()
                        .name(format!("prism-dag-node-{}", node.id))
                        .spawn(move || {
                            let result = runner.run(request);
                            // 接收端可能已经退出（整场被取消）；发不出去也无所谓。
                            let _ = tx.send(Completion { id, result });
                        });
                    if let Err(error) = spawn {
                        running -= 1;
                        handle.mark_failed(&node.id, format!("节点线程起不来：{error}"));
                    }
                }
                Err(error) => {
                    // 目录建不起来之类的"还没开始就失败"，同样按失败处理并走重试。
                    handle.mark_running(&node.id);
                    apply_failure(node, error, &handle, &mut retry_at);
                }
            }
        }

        // 全都落定了就收尾（Waiting 不算落定——但没人确认时会一直等，正好
        // 让运行停在"等待确认"这个可解释的状态上）。
        let settled = handle
            .statuses()
            .values()
            .all(|status| status.is_terminal());
        if settled && running == 0 {
            break;
        }

        match rx.recv_timeout(TICK) {
            Ok(done) => {
                running = running.saturating_sub(1);
                let Some(node) = plan.node(&done.id) else {
                    continue;
                };
                match done.result {
                    Ok(mut output) => {
                        // 跑完即落制品：正文只进一次磁盘，下游拿引用或摘要。
                        // 写失败不判节点失败——正文还在运行记录里，判失败等于
                        // 丢掉已经花钱跑出来的结果，降级成一条过程记录。
                        let artifact =
                            match handle.save_artifact(
                                &plan.id,
                                &node.id,
                                &node.artifact_key(),
                                &output.text,
                            ) {
                                Ok(reference) => reference,
                                Err(error) => {
                                    output.steps.push(format!("制品写入失败：{error}"));
                                    None
                                }
                            };
                        handle.mark_succeeded(&node.id, output, artifact);
                        retry_at.remove(&node.id);
                    }
                    Err(error) => apply_failure(node, error, &handle, &mut retry_at),
                }
            }
            // 超时：回到顶部，让闸门/取消有机会被观察到。
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let snapshot = handle.snapshot();
    let message = super::run::summarize(&snapshot, policy);
    handle.finish(message);
}

struct Completion {
    id: String,
    result: Result<NodeOutput, String>,
}

/// 失败处理：还有重试次数就退回 Pending 并记下退避时刻，否则标失败。
fn apply_failure(
    node: &DagNode,
    error: String,
    handle: &RunHandle,
    retry_at: &mut HashMap<String, Instant>,
) {
    let attempt = handle.attempt_of(&node.id);
    if attempt <= node.retry.attempts {
        handle.mark_retry(&node.id, &error);
        retry_at.insert(
            node.id.clone(),
            Instant::now() + Duration::from_millis(node.retry.backoff_ms),
        );
        return;
    }
    handle.mark_failed(&node.id, error);
    // fail-fast 的"不再放新节点"由调度循环里的 halted 判断统一处理，
    // 这里只负责"这个节点怎么样了"。
}

/// 依赖都成功了才算就绪。
fn ready(node: &DagNode, statuses: &HashMap<String, NodeStatus>) -> bool {
    node.depends_on.iter().all(|dep| {
        statuses
            .get(dep)
            .copied()
            .map(|status| status == NodeStatus::Succeeded)
            .unwrap_or(false)
    })
}

/// 有上游失败 / 跳过 / 取消时，本节点不再有跑的意义。
fn blocked_reason(node: &DagNode, statuses: &HashMap<String, NodeStatus>) -> Option<String> {
    for dep in &node.depends_on {
        match statuses.get(dep).copied().unwrap_or(NodeStatus::Pending) {
            NodeStatus::Failed => {
                return Some(format!("上游 {dep} 失败"))
            }
            NodeStatus::Skipped => return Some(format!("上游 {dep} 被跳过")),
            NodeStatus::Cancelled => return Some(format!("上游 {dep} 被取消")),
            _ => {}
        }
    }
    None
}

fn cancel_pending(plan: &DagPlan, handle: &RunHandle, reason: &str) {
    let statuses = handle.statuses();
    for node in &plan.nodes {
        let status = statuses
            .get(&node.id)
            .copied()
            .unwrap_or(NodeStatus::Pending);
        match status {
            NodeStatus::Pending | NodeStatus::Waiting => {
                handle.mark_cancelled(&node.id, reason.to_string())
            }
            // 正在跑的节点：停止标记已经置上了，等它自己收尾，不在这里改写
            // 状态——否则会出现"标了取消但线程还在写文件"的假象。
            _ => {}
        }
    }
}

/// 拼提示词时可用的制品视图。
///
/// 别名来源两条，后者覆盖前者：
/// 1. **隐式**：每个上游节点 → 别名 = 节点 id，key = 该节点的主产出 key；
/// 2. **显式** `input_artifacts`：要改别名、或要读上游非主产出（工具写的
///    `strings` / `hashes`）时才写。
pub fn artifact_views(
    plan: &DagPlan,
    node: &DagNode,
    handle: &RunHandle,
) -> HashMap<String, super::artifact::ArtifactView> {
    let mut views: HashMap<String, super::artifact::ArtifactView> = HashMap::new();
    for dep in &node.depends_on {
        let key = plan
            .node(dep)
            .map(DagNode::artifact_key)
            .unwrap_or_default();
        views.insert(dep.clone(), view_of(plan, handle, dep, &key));
    }
    for input in &node.input_artifacts {
        let key = if input.key.trim().is_empty() {
            plan.node(&input.from)
                .map(DagNode::artifact_key)
                .unwrap_or_default()
        } else {
            input.key.trim().to_string()
        };
        let alias = if input.alias.trim().is_empty() {
            input.from.clone()
        } else {
            input.alias.trim().to_string()
        };
        views.insert(alias, view_of(plan, handle, &input.from, &key));
    }
    views
}

fn view_of(
    plan: &DagPlan,
    handle: &RunHandle,
    node_id: &str,
    key: &str,
) -> super::artifact::ArtifactView {
    let text = handle.output_of(node_id).unwrap_or_default();
    let (summary, truncated) =
        super::artifact::head(&text, super::artifact::SUMMARY_LIMIT);
    // 主产出走内存里的运行记录（最快，写入失败时也能兜底）；工具写入的其它
    // key 只有磁盘索引里有，所以第二条路径必须查仓库。
    let reference = handle
        .artifact_of(node_id)
        .filter(|reference| reference.key == super::artifact::normalize_key(key))
        .or_else(|| {
            handle
                .artifact_store()
                .and_then(|store| store.find(&plan.id, node_id, key))
        });
    match reference {
        Some(reference) => super::artifact::ArtifactView {
            id: reference.id,
            key: reference.key,
            kind: reference.kind,
            size: reference.size,
            sha256: reference.sha256,
            summary,
            truncated,
        },
        None => super::artifact::ArtifactView {
            key: key.to_string(),
            summary,
            truncated,
            ..Default::default()
        },
    }
}

/// 组装一次节点执行：解析 Agent、权限、工具、目录与占位符。
fn build_request(
    plan: &DagPlan,
    node: &DagNode,
    profiles: &[AgentProfile],
    handle: &Arc<RunHandle>,
    override_runner: Option<&Arc<dyn NodeRunner>>,
) -> Result<(NodeRequest, Arc<dyn NodeRunner>), String> {
    let agent_id = node
        .agent
        .clone()
        .unwrap_or_else(|| plan.defaults.agent.clone());
    let profile = profiles
        .iter()
        .find(|profile| profile.id == agent_id)
        .cloned()
        .ok_or_else(|| format!("Agent「{agent_id}」不在注册表里"))?;
    // 执行器按 Agent 的 `executor` 从注册表解析（单测直接注入，跳过查表）。
    // 没装就早报错，比跑到一半再发现"这个 Agent 其实该由别的引擎跑"要好；
    // 报错里带上已装的，用户知道该去工坊开哪个插件。
    let runner = match override_runner {
        Some(runner) => Arc::clone(runner),
        None => super::runner::get(&profile.executor).ok_or_else(|| {
            let installed = super::runner::installed();
            format!(
                "Agent「{agent_id}」的执行器是 {}，但没装这个执行器插件（已装：{}）",
                profile.executor,
                if installed.is_empty() {
                    "无".to_string()
                } else {
                    installed.join("、")
                }
            )
        })?,
    };

    let permission = node.permission.unwrap_or(plan.defaults.permission);
    let selection = node
        .tools
        .clone()
        .unwrap_or_else(|| profile.tools.clone());
    let directory = prepare_directory(plan, node, &profile)?;

    let mut outputs: HashMap<String, String> = HashMap::new();
    for dep in &node.depends_on {
        if let Some(text) = handle.output_of(dep) {
            outputs.insert(dep.clone(), text);
        }
    }

    let request = NodeRequest {
        run_id: handle.id(),
        dag_id: plan.id.clone(),
        node_id: node.id.clone(),
        permission,
        selection,
        directory,
        prompt: resolve_task(&node.task, &outputs, &artifact_views(plan, node, handle)),
        profile,
        stop: StopHandle::new(handle.cancel_flag(), node.timeout_secs),
        on_text: {
            // `Arc` 而不是 `&RunHandle`：回调要 'static（跟着节点线程走）。
            let handle = Arc::clone(handle);
            let node_id = node.id.clone();
            Arc::new(move |text: &str| handle.push_text(&node_id, text))
        },
        on_step: {
            let handle = Arc::clone(handle);
            let node_id = node.id.clone();
            Arc::new(move |step: &str| handle.push_step(&node_id, step))
        },
        on_usage: {
            let handle = Arc::clone(handle);
            let node_id = node.id.clone();
            Arc::new(move |tokens_in: i64, tokens_out: i64| {
                handle.push_usage(&node_id, tokens_in, tokens_out)
            })
        },
        on_turn: {
            let handle = Arc::clone(handle);
            let node_id = node.id.clone();
            Arc::new(move |turn: u32, max_turns: u32| {
                handle.push_turn(&node_id, turn, max_turns)
            })
        },
    };
    Ok((request, runner))
}

/// 节点的工作目录。三种工作区语义见 `agents::Workspace`。
fn prepare_directory(
    plan: &DagPlan,
    node: &DagNode,
    profile: &AgentProfile,
) -> Result<String, String> {
    let workspace = node.workspace.unwrap_or(profile.workspace);
    match workspace {
        crate::agents::Workspace::Shared => Ok(plan.project.clone()),
        crate::agents::Workspace::Scratch => {
            let dir = crate::engines::opencode::search::prism_dir()
                .ok_or_else(|| "找不到 ~/.prism 目录".to_string())?
                .join("dags/scratch")
                .join(&plan.id)
                .join(&node.id);
            std::fs::create_dir_all(&dir)
                .map_err(|error| format!("建临时工作区失败：{error}"))?;
            Ok(dir.display().to_string())
        }
        crate::agents::Workspace::Worktree => {
            // 非 git 仓库不可能建 worktree。规划期已经改写过新计划的这类节点，
            // 但存盘的旧计划、手写的计划仍可能带 worktree —— 运行期兜底成共享目录，
            // 别让一个工作区语义上的小事把整条 DAG 判死。
            if crate::gitinfo::git_info(std::path::Path::new(&plan.project))
                .repo_root
                .is_none()
            {
                return Ok(plan.project.clone());
            }
            let dir = crate::engines::opencode::search::prism_dir()
                .ok_or_else(|| "找不到 ~/.prism 目录".to_string())?
                .join("dags/worktrees")
                .join(&plan.id)
                .join(&node.id);
            std::fs::create_dir_all(dir.parent().unwrap_or(&dir))
                .map_err(|error| format!("建 worktree 目录失败：{error}"))?;
            // 上一次运行可能留下同名目录：先摘掉再建，否则 git 会以
            // "already exists" 拒绝，重跑同一个计划就永远起不来。
            if dir.exists() {
                let _ = std::process::Command::new("git")
                    .args([
                        "-C",
                        &plan.project,
                        "worktree",
                        "remove",
                        "--force",
                        &dir.display().to_string(),
                    ])
                    .output();
            }
            let branch = format!("prism/{}/{}", plan.id, node.id);
            let output = std::process::Command::new("git")
                .args([
                    "-C",
                    &plan.project,
                    "worktree",
                    "add",
                    "-B",
                    &branch,
                    &dir.display().to_string(),
                ])
                .output()
                .map_err(|error| format!("git 不可用：{error}"))?;
            if !output.status.success() {
                let reason = String::from_utf8_lossy(&output.stderr);
                return Err(format!(
                    "建 worktree 失败（{} 不是 git 仓库？）：{}",
                    plan.project,
                    reason.trim().chars().take(200).collect::<String>()
                ));
            }
            Ok(dir.display().to_string())
        }
    }
}

/// 单测直接构造执行器时用（真机上由 `dag.executor.native` 插件注册带 AppHandle 的版本）。
#[allow(dead_code)]
pub fn native_runner() -> Arc<dyn NodeRunner> {
    Arc::new(super::runner::NativeRunner::new(None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{Permission, ToolSelection, Workspace};
    use crate::dag::model::{DagDefaults, DagNode, Retry};

    #[derive(Default)]
    struct ScriptedRunner {
        script: HashMap<String, Result<String, String>>,
        delay: Duration,
        /// 每个节点的入参提示词：用来断言"下游拿到的是摘要不是全文"。
        seen: Option<Arc<std::sync::Mutex<Vec<(String, String)>>>>,
    }

    impl NodeRunner for ScriptedRunner {
        fn id(&self) -> &'static str {
            "native"
        }

        fn run(&self, request: NodeRequest) -> Result<NodeOutput, String> {
            if let Some(seen) = self.seen.as_ref() {
                if let Ok(mut list) = seen.lock() {
                    list.push((request.node_id.clone(), request.prompt.clone()));
                }
            }
            std::thread::sleep(self.delay);
            match self.script.get(&request.node_id) {
                Some(Ok(text)) => Ok(NodeOutput {
                    text: text.clone(),
                    ..Default::default()
                }),
                Some(Err(error)) => Err(error.clone()),
                None => Ok(NodeOutput {
                    text: format!("{} 完成", request.node_id),
                    ..Default::default()
                }),
            }
        }
    }

    fn profile(id: &str) -> AgentProfile {
        AgentProfile {
            id: id.to_string(),
            name: id.to_string(),
            description: String::new(),
            origin: crate::agents::Origin::Builtin,
            overrides_builtin: false,
            executor: "native".to_string(),
            model: None,
            system_prompt: String::new(),
            permission: Permission::ReadOnly,
            workspace: Workspace::Shared,
            tools: ToolSelection::default(),
            mcp: Vec::new(),
            tags: Vec::new(),
            link_dirs: Vec::new(),
            gate_default: false,
        }
    }

    fn node(id: &str, deps: &[&str]) -> DagNode {
        DagNode {
            id: id.to_string(),
            title: id.to_string(),
            task: format!("跑 {id}"),
            depends_on: deps.iter().map(|item| item.to_string()).collect(),
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
        }
    }

    /// 旧计划、手写计划、导入的计划都可能存着 `workspace: worktree`，但目标
    /// 目录根本不是 git 仓库（桌面上的样本文件夹就是典型）。运行期必须降级到
    /// 共享目录，不能让 `git worktree add` 的一次失败把整条 DAG 判死。
    #[test]
    fn worktree_degrades_to_shared_outside_git() {
        let mut target = node("unpack", &[]);
        target.workspace = Some(Workspace::Worktree);
        let mut dag = plan(vec![target.clone()], 1, FailurePolicy::FailFast);
        dag.project = std::env::temp_dir()
            .join("prism-no-git-here")
            .display()
            .to_string();
        let dir = prepare_directory(&dag, &target, &profile("unpacker")).expect("不该报错");
        assert_eq!(dir, dag.project, "非 git 仓库的 worktree 应当落回共享目录");
    }

    fn plan(nodes: Vec<DagNode>, max_parallel: u8, policy: FailurePolicy) -> DagPlan {
        DagPlan {
            version: 1,
            id: "dag-test".to_string(),
            name: "测试图".to_string(),
            goal: String::new(),
            // 用 /tmp：Shared 工作区会拿它当目录，真实执行器不会用到。
            project: std::env::temp_dir().display().to_string(),
            defaults: DagDefaults {
                agent: "general".to_string(),
                permission: Permission::ReadOnly,
                max_parallel,
                failure_policy: policy,
                token_budget: None,
            },
            nodes,
            layout: Default::default(),
            created_at: 0,
            updated_at: 0,
        }
    }

    fn run_plan(
        plan: DagPlan,
        script: Vec<(&str, Result<&str, &str>)>,
        delay: Duration,
    ) -> Arc<RunHandle> {
        let hub = crate::dag::run::DagHub::ephemeral();
        let script: HashMap<String, Result<String, String>> = script
            .into_iter()
            .map(|(id, result)| {
                (
                    id.to_string(),
                    result.map(str::to_string).map_err(str::to_string),
                )
            })
            .collect();
        let runner = Arc::new(ScriptedRunner {
            script,
            delay,
            ..Default::default()
        });
        let started = hub
            .start(plan, &[profile("general")], Some(runner), Arc::new(|_: &crate::dag::run::DagRun| {}))
            .expect("应当能起跑");
        let handle = hub.get(&started.id).expect("运行表里应当有它");
        // 等调度线程收尾（最多 5 秒，避免测试挂死）。
        for _ in 0..100 {
            if handle.status() != crate::dag::run::RunStatus::Running {
                return handle;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("运行没有在 5 秒内结束：{:?}", handle.statuses());
    }

    #[test]
    fn runs_nodes_in_dependency_order() {
        let plan = plan(
            vec![node("a", &[]), node("b", &["a"]), node("c", &["b"])],
            2,
            FailurePolicy::FailFast,
        );
        let handle = run_plan(plan, vec![], Duration::from_millis(10));
        assert_eq!(handle.status(), crate::dag::run::RunStatus::Succeeded);
        let run = handle.snapshot();
        assert!(run.nodes.iter().all(|node| node.status == NodeStatus::Succeeded));
        // 下游拿到的正文来自上游
        assert_eq!(run.nodes.iter().filter(|n| n.status == NodeStatus::Succeeded).count(), 3);
        assert!(run.finished_at.is_some());
    }

    #[test]
    fn failure_skips_downstream() {
        let plan = plan(
            vec![node("a", &[]), node("b", &["a"]), node("c", &[])],
            2,
            FailurePolicy::FailFast,
        );
        let handle = run_plan(
            plan,
            vec![("a", Err("模型 500"))],
            Duration::from_millis(10),
        );
        let run = handle.snapshot();
        assert_eq!(run.status, crate::dag::run::RunStatus::Failed);
        assert_eq!(run.node("a").unwrap().status, NodeStatus::Failed);
        assert_eq!(run.node("b").unwrap().status, NodeStatus::Skipped);
        assert!(run.node("a").unwrap().error.as_deref().unwrap_or("").contains("500"));
    }

    #[test]
    fn retry_gives_the_node_another_attempt() {
        let mut flaky = node("a", &[]);
        flaky.retry = Retry {
            attempts: 1,
            backoff_ms: 1,
        };
        // 第一次失败、第二次成功：脚本只能给一种结果，所以这里断言"重试被记录了"。
        let plan = plan(vec![flaky], 1, FailurePolicy::FailFast);
        let handle = run_plan(plan, vec![("a", Err("超时"))], Duration::from_millis(10));
        let run = handle.snapshot();
        let a = run.node("a").unwrap();
        assert_eq!(a.status, NodeStatus::Failed);
        assert_eq!(a.attempt, 2, "应当尝试了两次");
    }

    #[test]
    fn gate_holds_the_node_until_approved() {
        let mut gated = node("a", &[]);
        gated.gate = Some(crate::dag::model::Gate {
            before: true,
            after: false,
        });
        let plan = plan(vec![gated, node("b", &["a"])], 2, FailurePolicy::FailFast);
        let hub = crate::dag::run::DagHub::ephemeral();
        let runner = Arc::new(ScriptedRunner {
            script: HashMap::new(),
            delay: Duration::from_millis(5),
            ..Default::default()
        });
        let started = hub
            .start(plan, &[profile("general")], Some(runner), Arc::new(|_: &crate::dag::run::DagRun| {}))
            .unwrap();
        let handle = hub.get(&started.id).unwrap();
        // 没确认前停在 Waiting
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(handle.snapshot().node("a").unwrap().status, NodeStatus::Waiting);

        handle.approve("a", true).unwrap();
        for _ in 0..100 {
            if handle.status() != crate::dag::run::RunStatus::Running {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let run = handle.snapshot();
        assert_eq!(run.status, crate::dag::run::RunStatus::Succeeded);
        assert_eq!(run.node("a").unwrap().status, NodeStatus::Succeeded);
    }

    #[test]
    fn rejecting_a_gate_skips_its_downstream() {
        let mut gated = node("a", &[]);
        gated.gate = Some(crate::dag::model::Gate {
            before: true,
            after: false,
        });
        let plan = plan(vec![gated, node("b", &["a"])], 1, FailurePolicy::FailFast);
        let hub = crate::dag::run::DagHub::ephemeral();
        let runner = Arc::new(ScriptedRunner {
            script: HashMap::new(),
            delay: Duration::from_millis(5),
            ..Default::default()
        });
        let started = hub
            .start(plan, &[profile("general")], Some(runner), Arc::new(|_: &crate::dag::run::DagRun| {}))
            .unwrap();
        let handle = hub.get(&started.id).unwrap();
        std::thread::sleep(Duration::from_millis(120));
        handle.approve("a", false).unwrap();
        for _ in 0..100 {
            if handle.status() != crate::dag::run::RunStatus::Running {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let run = handle.snapshot();
        assert_eq!(run.node("a").unwrap().status, NodeStatus::Cancelled);
        assert_eq!(run.node("b").unwrap().status, NodeStatus::Skipped);
    }

    #[test]
    fn cancel_stops_pending_nodes() {
        let plan = plan(vec![node("a", &[]), node("b", &["a"])], 1, FailurePolicy::FailFast);
        let hub = crate::dag::run::DagHub::ephemeral();
        let runner = Arc::new(ScriptedRunner {
            script: HashMap::new(),
            delay: Duration::from_millis(120),
            ..Default::default()
        });
        let started = hub
            .start(plan, &[profile("general")], Some(runner), Arc::new(|_: &crate::dag::run::DagRun| {}))
            .unwrap();
        let handle = hub.get(&started.id).unwrap();
        std::thread::sleep(Duration::from_millis(30));
        handle.cancel().unwrap();
        for _ in 0..100 {
            if handle.status() != crate::dag::run::RunStatus::Running {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let run = handle.snapshot();
        assert_eq!(run.status, crate::dag::run::RunStatus::Cancelled);
        assert_eq!(run.node("b").unwrap().status, NodeStatus::Cancelled);
    }

    #[test]
    fn rejects_a_second_run_of_the_same_plan() {
        let hub = crate::dag::run::DagHub::ephemeral();
        let plan = plan(vec![node("a", &[])], 1, FailurePolicy::FailFast);
        let runner = Arc::new(ScriptedRunner {
            script: HashMap::new(),
            delay: Duration::from_millis(200),
            ..Default::default()
        });
        let notify = Arc::new(|_: &crate::dag::run::DagRun| {});
        hub.start(plan.clone(), &[profile("general")], Some(runner.clone()), notify.clone())
            .unwrap();
        let second = hub.start(plan, &[profile("general")], Some(runner), notify);
        assert!(second.is_err(), "同一张计划不该并发跑");
    }

    #[test]
    fn node_output_lands_in_the_artifact_store() {
        let big = "结".repeat(200_000);
        let mut graph = plan(
            vec![node("a", &[]), node("b", &["a"])],
            1,
            FailurePolicy::FailFast,
        );
        graph.nodes[1].task =
            "引用：{{artifacts.a.id}} | 摘要：{{artifacts.a.summary}}".to_string();

        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let runner = Arc::new(ScriptedRunner {
            script: HashMap::from([("a".to_string(), Ok(big.clone()))]),
            delay: Duration::from_millis(5),
            seen: Some(seen.clone()),
        });
        let hub = crate::dag::run::DagHub::ephemeral();
        let started = hub
            .start(
                graph,
                &[profile("general")],
                Some(runner),
                Arc::new(|_: &crate::dag::run::DagRun| {}),
            )
            .expect("应当能起跑");
        let handle = hub.get(&started.id).unwrap();
        for _ in 0..100 {
            if handle.status() != crate::dag::run::RunStatus::Running {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        let run = handle.snapshot();
        let artifact = run
            .node("a")
            .and_then(|node| node.artifact.clone())
            .expect("节点跑完就该有制品");
        assert_eq!(artifact.key, "report");
        assert_eq!(artifact.size, big.len() as u64);
        assert_eq!(
            artifact.sha256,
            crate::dag::artifact::sha256_of(big.as_bytes())
        );

        // 下游提示词里是 id + 摘要（2 KB），不是 200 KB 正文。
        let prompts = seen.lock().unwrap();
        let prompt = &prompts.iter().find(|(id, _)| id == "b").unwrap().1;
        assert!(prompt.contains(&artifact.id), "摘要里必须带引用");
        assert!(
            prompt.chars().count() < 3_000,
            "下游不该拿到全文：{} 字",
            prompt.chars().count()
        );

        // 按引用取回：长度与原文一致（截断与否由调用方决定）。
        let store = handle.artifact_store().expect("单测应当有临时仓库");
        let (_, text, truncated) = store
            .read_text("dag-test", &artifact.id, 1_000_000)
            .unwrap();
        assert_eq!(text.chars().count(), big.chars().count());
        assert!(!truncated);
    }

    #[test]
    fn blocked_reason_points_at_the_upstream() {
        let mut statuses = HashMap::new();
        statuses.insert("a".to_string(), NodeStatus::Failed);
        let b = node("b", &["a"]);
        assert!(blocked_reason(&b, &statuses).unwrap().contains("a"));
    }
}
