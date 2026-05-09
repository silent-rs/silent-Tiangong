//! 多代理协调层
//!
//! 管理复杂任务的拆分、并行执行和结果汇总。
//! - 单 Worker 模式：简单任务直接执行（退化）
//! - 多 Worker 模式：LLM 拆分子任务 → 并行执行 → LLM 合成结果
//!
//! Worker 通过 AgentCommand channel 接收外部控制指令（取消、审批），
//! 实现真正的双向通信，而非 dummy channel。

pub mod channel;
pub mod task_coordinator;
pub mod types;
pub mod worker;

pub use channel::{AgentCommand, AgentEvent};
pub use task_coordinator::TaskCoordinator;
pub use types::*;
pub use worker::Worker;
