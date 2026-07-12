//! TiangongCore 的错误类型。
//!
//! 用于 [`crate::core::TiangongCoreBuilder::build`]、
//! [`crate::agent_input::AgentInput::deliver`] 与
//! [`crate::core::TiangongCore::into_session`]、
//! [`crate::core::TiangongCore::shutdown_join`] 的失败语义，
//! 让调用方能够区分失败原因，而非依赖布尔值或静默兜底。

use std::fmt;

/// TiangongCore 构造与运行期错误。
///
/// 变体按实际可观测的失败原因收敛（不预留当前架构无法触达的状态）。
#[derive(Debug)]
pub enum CoreError {
    /// Builder 缺少必填字段（如 config/session/stream_tx/storage）。
    MissingBuilderField(&'static str),
    /// worker 已停止，命令通道已关闭——`deliver` 无法投递命令。
    WorkerStopped,
    /// Prepared 用户消息未能持久化，Core 已恢复投递前的内存状态。
    MessagePersistenceFailed(String),
    /// 消息已入队，但 worker 未返回持久化确认。
    PersistenceConfirmationDropped,
    /// worker 线程 panic，会话不可恢复，关闭并等待 worker 的操作失败。
    WorkerPanicked,
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreError::MissingBuilderField(name) => {
                write!(f, "Builder 缺少必填字段：{name}")
            }
            CoreError::WorkerStopped => write!(f, "worker 已停止，命令通道已关闭"),
            CoreError::MessagePersistenceFailed(message) => {
                write!(f, "用户消息持久化失败：{message}")
            }
            CoreError::PersistenceConfirmationDropped => {
                write!(f, "worker 未返回用户消息持久化确认")
            }
            CoreError::WorkerPanicked => write!(f, "worker 线程 panic，会话不可恢复"),
        }
    }
}

impl std::error::Error for CoreError {}
