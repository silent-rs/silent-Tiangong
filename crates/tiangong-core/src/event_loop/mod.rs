//! 事件驱动循环运行时
//!
//! core 内部完成完整处理链路：
//! 事件输入 → 组织上下文 → LLM 调用 → 工具执行 → session 更新 → 回调通知外部
//!
//! 外部使用方式：
//! - CLI：直接调用 EventLoopRunner，传入打印回调
//! - GUI：为每个会话创建 EventLoopRunner，传入 channel/emit 回调
//!
//! 参考：`docs/rfc/0005-event-loop-runtime.md`

pub mod active_loops;
pub mod cli_host;
pub mod context;
pub mod host;
pub mod persistence;
pub mod runner;
pub mod types;

pub use runner::EventLoopRunner;
pub use types::{LoopEvent, LoopOutcome, LoopPhase, LoopState, SystemSignalKind};
