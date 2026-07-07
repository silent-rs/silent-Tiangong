//! MCP 管理插件。
//!
//! 承载 MCP 相关的全部能力（对齐 [`tiangong_plugin_skill`] 的自治模式）：
//! - **LLM 工具**：动态收集的 MCP server 工具（运行时探测缓存 → [`ToolSpec`]）
//! - **工具执行**：MCP 工具调用经 [`ToolOverrideHandler`] 统一分发
//! - **App 管理 API**：[`McpPlugin`] 直接提供 register / update / remove /
//!   set_enabled / probe / list / detail 方法，供 App/Tauri/CLI 调用（入口层
//!   持有插件实例，dual-ownership）
//!
//! MCP 配置已从 [`tiangong_core::agent_config::AgentConfig`] 脱离，由本插件
//! 自托管读写 `~/.tiangong/mcp.json`。MCP 底层模块（client / capability /
//! config / execution adapter）整块迁入本 crate，core 不再持有 MCP 概念。

pub mod capability;
pub mod client;
pub mod config;
pub mod execution;
pub mod handler;
pub mod management;
pub mod paths;
pub mod plugin;
pub mod validate;

// 仅 re-export 有外部 crate 消费方的符号。其余类型（McpToolMeta / McpClient /
// McpFunctionTarget / describe_mcp_servers 等）为 plugin 内部使用，外部如需可直接
// 经 `tiangong_plugin_mcp::client::McpToolMeta` 等模块路径访问。
pub use config::{
    McpConfig, McpServerConfig, McpTransportMode, RegisterMcpServerOptions,
    RegisterMcpServerRequest, ResolvedMcpTransport,
};
pub use plugin::McpPlugin;
pub use validate::validate_mcp_config;

use std::sync::Arc;

use tiangong_core::core::Plugin;

/// 构造 MCP 插件实例，返回 `Arc<dyn Plugin>` 供入口注册。
pub fn build_plugin() -> Arc<dyn Plugin> {
    Arc::new(McpPlugin::new())
}

/// 构造默认的 MCP 插件列表，供各入口（CLI / Server / Tauri）注入 core 时使用。
pub fn default_plugins() -> Vec<Arc<dyn Plugin>> {
    vec![build_plugin()]
}
