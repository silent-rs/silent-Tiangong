//! MCP 管理 facade 已迁移至 `tiangong-plugin-mcp`。
//!
//! 原 `TiangongState` 上的 MCP 管理方法（register/update/remove/set_enabled/
//! mcp_servers/mcp_server_summary/mcp_server_detail/mcp_server_cached_tools/
//! update_agent_config_entry）全部迁至 `McpPlugin` 固有方法，由入口层
//!（Tauri/CLI）持有 `Arc<McpPlugin>` 调用（dual-ownership）。
//!
//! `agent_config_summary` / `validate_agent_config` 不再涉及 MCP（MCP 配置
//! 已从 AgentConfig 脱离），如需保留请改读 plugin。
use super::super::*;

impl TiangongState {
    /// 校验 agent 配置（MCP 配置已脱离，此处仅保留接口供未来扩展）。
    pub fn validate_agent_config(&self) -> Result<()> {
        validate_agent_config(&self.store.agent.agent_config)
    }
}
