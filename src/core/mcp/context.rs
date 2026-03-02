use crate::core::agent_config::{McpConfig, McpServerConfig};

use super::client::{LocalMcpClient, McpClient};
use super::util::{format_mcp_record, truncate_text};

pub fn build_mcp_hints(user_input: &str, config: &McpConfig) -> Vec<String> {
    let client = LocalMcpClient;
    build_mcp_hints_with_client(user_input, config, &client)
}

pub fn collect_mcp_context(user_input: &str, config: &McpConfig) -> Vec<String> {
    let client = LocalMcpClient;
    collect_mcp_context_with_client(user_input, config, &client)
}

fn build_mcp_hints_with_client(
    user_input: &str,
    config: &McpConfig,
    client: &impl McpClient,
) -> Vec<String> {
    if !config.enabled || config.servers.is_empty() {
        return vec![format_mcp_record("skipped", "all", "mcp disabled or empty")];
    }

    let selected = matched_servers(user_input, config);
    if selected.is_empty() {
        return vec![format_mcp_record("skipped", "all", "no matched server")];
    }

    let mut hints = Vec::new();
    for server in selected {
        match client.list_resources(server, config.timeout_ms) {
            Ok(resources) => {
                let sample = resources
                    .first()
                    .map(|item| format!("{}@{}", item.server, item.uri))
                    .unwrap_or_else(|| "none".to_string());
                hints.push(format_mcp_record(
                    "ok",
                    &server.name,
                    &format!(
                        "timeout_ms={},resources={},sample={}",
                        config.timeout_ms,
                        resources.len(),
                        sample
                    ),
                ));
            }
            Err(err) => hints.push(format_mcp_record(
                "error",
                &server.name,
                &format!(
                    "timeout_ms={},action=list,error={}",
                    config.timeout_ms,
                    truncate_text(&err.to_string(), 160)
                ),
            )),
        }
    }

    hints
}

fn collect_mcp_context_with_client(
    user_input: &str,
    config: &McpConfig,
    client: &impl McpClient,
) -> Vec<String> {
    if !config.enabled || config.servers.is_empty() {
        return Vec::new();
    }

    const MAX_CONTEXT_ITEMS: usize = 4;
    let mut context = Vec::new();

    for server in matched_servers(user_input, config) {
        let resources = match client.list_resources(server, config.timeout_ms) {
            Ok(resources) => resources,
            Err(err) => {
                context.push(format_mcp_record(
                    "error",
                    &server.name,
                    &format!("action=list,error={}", truncate_text(&err.to_string(), 160)),
                ));
                if context.len() >= MAX_CONTEXT_ITEMS {
                    return context;
                }
                continue;
            }
        };
        if resources.is_empty() {
            continue;
        }

        for resource in resources {
            let line = match client.read_resource(server, &resource, config.timeout_ms) {
                Ok(content) => format_mcp_record(
                    "ok",
                    &server.name,
                    &format!(
                        "uri={},content={}",
                        resource.uri,
                        truncate_text(&content, 160)
                    ),
                ),
                Err(err) => format_mcp_record(
                    "error",
                    &server.name,
                    &format!(
                        "uri={},error={}",
                        resource.uri,
                        truncate_text(&err.to_string(), 160)
                    ),
                ),
            };
            context.push(line);
            if context.len() >= MAX_CONTEXT_ITEMS {
                return context;
            }
        }
    }

    context
}

pub(super) fn matched_servers<'a>(
    user_input: &'a str,
    config: &'a McpConfig,
) -> Vec<&'a McpServerConfig> {
    let input = user_input.to_ascii_lowercase();
    let mut servers = Vec::new();

    for server in config.servers.iter().filter(|server| server.enabled) {
        let name = server.name.to_ascii_lowercase();
        let mut matched = input.contains(&name);
        if !matched {
            matched = server
                .tags
                .iter()
                .map(|tag| tag.to_ascii_lowercase())
                .any(|tag| !tag.is_empty() && input.contains(&tag));
        }

        if !matched
            && (input.contains("网页") || input.contains("浏览器") || input.contains("页面"))
            && (name.contains("chrome") || name.contains("browser") || name.contains("web"))
        {
            matched = true;
        }

        if !matched
            && (input.contains("数据库") || input.contains("sql") || input.contains("表"))
            && (name.contains("db") || name.contains("sql") || name.contains("postgres"))
        {
            matched = true;
        }

        if matched {
            servers.push(server);
        }
    }

    servers
}
