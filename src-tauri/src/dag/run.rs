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

/// 节点运行中的增量事件名（正文增量 + 新的一步工具调用）。
///
/// 为什么不复用 `dag.run`：快照是整份运行记录（几个节点几万字），正文一秒能
/// 吐几十个 delta，用快照推会把 IPC 与前端重渲染打满。增量单独走这一条，
/// 前端按 `nodeId` 叠在本地视图上；节点落定时快照会带完整正文，增量随之丢弃。
pub const LIVE_EVENT_NAME: &str = "dag.node.live";

/// 正文增量攒多久发一次。120ms 肉眼仍像逐字出现，同时把事件量压掉一个量级。
const LIVE_FLUSH: std::time::Duration = std::time::Duration::from_millis(120);

/// 单节点保留的步数上限（与执行器侧一致：过程是"感"，不是流水账）。
const LIVE_STEPS: usize = 24;

/// 一条增量。三个可选字段都可能为空：新一步会顺带把攒着的正文带出去。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeLive {
    pub run_id: String,
    pub node_id: String,
    /// 正文增量（攒了 `LIVE_FLUSH` 一起发）。
    pub delta: String,
    /// 新落的一步工具调用（`read_file src/x.rs`）。
    pub step: Option<String>,
    /// 到现在为止本节点累计的 token（跨重试累计）。
    pub tokens_in: i64,
    pub tokens_out: i64,
    /// 正在跑第几轮模型调用（1 起）。0 = 还没开始第一轮。与快照里的
    /// `turns` 同义：轮询兜底读到的数字和增量事件说的一致。
    pub turn: u32,
    /// 轮次上限。`turn/max_turns` 一起给出"进度感"：卡住时能看出是
    /// 第 3 轮还是第 11 轮，而不是只有一句"运行中"。
    pub max_turns: u32,
}

pub type LiveNotifier = Arc<dyn Fn(&NodeLive) + Send + Sync>;

/// 单个节点的增量缓冲。按节点分开，否则并行跑的两个节点会把正文串在一起。
#[derive(Default)]
struct LiveBuf {
    text: String,
    at: Option<std::time::Instant>,
    /// 已经通过增量事件报出去的 token（成功时据此补齐差额，避免重复计数）。
    tokens_in: i64,
    tokens_out: i64,
    /// 当前轮次，跟着增量一起发（正文/步骤事件都带上最新值）。
    turn: u32,
    max_turns: u32,
}

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
    /// tokens_in 里命中前缀缓存的部分（serde default：旧运行快照没有这个
    /// 字段，补 0，命中率显示"—"而不是编一个数）。
    #[serde(default)]
    pub tokens_cached: i64,
    /// 工具调用摘要（`read_file src/x.rs` 这种），给运行面板做过程感。
    pub steps: Vec<String>,
    /// 当前（或最后）一轮模型调用，1 起。和 `NodeLive.turn` 同一个数字：
    /// 事件断了靠轮询读快照，两边不能各说一套。
    #[serde(default)]
    pub turns: u32,
    /// 这一节点允许的最大轮次（`max_turns`）。面板显示 "3/12"，
    /// 用户才知道"跑得慢"和"快跑满了"是两件事。
    #[serde(default)]
    pub max_turns: u32,
    /// 主产出落制品的结果。写入失败或正文为空时为 `None`（正文仍在 `output` 里）。
    pub artifact: Option<super::artifact::ArtifactRef>,
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
                    tokens_cached: 0,
                    steps: Vec::new(),
                    turns: 0,
                    max_turns: 0,
                    artifact: None,
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
    /// 增量事件出口（正文 / 步骤 / token）。单测注入空回调。
    live: LiveNotifier,
    /// 每个节点攒着的正文与已报 token，见 [`LiveBuf`]。
    live_buf: Mutex<HashMap<String, LiveBuf>>,
    /// 是否把记录写到 `~/.prism/dags/runs/`。单测用临时/内存模式，
    /// 否则跑一次测试就往用户的真实目录里丢一堆假运行记录。
    persist: bool,
    /// 制品仓库。`None` = 不落制品（老环境或单测没给），下游退回内联正文。
    artifacts: Option<super::artifact::ArtifactStore>,
}

impl RunHandle {
    fn new(
        run: DagRun,
        notify: Notifier,
        live: LiveNotifier,
        persist: bool,
        artifacts: Option<super::artifact::ArtifactStore>,
    ) -> RunHandle {
        RunHandle {
            state: Mutex::new(run),
            cancelled: Arc::new(AtomicBool::new(false)),
            gates: Mutex::new(HashMap::new()),
            warned: AtomicBool::new(false),
            notify,
            live,
            live_buf: Mutex::new(HashMap::new()),
            persist,
            artifacts,
        }
    }

    /// 把节点正文落成制品。正文为空返回 `Ok(None)`（空产出没有引用价值）。
    ///
    /// 写入失败**不把节点判失败**：正文还在运行记录里，判失败等于丢掉已经
    /// 花钱跑出来的结果。调用方把它降级成一条 step。
    pub fn save_artifact(
        &self,
        dag_id: &str,
        node_id: &str,
        key: &str,
        text: &str,
    ) -> Result<Option<super::artifact::ArtifactRef>, String> {
        let Some(store) = self.artifacts.as_ref() else {
            return Ok(None);
        };
        if text.trim().is_empty() {
            return Ok(None);
        }
        store
            .put_text(dag_id, node_id, key, text, None)
            .map(Some)
    }

    pub fn artifact_store(&self) -> Option<&super::artifact::ArtifactStore> {
        self.artifacts.as_ref()
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
        // 上一次尝试的增量缓冲要清掉：重试从空白开始，否则旧正文会接到新一轮前面。
        self.drop_live(node_id);
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
                node.artifact = None;
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
        self.drop_live(node_id);
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

    /// 成功：一次状态迁移同时写正文与制品引用，只落一次盘、只广播一次
    /// （拆成两次会推两份快照，前端会看到"有正文但没制品"的中间态）。
    pub fn mark_succeeded(
        &self,
        node_id: &str,
        output: NodeOutput,
        artifact: Option<super::artifact::ArtifactRef>,
    ) {
        // 差额要在 update 之前算：settle_tokens 会把缓冲删掉。
        let (extra_in, extra_out) = self.settle_tokens(node_id, &output);
        self.update(
            node_id,
            |node| {
                node.status = NodeStatus::Succeeded;
                node.finished_at = Some(now_ms());
                node.output = output.text;
                node.tokens_in += extra_in;
                node.tokens_out += extra_out;
                node.steps = output.steps;
                if output.turns > 0 {
                    node.turns = output.turns;
                }
                node.error = None;
                node.artifact = artifact;
            },
            true,
        );
    }

    /// 某节点的主产制品引用（下游拼提示词时用）。
    pub fn artifact_of(&self, node_id: &str) -> Option<super::artifact::ArtifactRef> {
        self.state
            .lock()
            .ok()
            .and_then(|run| run.node(node_id).and_then(|node| node.artifact.clone()))
    }

    pub fn mark_skipped(&self, node_id: &str, reason: String) {
        self.drop_live(node_id);
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
        self.drop_live(node_id);
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

    /// 正文增量：写进内存里的 `output`，并按 120ms 一档推给前端。
    ///
    /// 只写内存不发事件的话，前端要等节点落定才看见正文——一个节点跑两分钟，
    /// 面板上就是两分钟空白，看起来像卡死。
    pub fn push_text(&self, node_id: &str, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Ok(mut run) = self.state.lock() {
            if let Some(node) = run.nodes.iter_mut().find(|node| node.id == node_id) {
                node.output.push_str(text);
            }
        }
        let due = {
            let Ok(mut buffers) = self.live_buf.lock() else {
                return;
            };
            let entry = buffers.entry(node_id.to_string()).or_default();
            entry.text.push_str(text);
            let due = entry.at.map_or(true, |at| at.elapsed() >= LIVE_FLUSH);
            if due {
                entry.at = Some(std::time::Instant::now());
            }
            due
        };
        if due {
            let chunk = self.take_text(node_id);
            self.emit_live(node_id, chunk, None);
        }
    }

    /// 新落的一步工具调用。顺带把攒着的正文发出去：新一步意味着上一段说完了，
    /// 别让它压在缓冲里再等一个 120ms。
    pub fn push_step(&self, node_id: &str, step: &str) {
        let step = step.trim();
        if step.is_empty() {
            return;
        }
        if let Ok(mut run) = self.state.lock() {
            if let Some(node) = run.nodes.iter_mut().find(|node| node.id == node_id) {
                if node.steps.len() < LIVE_STEPS {
                    node.steps.push(step.to_string());
                }
            }
        }
        let chunk = self.take_text(node_id);
        self.emit_live(node_id, chunk, Some(step.to_string()));
    }

    /// 一轮的 token 用量。写进内存里的节点（中途快照也能看到）并发一条增量。
    ///
    /// `tokens_cached` 只进节点状态（快照 / 结束后可见），不进 live 增量——
    /// 命中率是"看完一段才知道"的数字，跟着轮次跳来跳去只会添乱。
    pub fn push_usage(&self, node_id: &str, tokens_in: i64, tokens_out: i64, tokens_cached: i64) {
        if let Ok(mut buffers) = self.live_buf.lock() {
            let entry = buffers.entry(node_id.to_string()).or_default();
            entry.tokens_in += tokens_in;
            entry.tokens_out += tokens_out;
        }
        if let Ok(mut run) = self.state.lock() {
            if let Some(node) = run.nodes.iter_mut().find(|node| node.id == node_id) {
                node.tokens_in += tokens_in;
                node.tokens_out += tokens_out;
                node.tokens_cached += tokens_cached;
            }
        }
        let chunk = self.take_text(node_id);
        self.emit_live(node_id, chunk, None);
    }

    /// 到目前为止这场运行烧掉的 token 合计（所有节点的输入 + 输出）。
    ///
    /// 预算闸门用它。编排一次能花掉一块钱，没有上限就等于让模型自己决定账单：
    /// 一次五次工具全失效的运行，照样每个节点跑满轮次，钱花完才知道。
    ///
    /// 只算已经落账的：正在跑的那一轮还没 push 进来，所以闸门是"超一点才停"，
    /// 不会卡在半轮中间。
    pub fn used_tokens(&self) -> i64 {
        let Ok(run) = self.state.lock() else {
            return 0;
        };
        run.nodes
            .iter()
            .map(|node| node.tokens_in + node.tokens_out)
            .sum()
    }

    /// 进入新的一轮模型调用。
    ///
    /// 这一条解决的是最让人难受的那种"假死"：模型在想、在生成长回复时可能
    /// 一两分钟不吐一个字也不调工具，面板上就只剩"运行中"三个字。轮次事件
    /// 让面板能写"第 3/12 轮 · 等模型返回"，用户至少知道它在往前走。
    pub fn push_turn(&self, node_id: &str, turn: u32, max_turns: u32) {
        if let Ok(mut buffers) = self.live_buf.lock() {
            let entry = buffers.entry(node_id.to_string()).or_default();
            entry.turn = turn;
            entry.max_turns = max_turns;
        }
        if let Ok(mut run) = self.state.lock() {
            if let Some(node) = run.nodes.iter_mut().find(|node| node.id == node_id) {
                // 存"当前轮次"而不是"已完成轮次"：轮询兜底读的是快照，
                // 两边必须说同一个数字，否则事件一断就退回"第 0 轮"。
                node.turns = turn;
                node.max_turns = max_turns;
            }
        }
        let chunk = self.take_text(node_id);
        self.emit_live(node_id, chunk, None);
    }

    /// 取走某节点攒着的正文（取完就清空）。
    fn take_text(&self, node_id: &str) -> String {
        self.live_buf
            .lock()
            .ok()
            .and_then(|mut buffers| {
                buffers
                    .get_mut(node_id)
                    .map(|entry| std::mem::take(&mut entry.text))
            })
            .unwrap_or_default()
    }

    /// 丢掉某节点的增量缓冲（重试 / 落定后调用，别把旧正文接到新一轮前面）。
    fn drop_live(&self, node_id: &str) {
        if let Ok(mut buffers) = self.live_buf.lock() {
            buffers.remove(node_id);
        }
    }

    fn emit_live(&self, node_id: &str, delta: String, step: Option<String>) {
        let (tokens_in, tokens_out, turn, max_turns) = self
            .live_buf
            .lock()
            .ok()
            .and_then(|buffers| {
                buffers
                    .get(node_id)
                    .map(|entry| (entry.tokens_in, entry.tokens_out, entry.turn, entry.max_turns))
            })
            .unwrap_or((0, 0, 0, 0));
        (self.live)(&NodeLive {
            run_id: self.id().to_string(),
            node_id: node_id.to_string(),
            delta,
            step,
            tokens_in,
            tokens_out,
            turn,
            max_turns,
        });
    }

    /// 成功收尾前把「增量里已经报过的 token」扣掉，只补差额。
    ///
    /// `mark_succeeded` 用的累加（重试要跨次累计），而 `push_usage` 已经逐轮
    /// 加过一次——不扣就会双计。执行器不报 usage 时（外部引擎自己算总账），
    /// 这里退化成全额累加，和原来一致。
    fn settle_tokens(&self, node_id: &str, output: &NodeOutput) -> (i64, i64) {
        let (reported_in, reported_out) = self
            .live_buf
            .lock()
            .ok()
            .and_then(|mut buffers| buffers.remove(node_id))
            .map(|entry| (entry.tokens_in, entry.tokens_out))
            .unwrap_or((0, 0));
        (
            (output.tokens_in - reported_in).max(0),
            (output.tokens_out - reported_out).max(0),
        )
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
    /// 制品仓库：真机是 `~/.prism/artifacts`，单测指到临时目录（不污染用户数据）。
    artifacts: Option<super::artifact::ArtifactStore>,
}

impl Default for DagHub {
    fn default() -> Self {
        DagHub {
            runs: Mutex::new(HashMap::new()),
            latest: Mutex::new(HashMap::new()),
            persist: true,
            artifacts: super::artifact::ArtifactStore::global().ok(),
        }
    }
}

impl DagHub {
    /// 只在内存里跑（单测用）：不写 `~/.prism`。
    #[cfg(test)]
    pub fn ephemeral() -> Self {
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "prism-dag-artifacts-{}-{seq}",
            std::process::id()
        ));
        DagHub {
            persist: false,
            artifacts: Some(super::artifact::ArtifactStore::at(dir)),
            ..DagHub::default()
        }
    }

    /// 起一次运行（不带直播）。单测用：没人听增量事件。
    ///
    /// 真机一律走 [`DagHub::start_with`]——面板要看节点跑的过程，没有直播就是
    /// 一片空白。
    #[cfg(test)]
    pub fn start(
        &self,
        plan: DagPlan,
        agents: &[crate::agents::AgentProfile],
        // 单测注入的假执行器；真机传 None，由执行器注册表按 Agent 解析。
        runner: Option<Arc<dyn NodeRunner>>,
        notify: Notifier,
    ) -> Result<DagRun, String> {
        // 不带直播：单测与"只看结果"的调用方都不需要增量事件。
        self.start_with(plan, agents, runner, notify, Arc::new(|_| {}))
    }

    /// 起一次运行，并把节点运行中的增量（正文 / 步骤 / token）交给 `live`。
    /// 真机由 IPC 注入"发事件"的闭包；校验不通过就不起（返回人能看懂的原因）。
    pub fn start_with(
        &self,
        plan: DagPlan,
        agents: &[crate::agents::AgentProfile],
        runner: Option<Arc<dyn NodeRunner>>,
        notify: Notifier,
        live: LiveNotifier,
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
        let handle = Arc::new(RunHandle::new(
            run.clone(),
            notify.clone(),
            live,
            self.persist,
            self.artifacts.clone(),
        ));
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
        let run = super::store::latest_run_of(plan_id).ok().flatten()?;
        Some(self.repair_if_orphan(run))
    }

    /// 按 id 取运行：内存优先，其次磁盘（进程重启后仍能看历史）。
    pub fn run_by_id(&self, run_id: &str) -> Option<DagRun> {
        if let Some(handle) = self.get(run_id) {
            return Some(handle.snapshot());
        }
        let run = super::store::load_run(run_id).ok()?;
        Some(self.repair_if_orphan(run))
    }

    /// 磁盘上的记录还写着「运行中」，但内存里没有句柄 —— 说明上次进程已经没了。
    ///
    /// 不修的话，重启后这张计划永远显示"正在跑"：`runBlocker` 会以"这张计划正在跑"
    /// 拒绝重跑，用户看到的就是"卡住了"。改成中断状态，并把还在跑/等确认的节点
    /// 标成失败——这才是事实：没有进程在跑。
    fn repair_if_orphan(&self, mut run: DagRun) -> DagRun {
        if run.status != RunStatus::Running {
            return run;
        }
        let now = now_ms();
        let mut interrupted = 0;
        for node in run.nodes.iter_mut() {
            if matches!(node.status, NodeStatus::Running | NodeStatus::Waiting) {
                node.status = NodeStatus::Failed;
                node.finished_at = Some(now);
                node.error = Some("运行中断：应用重启，这个节点没有跑完".to_string());
                interrupted += 1;
            }
        }
        run.status = RunStatus::Failed;
        run.finished_at = Some(now);
        run.message = Some(if interrupted > 0 {
            format!(
                "这次运行被应用重启打断了（{interrupted} 个节点没跑完）。可以直接重跑。"
            )
        } else {
            "这次运行被应用重启打断了。可以直接重跑。".to_string()
        });
        // 落回去：否则下次再查还是"运行中"，前端每次都要自己纠正一遍。
        // 内存模式（单测）不写盘，别把假记录丢进用户的真实目录。
        if self.persist {
            let _ = super::store::save_run(&run);
        }
        run
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 单节点运行记录：直播相关的方法只看这一个节点。
    fn handle(node: &str) -> (Arc<RunHandle>, Arc<Mutex<Vec<NodeLive>>>) {
        let run = DagRun {
            id: "run-1".to_string(),
            plan_id: "dag-1".to_string(),
            plan_name: "计划".to_string(),
            project: "/tmp".to_string(),
            status: RunStatus::Running,
            started_at: now_ms(),
            finished_at: None,
            message: None,
            nodes: vec![NodeRun {
                id: node.to_string(),
                title: node.to_string(),
                agent: "general".to_string(),
                status: NodeStatus::Running,
                attempt: 1,
                started_at: Some(now_ms()),
                finished_at: None,
                output: String::new(),
                error: None,
                tokens_in: 0,
                tokens_out: 0,
                tokens_cached: 0,
                steps: Vec::new(),
                turns: 0,
                max_turns: 0,
                artifact: None,
            }],
        };
        let seen: Arc<Mutex<Vec<NodeLive>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let live: LiveNotifier = Arc::new(move |event: &NodeLive| {
            if let Ok(mut list) = sink.lock() {
                list.push(event.clone());
            }
        });
        (Arc::new(RunHandle::new(run, Arc::new(|_| {}), live, false, None)), seen)
    }

    fn node_of(handle: &RunHandle) -> NodeRun {
        handle.state.lock().unwrap().nodes[0].clone()
    }

    #[test]
    fn push_step_appends_and_reports_live() {
        let (handle, seen) = handle("n1");
        handle.push_step("n1", "read_file src/x.rs");
        let steps = node_of(&handle).steps;
        assert_eq!(steps, vec!["read_file src/x.rs"]);
        let events = seen.lock().unwrap().clone();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].step.as_deref(), Some("read_file src/x.rs"));
        assert_eq!(events[0].node_id, "n1");
        assert_eq!(events[0].run_id, "run-1");
    }

    #[test]
    fn push_text_is_coalesced_not_per_delta() {
        let (handle, seen) = handle("n1");
        // 第一段立刻发（还没有上一次发送时间），第二段压在缓冲里等 120ms。
        handle.push_text("n1", "先看");
        handle.push_text("n1", "头部");
        assert_eq!(seen.lock().unwrap().len(), 1, "两段合并不该发两条");
        // 新一步会把攒着的正文一起带出去，顺序不乱、内容不丢。
        handle.push_step("n1", "read_file a.bin");
        let events = seen.lock().unwrap().clone();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].delta, "头部");
        assert_eq!(events[1].step.as_deref(), Some("read_file a.bin"));
        // 内存里的正文是完整的：落定时快照用它。
        assert_eq!(node_of(&handle).output, "先看头部");
    }

    #[test]
    fn usage_reported_twice_is_not_double_counted() {
        let (handle, _) = handle("n1");
        handle.push_usage("n1", 100, 10, 80);
        handle.push_usage("n1", 50, 5, 40);
        assert_eq!(node_of(&handle).tokens_in, 150, "逐轮上报要实时累加");
        assert_eq!(node_of(&handle).tokens_cached, 120, "缓存命中同步累加");

        // 执行器最后的总额与逐轮之和一致：收尾只补差额，不能再来一遍。
        let mut output = NodeOutput::default();
        output.text = "报告".to_string();
        output.tokens_in = 150;
        output.tokens_out = 15;
        output.tokens_cached = 120;
        handle.mark_succeeded("n1", output, None);
        let node = node_of(&handle);
        assert_eq!(node.tokens_in, 150);
        assert_eq!(node.tokens_out, 15);
        assert_eq!(node.tokens_cached, 120);
        assert_eq!(node.output, "报告");
    }

    #[test]
    fn usage_only_at_the_end_still_counts() {
        // 外部引擎自己算总账：没有逐轮上报，收尾要全额累加（和改之前一致）。
        let (handle, _) = handle("n1");
        let mut output = NodeOutput::default();
        output.tokens_in = 900;
        output.tokens_out = 90;
        handle.mark_succeeded("n1", output, None);
        let node = node_of(&handle);
        assert_eq!(node.tokens_in, 900);
        assert_eq!(node.tokens_out, 90);
    }

    #[test]
    fn orphaned_running_record_is_repaired() {
        // 重启后内存里没有句柄，磁盘记录还写着 running：不改成中断，面板会永远
        // 显示"正在跑"，重跑按钮也一直被"这张计划正在跑"挡住。
        let hub = DagHub::ephemeral();
        let mut run = {
            let (handle, _) = handle("n1");
            let mut snapshot = handle.snapshot();
            snapshot.nodes[0].status = NodeStatus::Running;
            snapshot
        };
        run.status = RunStatus::Running;
        let repaired = hub.repair_if_orphan(run);
        assert_eq!(repaired.status, RunStatus::Failed);
        assert!(repaired.finished_at.is_some());
        assert_eq!(repaired.nodes[0].status, NodeStatus::Failed);
        assert!(repaired.message.unwrap_or_default().contains("重启"));
    }

    #[test]
    fn turn_is_reported_per_round_and_kept_in_snapshot() {
        let (handle, seen) = handle("n1");
        handle.push_turn("n1", 1, 12);
        handle.push_turn("n1", 2, 12);
        // 快照里存"当前轮次"：和增量事件同一个数字，事件断了轮询也对得上。
        let node = node_of(&handle);
        assert_eq!(node.turns, 2, "快照按当前轮次记");
        assert_eq!(node.max_turns, 12);

        let events = seen.lock().unwrap().clone();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].turn, 2);
        assert_eq!(events[1].max_turns, 12);

        // 收尾时把执行器的总轮次补上（重试只补不覆盖）。
        let mut output = NodeOutput::default();
        output.text = "报告".to_string();
        output.turns = 3;
        handle.mark_succeeded("n1", output, None);
        assert_eq!(node_of(&handle).turns, 3);
    }

    #[test]
    fn retry_drops_previous_live_text() {
        let (handle, _) = handle("n1");
        handle.push_text("n1", "上一次尝试的正文");
        handle.mark_failed("n1", "超时".to_string());
        handle.mark_running("n1");
        let node = node_of(&handle);
        assert!(node.output.is_empty(), "重试要从空白开始");
        assert!(node.steps.is_empty());
    }
}
