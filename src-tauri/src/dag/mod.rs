//! DAG 编排：计划、校验、存储、规划器与**执行引擎**。
//!
//! 分层很硬：
//!
//! ```text
//! model      数据结构（计划 / 节点 / 策略）
//! validate   图能不能跑（环、悬空依赖、占位符越界）
//! store      计划与运行记录的落盘
//! artifact   制品仓库（节点产物托管与引用）
//! planner    一句目标 → 草案（调模型）
//! runner     一个 Agent + 一段任务 → 一段报告（执行器插件）
//! engine_runner  外部引擎的桥：开会话 → 发提示词 → 从事件流收正文
//! executor   按依赖把节点放出去（调度、并发、重试、闸门、失败策略）
//! run        运行状态机与广播（前端看的那份快照）
//! ```
//!
//! 加一个执行器引擎 = 实现 [`runner::NodeRunner`]；调度、重试、闸门全都复用。

pub mod artifact;
pub mod engine_runner;
pub mod executor;
pub mod model;
pub mod planner;
pub mod run;
pub mod runner;
pub mod store;
pub mod validate;

pub use model::{DagPlan, DagSummary};
pub use run::{DagHub, DagRun};
pub use validate::Issue;
