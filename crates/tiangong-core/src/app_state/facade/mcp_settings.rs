use super::super::*;

impl TiangongState {
    pub fn agent_config_summary(&self) -> String {
        format!(
            "skills.enabled={}, skills.max_matches={}, skills.dirs={}, skills.installed={}, mcp.enabled={}, mcp.timeout_ms={}, mcp.servers={}",
            self.store.agent.agent_config.skills.enabled,
            self.store.agent.agent_config.skills.max_matches,
            self.store.agent.agent_config.skills.dirs.len(),
            self.store.agent.agent_config.skills.installed.len(),
            self.store.agent.agent_config.mcp.enabled,
            self.store.agent.agent_config.mcp.timeout_ms,
            self.store.agent.agent_config.mcp.servers.len()
        )
    }

    pub fn validate_agent_config(&self) -> Result<()> {
        validate_agent_config(&self.store.agent.agent_config)
    }

    pub fn mcp_server_summary(&self, name_filter: Option<&str>) -> String {
        summarize_mcp_servers(&self.store.agent.agent_config.mcp.servers, name_filter)
    }

    pub fn mcp_server_detail(&self, name_filter: Option<&str>) -> String {
        describe_mcp_servers(&self.store.agent.agent_config.mcp.servers, name_filter)
    }

    pub fn mcp_servers(&self) -> &[McpServerConfig] {
        &self.store.agent.agent_config.mcp.servers
    }

    pub fn mcp_server_cached_tools(&self, name: &str) -> Option<Vec<McpToolMeta>> {
        cached_server_tools(name)
    }

    pub fn register_mcp_server(&mut self, request: RegisterMcpServerRequest) -> Result<String> {
        let service = self.services.mcp_service;
        service.register_mcp_server(self, request)
    }

    pub fn remove_mcp_server(&mut self, name: &str) -> Result<String> {
        let service = self.services.mcp_service;
        service.remove_mcp_server(self, name)
    }

    pub fn set_mcp_server_enabled(&mut self, name: &str, enabled: bool) -> Result<String> {
        let service = self.services.mcp_service;
        service.set_mcp_server_enabled(self, name, enabled)
    }

    pub fn update_mcp_server(
        &mut self,
        name: &str,
        request: RegisterMcpServerRequest,
    ) -> Result<String> {
        let service = self.services.mcp_service;
        service.update_mcp_server(self, name, request)
    }

    pub fn update_agent_config_entry(&mut self, key: &str, value: &str) -> Result<String> {
        let service = self.services.mcp_service;
        service.update_agent_config_entry(self, key, value)
    }
}
