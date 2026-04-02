//! 多代理协调层
//!
//! 管理复杂任务的拆分、并行执行和结果汇总。
//! 当前版本支持单 Worker 模式（退化为直接执行），
//! 后续版本将支持多 Worker 并行。

pub mod task_coordinator;
pub mod types;
pub mod worker;

pub use task_coordinator::TaskCoordinator;
pub use types::*;
