mod capability;
mod client;
mod config;
mod context;
mod util;

pub use capability::{
    McpServerHealthStatus, build_mcp_tools_system_prompt, cached_active_tools, cached_server_tools,
    configure_mcp_capability_scheduler, load_mcp_capabilities_cache, mcp_server_health_statuses,
    refresh_mcp_capabilities_async,
};
pub use client::{LocalMcpClient, McpClient, McpToolMeta};
pub use config::{describe_mcp_servers, summarize_mcp_servers, validate_mcp_config};
#[allow(unused_imports)]
pub use context::{build_mcp_hints, collect_mcp_context};

#[cfg(test)]
#[path = "tests/mod.rs"]
mod tests;
