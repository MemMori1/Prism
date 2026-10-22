//! DAG 数据模型。
//!
//! 计划是**数据**不是代码：一张图能存成 JSON、能手改、能导出、能被前端画出来。
//! 执行器、工作区、规划器才是插件（见 `docs/DAG_PLANNER.md` §6）。
//!
//! 权限档与工作区模式直接复用 [`crate::agents`] 里的定义——节点和 Agent profile
//! 说的是同一件事，分成两套枚举迟早会漂移。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub use crate::agents::{Permission, ToolSelection, Workspace};

/// schema 版本：加字段时不动它，改语义时才 +1，便于迁移。
pub const VERSION: u32 = 1;

/// 节点在画布上的坐标（布局结果由前端算，这里只存用户拖过的位置）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

/// 节点失败后的重试策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Retry {
    /// 额外重试次数（0 = 不重试）。
    #[serde(default)]
    pub attempts: u8,
    #[serde(default = "default_backoff")]
    pub backoff_ms: u64,
}

fn default_backoff() -> u64 {
    2_000
}

impl Default for Retry {
    fn default() -> Self {
        Retry {
            attempts: 0,
            backoff_ms: default_backoff(),
        }
    }
}

/// 人工闸门：运行前 / 收尾后要不要人点一下。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Gate {
    #[serde(default)]
    pub before: bool,
    #[serde(default)]
    pub after: bool,
}

/// 失败策略。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailurePolicy {
    /// 一个节点失败 → 取消兄弟节点，下游标 skipped。
    #[default]
    FailFast,
    /// 无关分支继续跑，最后汇总失败清单。
    Continue,
}

/// 整张图的默认值：节点没写就跟随这里。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DagDefaults {
    /// 默认 Agent profile id。
    pub agent: String,
    #[serde(default)]
    pub permission: Permission,
    #[serde(default = "default_parallel")]
    pub max_parallel: u8,
    #[serde(default)]
    pub failure_policy: FailurePolicy,
    #[serde(default)]
    pub token_budget: Option<i64>,
}

fn default_parallel() -> u8 {
    2
}

impl Default for DagDefaults {
    fn default() -> Self {
        DagDefaults {
            agent: "general".to_string(),
            permission: Permission::WorkspaceWrite,
            max_parallel: default_parallel(),
            failure_policy: FailurePolicy::FailFast,
            token_budget: None,
        }
    }
}

/// 一条"要读上游哪个制品"的声明。
///
/// 不写也能用：每个上游的主产出制品自动以「节点 id」为别名可引用。
/// 需要改别名、或者要读上游**非主产出**（工具写入的 `strings` / `hashes`）时才写。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactInput {
    /// 上游节点 id。
    pub from: String,
    /// 要它哪个制品；空 = 上游的主产出 key。
    #[serde(default)]
    pub key: String,
    /// 占位符里用的名字：`{{artifacts.<as>.summary}}`。
    #[serde(default, rename = "as")]
    pub alias: String,
}

/// 一个节点 = 一次独立会话。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DagNode {
    /// 短、稳定、人可读：`research` / `frontend` / `verify`。
    pub id: String,
    pub title: String,
    /// 提示词模板，支持 `{{nodes.<id>.output}}` 占位符。
    pub task: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// `None` = 用 `defaults.agent`。
    #[serde(default)]
    pub agent: Option<String>,
    /// `None` = 跟随 Agent profile。
    #[serde(default)]
    pub workspace: Option<Workspace>,
    /// `None` = 跟随 Agent profile。
    #[serde(default)]
    pub tools: Option<ToolSelection>,
    /// `None` = 用 `defaults.permission`。
    #[serde(default)]
    pub permission: Option<Permission>,
    #[serde(default)]
    pub retry: Retry,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub gate: Option<Gate>,
    #[serde(default)]
    pub position: Option<Position>,
    /// 显式声明的上游制品引用（可省，见 [`ArtifactInput`]）。
    #[serde(default)]
    pub input_artifacts: Vec<ArtifactInput>,
    /// 本节点产出落到哪个制品 key。空 = `report`。
    #[serde(default)]
    pub output_artifact_key: String,
}

/// 主产出的默认 key。
pub const DEFAULT_ARTIFACT_KEY: &str = "report";

impl DagNode {
    /// 这个节点要人确认才开跑。
    pub fn needs_gate(&self) -> bool {
        self.gate.map(|gate| gate.before).unwrap_or(false)
    }

    /// 本节点产出的制品 key（空值回落默认）。
    pub fn artifact_key(&self) -> String {
        let trimmed = self.output_artifact_key.trim();
        if trimmed.is_empty() {
            DEFAULT_ARTIFACT_KEY.to_string()
        } else {
            trimmed.to_string()
        }
    }
}

/// 一张计划。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DagPlan {
    #[serde(default = "default_version")]
    pub version: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub goal: String,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub defaults: DagDefaults,
    #[serde(default)]
    pub nodes: Vec<DagNode>,
    /// 画布坐标：有值就不覆盖（用户拖过）；「自动排版」清空它。
    #[serde(default)]
    pub layout: HashMap<String, Position>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

fn default_version() -> u32 {
    VERSION
}

impl DagPlan {
    pub fn node(&self, id: &str) -> Option<&DagNode> {
        self.nodes.iter().find(|node| node.id == id)
    }
}

/// 列表视图：不带节点正文，够画左栏就行。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DagSummary {
    pub id: String,
    pub name: String,
    pub goal: String,
    pub project: String,
    pub node_count: usize,
    /// 需要人工确认的节点数（左栏标「待确认」用）。
    pub gate_count: usize,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<&DagPlan> for DagSummary {
    fn from(plan: &DagPlan) -> Self {
        DagSummary {
            id: plan.id.clone(),
            name: plan.name.clone(),
            goal: plan.goal.clone(),
            project: plan.project.clone(),
            node_count: plan.nodes.len(),
            gate_count: plan.nodes.iter().filter(|node| node.needs_gate()).count(),
            created_at: plan.created_at,
            updated_at: plan.updated_at,
        }
    }
}
