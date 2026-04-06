//! 事件驱动循环运行时
//!
//! 替代 Turn-based 的 `TurnRunner`，以事件驱动方式处理用户交互。
//! 所有外部输入（用户消息、工具结果、后台任务完成等）统一为事件，
//! 循环持续运行直到无事件时自动挂起。
//!
//! 参考：`docs/rfc/0005-event-loop-runtime.md`

pub mod context;
pub mod runner;
pub mod types;

pub use runner::EventLoopRunner;
pub use types::{LoopEvent, LoopOutcome, LoopPhase, LoopState, SystemSignalKind};
