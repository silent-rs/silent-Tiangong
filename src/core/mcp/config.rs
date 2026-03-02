use std::collections::HashSet;

use anyhow::{Result, anyhow};

use crate::core::agent_config::{McpConfig, McpServerConfig};

pub fn summarize_mcp_servers(servers: &[McpServerConfig], name_filter: Option<&str>) -> String {
    let mut lines = Vec::new();
    for (idx, server) in servers.iter().enumerate() {
        if let Some(filter) = name_filter {
            let filter = filter.trim();
            if !filter.is_empty() && server.name != filter {
                continue;
            }
        }
        let command = if server.command.trim().is_empty() {
            "(empty)"
        } else {
            server.command.trim()
        };
        let args = if server.args.is_empty() {
            "(none)".to_string()
        } else {
            server.args.join(" ")
        };
        let tags = if server.tags.is_empty() {
            "(none)".to_string()
        } else {
            server.tags.join(",")
        };
        lines.push(format!(
            "{}. name={} enabled={} command={} args={} tags={}",
            idx + 1,
            server.name,
            server.enabled,
            command,
            args,
            tags
        ));
    }

    if lines.is_empty() {
        "未找到 MCP server 配置".to_string()
    } else {
        lines.join(" | ")
    }
}

pub fn validate_mcp_config(config: &McpConfig) -> Result<()> {
    if config.timeout_ms == 0 {
        return Err(anyhow!("mcp.timeout_ms 必须大于 0"));
    }

    let mut seen_names = HashSet::new();
    if let Some(server) = config
        .servers
        .iter()
        .find(|server| server.name.trim().is_empty())
    {
        return Err(anyhow!("mcp.servers 包含空名称配置：{:?}", server));
    }

    for server in &config.servers {
        let name = server.name.trim();
        if !seen_names.insert(name.to_string()) {
            return Err(anyhow!("mcp.servers 存在重复名称：{name}"));
        }
        if server.command.trim().is_empty() && server.tags.iter().all(|tag| tag.trim().is_empty()) {
            return Err(anyhow!(
                "mcp.servers 配置无效（command/tags 不能同时为空）：{}",
                server.name
            ));
        }
    }

    Ok(())
}
