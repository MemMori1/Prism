//! 一次运行的状态与生命周期。
//!
//! 计划是**静态**的（`plan.rs` 那边的 `DagPlan`），这里是它的**动态影子**：
//! 哪个节点在跑、跑了几次、产出什么、为什么失败。
//!
//! 几条硬约定
//! ----------
//! - 一张计划同一时刻只允许一次运行：并发跑同一张图会互相踩工作区，
//!   而且"改图"和"改运行结果"会分不清谁对。
//! - 状态变化立刻落盘（`~/.prism/dags/runs/<run_id>.json`）：应用重启后
//!   还能看见上一次跑到哪儿，而不是只剩一个「未运行」。
//! - 状态机只在这里改。执行器（`executor`）只调这些方法，不直接摸锁，
//!   免得到处都是 `Mutex` 与"忘了发事件"。
//!
//! 事件与锁：句柄**不持有** `AppHandle`（会形成 `App -> State -> App` 的引用环），
//! 只持有一个 `notify` 回调；由 `start` 在构造时注入。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::model::{DagPlan, FailurePolicy};
use super::runner::{NodeOutput, NodeRunner};

/// 前端监听的事件名（载荷是整份 [`DagRun`] 快照）。
///
/// 推全量而不是增量：节点只有十几个，一份快照几十 KB 上限，换来的是前端
/// 不需要维护"补丁合并"这套逻辑——少一类状态不同步的 bug。
pub const EVENT_NAME: &str = "dag.run";

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeStatus {
    /// 还没轮到（依赖没齐或并发满了）。
    Pending,
    /// 依赖齐了，等人工确认（`gate.before`）。
    Waiting,
    Running,
    Succeeded,
    Failed,
    /// 上游没成功，或者被人工拒绝。**不是**错误——是没跑。
    Skipped,
    Cancelled,
}

impl NodeStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            NodeStatus::Succeeded
                | NodeStatus::Failed
                | NodeStatus::Skipped
                | NodeStatus::Cancelled
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

/// 一个节点的运行记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeRun {
    pub id: String,
    pub title: String,
    /// 实际指派的 Agent（`None` 已解析成计划默认值）。
    pub agent: String,
    pub status: NodeStatus,
    /// 第几次尝试（从 1 开始；0 表示还没跑过）。
    pub attempt: u8,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    /// 成功时的报告正文（失败时保留最后一次的部分）
    pub output: String,
    pub error: Option<String>,
    pub tokens_in: i64,
    pub tokens_out: i64,
    /// 工具调用摘要（`read_file src/x.rs` 这种），给运行面板做过程感。
    pub steps: Vec<String>,
}

/// 一次运行。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DagRun {
    pub id: String,
    pub plan_id: String,
    pub plan_name: String,
    pub project: String,
    pub status: RunStatus,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    /// 收尾说明：为什么失败 / 哪些被跳过。
    pub message: Option<String>,
    pub nodes: Vec<NodeRun>,
}

impl DagRun {
    fn build(plan: &DagPlan, id: String) -> DagRun {
        DagRun {
            id,
            plan_id: plan.id.clone(),
            plan_name: plan.name.clone(),
            project: plan.project.clone(),
            status: RunStatus::Running,
            started_at: now_ms(),
            finished_at: None,
            message: None,
            nodes: plan
                .nodes
                .iter()
                .map(|node| NodeRun {
                    id: node.id.clone(),
                    title: node.title.clone(),
                    agent: node.agent.clone().unwrap_or_else(|| plan.defaults.agent.clone()),
                    status: NodeStatus::Pending,
                    attempt: 0,
                    started_at: None,
                    finished_at: None,
                    output: String::new(),
                    error: None,
                    tokens_in: 0,
                    tokens_out: 0,
                    steps: Vec::new(),
                })
                .collect(),
        }
    }

    pub fn node(&self, id: &str) -> Option<&NodeRun> {
        self.nodes.iter().find(|node| node.id == id)
    }
}

// ---------------------------------------------------------------- 句柄

pub type Notifier = Arc<dyn Fn(&DagRun) + Send + Sync>;

/// 一次运行的共享句柄：执行器、IPC 命令、事件推送都看这一个。
pub struct RunHandle {
    state: Mutex<DagRun>,
    /// 取消标记与执行器共享：节点的 `should_stop` 直接读这个 `Arc`。
    cancelled: Arc<AtomicBool>,
    /// 人工闸门的决定：`true` 放行、`false` 拒绝、缺键＝还没人点。
    gates: Mutex<HashMap<String, bool>>,
    /// 落盘失败只报一次，别把 stderr 刷满。
    warned: AtomicBool,
    notify: Notifier,
    /// 是否把记录写到 `~/.prism/dags/runs/`。单测用临时/内存模式，
    /// 否则跑一次测试就往用户的真实目录里丢一堆假运行记录。
    persist: bool,
}

impl RunHandle {
    fn new(run: DagRun, notify: Notifier, persist: bool) -> RunHandle {
        RunHandle {
            state: Mutex::new(run),
            cancelled: Arc::new(AtomicBool::new(false)),
            gates: Mutex::new(HashMap::new()),
            warned: AtomicBool::new(false),
            notify,
            persist,
        }
    }

    pub fn id(&self) -> String {
        self.state
            .lock()
            .map(|run| run.id.clone())
            .unwrap_or_default()
    }

    pub fn snapshot(&self) -> DagRun {
        self.state
            .lock()
            .map(|run| run.clone())
            .unwrap_or_else(|_| DagRun {
                id: String::new(),
                plan_id: String::new(),
                plan_name: String::new(),
                project: String::new(),
                status: RunStatus::Cancelled,
                started_at: 0,
                finished_at: Some(0),
                message: Some("运行状态被占用（内部错误）".to_string()),
                nodes: Vec::new(),
            })
    }

    pub fn status(&self) -> RunStatus {
        self.state
            .lock()
            .map(|run| run.status)
            .unwrap_or(RunStatus::Cancelled)
    }

    pub fn statuses(&self) -> HashMap<String, NodeStatus> {
        self.state
            .lock()
            .map(|run| {
                run.nodes
                    .iter()
                    .map(|node| (node.id.clone(), node.status))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn attempt_of(&self, node_id: &str) -> u8 {
        self.state
            .lock()
            .ok()
            .and_then(|run| run.node(node_id).map(|node| node.attempt))
            .unwrap_or(0)
    }

    pub fn output_of(&self, node_id: &str) -> Option<String> {
        self.state
            .lock()
            .ok()
            .and_then(|run| run.node(node_id).map(|node| node.output.clone()))
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// 取消标记的共享句柄（执行器把它塞进 `StopHandle`）。
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancelled.clone()
    }

    /// 闸门决定：`None` 还没人点。
    pub fn gate_of(&self, node_id: &str) -> Option<bool> {
        self.gates.lock().ok().and_then(|gates| gates.get(node_id).copied())
    }

    pub fn approve(&self, node_id: &str, allowed: bool) -> Result<DagRun, String> {
        {
            let mut gates = self.gates.lock().map_err(|_| "闸门状态被占用".to_string())?;
            gates.insert(node_id.to_string(), allowed);
        }
        Ok(self.snapshot())
    }

    pub fn cancel(&self) -> Result<DagRun, String> {
        self.cancelled.store(true, Ordering::SeqCst);
        // 等在闸门上的节点不会自己醒，直接把决定写进去让它走取消分支。
        if let Ok(mut gates) = self.gates.lock() {
            for (node, _) in gates.clone() {
                gates.insert(node, false);
            }
        }
        Ok(self.snapshot())
    }

    /// 改写一个节点并对外广播。`finish=false` 时不落盘（状态太密，没必要）。
    fn update(&self, node_id: &str, edit: impl FnOnce(&mut NodeRun), persist: bool) {
        let snapshot = {
            let Ok(mut run) = self.state.lock() else {
                return;
            };
            let Some(node) = run.nodes.iter_mut().find(|node| node.id == node_id) else {
                return;
            };
            edit(node);
            run.clone()
        };
        if persist {
            self.save(&snapshot);
        }
        (self.notify)(&snapshot);
    }

    pub fn mark_waiting(&self, node_id: &str) {
        self.update(node_id, |node| node.status = NodeStatus::Waiting, true);
    }

    pub fn mark_running(&self, node_id: &str) {
        self.update(
            node_id,
            |node| {
                node.status = NodeStatus::Running;
                node.attempt += 1;
                node.started_at = Some(now_ms());
                node.finished_at = None;
                node.error = None;
                node.output.clear();
                node.steps.clear();
            },
            true,
        );
    }

    /// 退回待跑（重试用）。保留 attempt，清掉上一次的产出。
    pub fn mark_retry(&self, node_id: &str, reason: &str) {
        self.update(
            node_id,
            |node| {
                node.status = NodeStatus::Pending;
                node.error = Some(format!("第 {} 次失败，准备重试：{reason}", node.attempt));
            },
            true,
        );
    }

    pub fn mark_failed(&self, node_id: &str, error: String) {
        self.update(
            node_id,
            |node| {
                node.status = NodeStatus::Failed;
                node.finished_at = Some(now_ms());
                node.error = Some(error);
            },
            true,
        );
    }

    pub fn mark_succeeded(&self, node_id: &str, output: NodeOutput) {
        self.update(
            node_id,
            |node| {
                node.status = NodeStatus::Succeeded;
                node.finished_at = Some(now_ms());
                node.output = output.text;
                node.tokens_in += output.tokens_in;
                node.tokens_out += output.tokens_out;
                node.steps = output.steps;
                node.error = None;
            },
            true,
        );
    }

    pub fn mark_skipped(&self, node_id: &str, reason: String) {
        self.update(
            node_id,
            |node| {
                node.status = NodeStatus::Skipped;
                node.finished_at = Some(now_ms());
                node.error = Some(reason);
            },
            true,
        );
    }

    pub fn mark_cancelled(&self, node_id: &str, reason: String) {
        self.update(
            node_id,
            |node| {
                node.status = NodeStatus::Cancelled;
                node.finished_at = Some(now_ms());
                node.error = Some(reason);
            },
            true,
        );
    }

    /// 收尾：算总状态 + 写结束时间。
    pub fn finish(&self, message: Option<String>) {
        let snapshot = {
            let Ok(mut run) = self.state.lock() else {
                return;
            };
            let cancelled = self.is_cancelled();
            let failed = run.nodes.iter().any(|node| node.status == NodeStatus::Failed);
            run.status = if cancelled {
                RunStatus::Cancelled
            } else if failed {
                RunStatus::Failed
            } else {
                RunStatus::Succeeded
            };
            run.finished_at = Some(now_ms());
            run.message = message;
            run.clone()
        };
        self.save(&snapshot);
        (self.notify)(&snapshot);
    }

    /// 流式正文推进（不落盘、不广播——太密）。
    ///
    /// 只更新内存里的 `output`，让下一次状态事件顺带把它带出去；前端靠
    /// `dag.node.delta` 单独直播，不依赖这里。
    pub fn push_text(&self, node_id: &str, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Ok(mut run) = self.state.lock() {
            if let Some(node) = run.nodes.iter_mut().find(|node| node.id == node_id) {
                node.output.push_str(text);
            }
        }
    }

    fn save(&self, run: &DagRun) {
        if !self.persist {
            return;
        }
        if let Err(error) = super::store::save_run(run) {
            if !self.warned.swap(true, Ordering::Relaxed) {
                eprintln!("[prism] 运行记录落盘失败：{error}");
            }
        }
    }
}

// ---------------------------------------------------------------- 运行表

/// 进程内的运行表：`run_id -> 句柄`，外加「每张计划最近一次运行」的索引。
pub struct DagHub {
    runs: Mutex<HashMap<String, Arc<RunHandle>>>,
    latest: Mutex<HashMap<String, String>>,
    /// 是否落盘。见 [`RunHandle`] 里同名字段的说明。
    persist: bool,
}

impl Default for DagHub {
    fn default() -> Self {
        DagHub {
            runs: Mutex::new(HashMap::new()),
            latest: Mutex::new(HashMap::new()),
            persist: true,
        }
    }
}

impl DagHub {
    /// 只在内存里跑（单测用）：不写 `~/.prism`。
    #[cfg(test)]
    pub fn ephemeral() -> Self {
        DagHub {
            persist: false,
            ..DagHub::default()
        }
    }

    /// 起一次运行。校验不通过就不起（返回人能看懂的原因）。
    ///
    /// 执行线程在这里就分出去：调用方（IPC）只等"已经开跑"这一件事，
    /// 不能等整张图跑完——那可能是几十分钟。
    pub fn start(
        &self,
        plan: DagPlan,
        agents: &[crate::agents::AgentProfile],
        // 单测注入的假执行器；真机传 None，由执行器注册表按 Agent 解析。
        runner: Option<Arc<dyn NodeRunner>>,
        notify: Notifier,
    ) -> Result<DagRun, String> {
        let known: Vec<String> = agents.iter().map(|agent| agent.id.clone()).collect();
        let issues = super::validate::validate(&plan, &known);
        let errors: Vec<String> = issues
            .iter()
            .filter(|issue| issue.level == super::validate::Level::Error)
            .map(|issue| {
                let who = issue.node_id.as_deref().unwrap_or("计划");
                format!("{who}：{}", issue.message)
            })
            .collect();
        if !errors.is_empty() {
            return Err(format!("这张图还不能跑：{}", errors.join("；")));
        }

        if let Some(previous) = self.latest_handle(&plan.id) {
            if previous.status() == RunStatus::Running {
                return Err("这张计划还在跑：先停止，或等它结束".to_string());
            }
        }

        let run = DagRun::build(&plan, new_run_id(&plan.id));
        let handle = Arc::new(RunHandle::new(run.clone(), notify.clone(), self.persist));
        {
            let mut runs = self.runs.lock().map_err(|_| "运行表被占用".to_string())?;
            runs.insert(run.id.clone(), handle.clone());
        }
        {
            let mut latest = self.latest.lock().map_err(|_| "运行表被占用".to_string())?;
            latest.insert(plan.id.clone(), run.id.clone());
        }
        handle.save(&run);
        notify(&run);

        let worker = handle.clone();
        // 先把 Agent 目录拍成 Vec：`agents` 是借用，线程要有自己的所有权。
        let profiles = agents.to_vec();
        let spawn = std::thread::Builder::new()
            .name(format!("prism-dag-{}", run.id))
            .spawn(move || {
                super::executor::execute(plan, profiles, runner, worker);
            });
        if let Err(error) = spawn {
            handle.mark_failed_all(format!("运行线程起不来：{error}"));
            return Err(format!("运行线程起不来：{error}"));
        }
        Ok(run)
    }

    pub fn get(&self, run_id: &str) -> Option<Arc<RunHandle>> {
        self.runs
            .lock()
            .ok()
            .and_then(|runs| runs.get(run_id).cloned())
    }

    fn latest_handle(&self, plan_id: &str) -> Option<Arc<RunHandle>> {
        let id = self
            .latest
            .lock()
            .ok()
            .and_then(|latest| latest.get(plan_id).cloned())?;
        self.get(&id)
    }

    /// 某张计划最近一次运行（内存里没有就从磁盘捡回来）。
    pub fn latest_run(&self, plan_id: &str) -> Option<DagRun> {
        if let Some(handle) = self.latest_handle(plan_id) {
            return Some(handle.snapshot());
        }
        super::store::latest_run_of(plan_id).ok().flatten()
    }

    /// 按 id 取运行：内存优先，其次磁盘（进程重启后仍能看历史）。
    pub fn run_by_id(&self, run_id: &str) -> Option<DagRun> {
        if let Some(handle) = self.get(run_id) {
            return Some(handle.snapshot());
        }
        super::store::load_run(run_id).ok()
    }
}

impl RunHandle {
    /// 线程都起不来时把整场标成失败（不能留一个永远 Running 的记录）。
    fn mark_failed_all(&self, error: String) {
        let snapshot = {
            let Ok(mut run) = self.state.lock() else {
                return;
            };
            for node in run.nodes.iter_mut() {
                if !node.status.is_terminal() {
                    node.status = NodeStatus::Failed;
                    node.finished_at = Some(now_ms());
                    node.error = Some(error.clone());
                }
            }
            run.status = RunStatus::Failed;
            run.finished_at = Some(now_ms());
            run.message = Some(error);
            run.clone()
        };
        self.save(&snapshot);
        (self.notify)(&snapshot);
    }
}

/// run id：`run-<plan>-<毫秒>-<序号>`。计划 id 已受 `store::check_id` 约束。
pub fn new_run_id(plan_id: &str) -> String {
    use std::sync::atomic::AtomicU32;
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed) % 1000;
    format!("run-{plan_id}-{}-{seq:03}", now_ms())
}

/// runs 目录（需要时创建）。
pub fn runs_dir() -> Option<PathBuf> {
    let dir = crate::engines::opencode::search::prism_dir()?.join("dags/runs");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// 收尾说明：把失败 / 跳过的节点摘成一句话。
pub fn summarize(run: &DagRun, policy: FailurePolicy) -> Option<String> {
    let failed: Vec<&str> = run
        .nodes
        .iter()
        .filter(|node| node.status == NodeStatus::Failed)
        .map(|node| node.id.as_str())
        .collect();
    let skipped: usize = run
        .nodes
        .iter()
        .filter(|node| node.status == NodeStatus::Skipped)
        .count();
    let cancelled: usize = run
        .nodes
        .iter()
        .filter(|node| node.status == NodeStatus::Cancelled)
        .count();
    if failed.is_empty() && skipped == 0 && cancelled == 0 {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    if !failed.is_empty() {
        parts.push(format!("失败：{}", failed.join("、")));
    }
    if skipped > 0 {
        parts.push(format!(
            "{skipped} 个节点被跳过（{}）",
            match policy {
                FailurePolicy::FailFast => "fail-fast",
                FailurePolicy::Continue => "上游没成功",
            }
        ));
    }
    if cancelled > 0 {
        parts.push(format!("{cancelled} 个节点被取消"));
    }
    Some(parts.join("；"))
}
