//! 后台任务管理进程内插件。
//!
//! 收敛 5 个后台任务工具：
//! - `spawn_task`：在后台启动命令（不阻塞当前 turn）
//! - `query_task`：查询后台任务状态
//! - `list_tasks`：列出所有后台任务
//! - `cancel_task`：取消后台任务
//! - `wait_tasks`：等待后台任务完成
//!
//! 原 `tiangong-core::runtime::handle_background_task` + `inject_enhanced_tools` 的
//! 后台任务 spec，随收敛重构迁出为独立插件（#208）。通过 `ToolOverrideHandler`
//! 统一分发，core 不再硬编码特判这 5 个工具。
//!
//! `task_registry()` 为全局 `OnceLock` 静态，GUI 管理（src-tauri 的
//! get_background_tasks / cancel_background_task）与 LLM 工具调用命中同一注册表。

pub mod handler;
pub mod plugin;

pub use handler::{TaskInfo, TaskRegistry, TaskStatus, task_registry};
pub use plugin::TaskPlugin;

use std::sync::Arc;

use tiangong_core::core::Plugin;

/// 构造后台任务插件实例，返回 `Arc<dyn Plugin>` 供入口注册。
pub fn build_plugin() -> Arc<dyn Plugin> {
    Arc::new(TaskPlugin::new())
}

/// 构造默认的后台任务插件列表，供各入口（CLI / Server）注入 core 时使用。
pub fn default_plugins() -> Vec<Arc<dyn Plugin>> {
    vec![build_plugin()]
}
