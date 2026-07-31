//! 记忆召回进程内插件。
//!
//! 收敛 `recall_memory` 工具：按需回忆历史上下文、跨会话结果、之前的工具输出或
//! 生成产物。原为 core 硬编码特判（`inject_memory_recall_tool` +
//! `execute_memory_recall_tool` + engine.rs 内的工具名拦截分支），现作为插件工具暴露。
//!
//! 运行时上下文通过以下方式获取：
//! - **构造注入**：调用方显式提供 MemoryHandle。当前 App、CLI、Server 已改用
//!   WASM 插件与 sidecar，不再注册本原生插件。
//! - [`Plugin::set_feedback_tx`]：复用 worker 命令通道，通过
//!   [`PluginFeedbackTx::send_stream_event`] 转发 `MemoryRecallStart` / `Progress` /
//!   `Done` 流事件（不进入对话历史，纯 UI 反馈）。
//! - [`Plugin::on_config_updated`]：config 变化时重新加载 MemoryConfig 并 reconfigure。
//! - [`ToolOverrideHandler::handle`] 的 `&Session` 参数：读取会话消息构建检索上下文，
//!   以及 query 参数为空时回退取最近一条用户消息。
//! - [`PromptSectionProvider::prompt_sections`]：注入三级 Memory Injection（用户偏好/
//!   记忆），替代原 core 的 `build_user_context`。
//!
//! 「本轮已回忆」去重（原 engine.rs 的 `memory_recall_attempted` 标志）下沉到插件的
//! `RwLock<bool>` 字段，由 [`Plugin::on_turn_started`] 每轮重置。

pub mod handler;
pub mod plugin;
mod turn_extract;

pub use plugin::MemoryPlugin;

use std::sync::Arc;

use tiangong_core::core::Plugin;

/// 构造记忆召回插件实例，返回 `Arc<dyn Plugin>` 供入口注册。
///
/// `memory_handle` 由调用方显式传入。`None` 表示记忆系统未启用。
pub fn build_plugin(memory_handle: Option<tiangong_memory::MemoryHandle>) -> Arc<dyn Plugin> {
    Arc::new(MemoryPlugin::new(memory_handle))
}

/// 构造默认的记忆召回插件列表，供各入口（CLI / Server）注入 core 时使用。
pub fn default_plugins(
    memory_handle: Option<tiangong_memory::MemoryHandle>,
) -> Vec<Arc<dyn Plugin>> {
    vec![build_plugin(memory_handle)]
}
