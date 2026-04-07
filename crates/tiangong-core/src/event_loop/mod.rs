//! 事件驱动循环运行时
//!
//! EventLoopRunner 负责：事件输入 → LLM 调用 → 工具执行 → TurnEvent 输出
//! TiangongCore 负责：消费 TurnEvent → 更新 session → 推送 StreamEvent

pub mod context;
pub mod persistence;
pub mod runner;
pub mod types;

pub use runner::EventLoopRunner;
pub use types::{LoopEvent, LoopOutcome, LoopPhase, LoopState, SystemSignalKind};
