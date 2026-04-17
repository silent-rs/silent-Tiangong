//! 天工 Memory 系统
//!
//! 提供分层记忆管理、三级 Injection 注入、Episode 写入与渐进式召回。
//!
//! # 使用方式
//!
//! 各入口（GUI、CLI、Server）在启动时调用 [`start`] 获取 [`MemoryHandle`]，
//! 然后将 Handle 显式传给上层运行时。
//!
//! ```no_run
//! let handle = tiangong_memory::start(None).expect("Memory 系统启动失败");
//! let injections = tiangong_memory::load_injection_sync("session-1", None);
//! ```

pub mod command;
pub mod election;
pub mod handle;
pub mod ipc;
pub mod types;

mod actor;
mod db;
mod injection;
mod recall;
mod rumination;
mod search;
mod store;
mod writer;

pub use actor::start_memory as start;
pub use election::ProcessType;
pub use handle::MemoryHandle;
pub use types::*;

/// 同步加载三级注入上下文（不经过 Actor，直接读文件）
///
/// 用于不在异步上下文中调用注入内容的场景（如测试、同步 prompt 装配）。
pub fn load_injection_sync(session_id: &str, workspace_id: Option<&str>) -> Vec<String> {
    injection::load_injection_context(session_id, workspace_id)
}

/// 同步初始化 Memory（不启动 Actor，仅确保 SQLite 数据库就绪）
///
/// 适合无 tokio 运行时的入口（如 CLI 同步模式），保证数据库文件被创建并加密。
pub fn init_blocking() -> anyhow::Result<()> {
    let _ = db::MemoryDb::open()?;
    Ok(())
}
