//! 天工 Memory 系统
//!
//! 提供分层记忆管理、三级 Injection 注入、Episode 写入与渐进式召回。
//!
//! # 使用方式
//!
//! 各入口（GUI、CLI、Server）在启动时调用 [`ensure_started`]，
//! 之后任何代码都可通过 [`global_handle`] 直接获取 Handle，无需外部传递。
//!
//! ```no_run
//! tiangong_memory::ensure_started(None).expect("Memory 系统启动失败");
//! let handle = tiangong_memory::global_handle(); // 可在任何地方调用
//! ```

use std::sync::OnceLock;

pub mod command;
pub mod election;
pub mod handle;
pub mod types;

mod actor;
mod db;
mod injection;
mod ipc;
mod recall;
mod rumination;
mod search;
mod store;
mod writer;

pub use actor::start_memory as start;
pub use election::ProcessType;
pub use handle::MemoryHandle;
pub use types::*;

/// 进程级全局 Memory Handle 单例
static GLOBAL_HANDLE: OnceLock<MemoryHandle> = OnceLock::new();

/// 确保 Memory Actor 已启动并返回全局 Handle 引用（幂等，多次调用安全）
///
/// 各进程入口（CLI、Server、GUI）在启动时调用一次即可。
/// 若 Actor 已启动，忽略 workspace_id 参数直接返回已有 Handle。
pub fn ensure_started(workspace_id: Option<String>) -> anyhow::Result<&'static MemoryHandle> {
    if let Some(h) = GLOBAL_HANDLE.get() {
        return Ok(h);
    }
    let handle = start(workspace_id)?;
    // OnceLock::get_or_init 在竞争时保证只有一个 winner
    Ok(GLOBAL_HANDLE.get_or_init(|| handle))
}

/// 获取全局 Memory Handle（返回 None 表示 ensure_started 尚未调用或已失败）
pub fn global_handle() -> Option<&'static MemoryHandle> {
    GLOBAL_HANDLE.get()
}

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
