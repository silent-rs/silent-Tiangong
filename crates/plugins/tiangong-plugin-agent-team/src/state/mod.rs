//! 团队状态数据结构（迁自 `tiangong-core/src/agent_team/`）。
//!
//! 本模块承载与执行链路无关的纯数据结构：Agent 描述符、消息总线、文件锁管理器、
//! Agent 注册表。迁入插件 crate 后仅调整 import 路径（`crate::session::Session`
//! → `tiangong_core::session::Session`），数据结构与算法保持原样。

pub mod descriptor;
pub mod file_lock;
pub mod message_bus;
pub mod registry;

pub use descriptor::{AgentDescriptor, AgentStatus};
pub use file_lock::{FileLock, FileLockManager};
pub use message_bus::{AgentMessage, MessagePriority};
pub use registry::AgentRegistry;
