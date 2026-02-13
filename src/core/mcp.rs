use anyhow::{Result, anyhow};

use crate::core::agent_config::{McpConfig, McpServerConfig};

#[derive(Debug, Clone)]
pub struct McpResourceMeta {
    pub server: String,
    pub uri: String,
}

pub trait McpClient {
    fn list_resources(&self, server: &McpServerConfig) -> Vec<McpResourceMeta>;
    fn read_resource(&self, server: &McpServerConfig, resource: &McpResourceMeta)
    -> Result<String>;
}

#[derive(Debug, Clone, Default)]
pub struct LocalMcpClient;

impl McpClient for LocalMcpClient {
    fn list_resources(&self, server: &McpServerConfig) -> Vec<McpResourceMeta> {
        if !server.enabled {
            return Vec::new();
        }

        server
            .tags
            .iter()
            .map(|tag| McpResourceMeta {
                server: server.name.clone(),
                uri: format!("mcp://{}/{}", server.name, tag),
            })
            .collect()
    }

    fn read_resource(
        &self,
        server: &McpServerConfig,
        resource: &McpResourceMeta,
    ) -> Result<String> {
        if !server.enabled {
            return Err(anyhow!("MCP server 未启用：{}", server.name));
        }
        Ok(format!(
            "resource={} from_server={} command={} args={}",
            resource.uri,
            server.name,
            if server.command.trim().is_empty() {
                "(empty)"
            } else {
                server.command.trim()
            },
            if server.args.is_empty() {
                "(none)".to_string()
            } else {
                server.args.join(" ")
            }
        ))
    }
}

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

    let mut hints = Vec::new();

    for server in matched_servers(user_input, config) {
        let resources = client.list_resources(server);
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
        let resources = client.list_resources(server);
        if resources.is_empty() {
            continue;
        }

        for resource in resources {
            let line = match client.read_resource(server, &resource) {
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
                    &format!("uri={},error={}", resource.uri, err),
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

fn matched_servers<'a>(user_input: &'a str, config: &'a McpConfig) -> Vec<&'a McpServerConfig> {
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

fn format_mcp_record(status: &str, server: &str, detail: &str) -> String {
    format!("mcp|{status}|server={server}|detail={detail}")
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    let mut out = text.chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        out.push_str("...");
    }
    out
}
