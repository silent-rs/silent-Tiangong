use crate::core::agent_config::{McpConfig, McpServerConfig};

use super::capability::cached_server_tools;
use super::client::McpToolMeta;
use super::util::{format_mcp_record, truncate_text};

pub fn build_mcp_hints(_user_input: &str, config: &McpConfig) -> Vec<String> {
    if !config.enabled || config.servers.is_empty() {
        return vec![format_mcp_record("skipped", "all", "mcp disabled or empty")];
    }

    let selected = enabled_servers(config);
    if selected.is_empty() {
        return vec![format_mcp_record("skipped", "all", "no enabled server")];
    }

    selected
        .into_iter()
        .map(|server| {
            let (tools, source) = fetch_server_tools(server);
            let tool_count = tools.len();
            let sample_tool = tools
                .first()
                .map(|item| item.compact_signature())
                .unwrap_or_else(|| "none".to_string());
            format_mcp_record(
                "ok",
                &server.name,
                &format!(
                    "source={},timeout_ms={},tools={},sample_tool={}",
                    source, config.timeout_ms, tool_count, sample_tool
                ),
            )
        })
        .collect()
}

#[allow(dead_code)]
pub fn collect_mcp_context(_user_input: &str, config: &McpConfig) -> Vec<String> {
    if !config.enabled || config.servers.is_empty() {
        return Vec::new();
    }

    const MAX_CONTEXT_ITEMS: usize = 24;
    let mut context = Vec::new();

    for server in enabled_servers(config) {
        let (tools, source) = fetch_server_tools(server);

        for tool in tools {
            let schema_state = if tool.input_schema.is_null() {
                "none".to_string()
            } else {
                let schema =
                    serde_json::to_string(&tool.input_schema).unwrap_or_else(|_| "{}".to_string());
                truncate_text(&schema, 240)
            };
            context.push(format_mcp_record(
                "ok",
                &server.name,
                &format!(
                    "source={},tool={},args={},schema={},description={}",
                    source,
                    tool.compact_signature(),
                    truncate_text(&tool.argument_brief(8), 200),
                    schema_state,
                    truncate_text(&tool.description, 120)
                ),
            ));
            if context.len() >= MAX_CONTEXT_ITEMS {
                return context;
            }
        }
    }

    context
}

fn enabled_servers(config: &McpConfig) -> Vec<&McpServerConfig> {
    config
        .servers
        .iter()
        .filter(|server| server.enabled)
        .collect()
}

fn fetch_server_tools(server: &McpServerConfig) -> (Vec<McpToolMeta>, &'static str) {
    if let Some(tools) = cached_server_tools(&server.name)
        && !tools.is_empty()
    {
        return (tools, "cache");
    }

    (Vec::new(), "cache-miss")
}
