//! 基于独立 [`tiangong_core::core::TiangongCore`] 的 Agent Team 插件。
//!
//! 每个团队成员拥有独立 Core、worker 线程、Tokio runtime 与 Session。插件只负
//! 责生命周期、消息路由、事件桥和团队策略，不再绕过 Core worker，也不重建
//! Core 的取消、持久化或终态逻辑。

mod adapter;
mod child_runtime;
mod constants;
mod coordinator;
mod manifest;
mod state;
mod tools;

pub use adapter::AgentTeamPlugin;
pub use constants::*;
pub use state::{AgentDescriptor, AgentStatus, FileLock, FileLockManager};

use std::path::PathBuf;
use std::sync::Arc;

use tiangong_core::core::Plugin;

use crate::coordinator::Coordinator;

/// 构造一个父 Core 使用的 Agent Team 插件。
pub fn build_plugin(
    storage_root: PathBuf,
    child_plugins: Arc<dyn Fn() -> Vec<Arc<dyn Plugin>> + Send + Sync>,
) -> Arc<dyn Plugin> {
    Arc::new(AgentTeamPlugin::new(storage_root, child_plugins))
}

/// 返回默认插件集合。
pub fn default_plugins(
    storage_root: PathBuf,
    child_plugins: Arc<dyn Fn() -> Vec<Arc<dyn Plugin>> + Send + Sync>,
) -> Vec<Arc<dyn Plugin>> {
    vec![build_plugin(storage_root, child_plugins)]
}

/// 由宿主直接请求 Agent Team 插件取消指定子 Agent，不经过 Core 命令路由。
pub fn cancel_agent(parent_session_id: &str, role: &str) -> bool {
    Coordinator::cancel_registered(parent_session_id, role)
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::OnceLock;

    fn storage_test_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    pub(crate) fn storage_test_guard() -> tokio::sync::MutexGuard<'static, ()> {
        storage_test_lock().blocking_lock()
    }

    pub(crate) async fn storage_test_guard_async() -> tokio::sync::MutexGuard<'static, ()> {
        storage_test_lock().lock().await
    }
}
