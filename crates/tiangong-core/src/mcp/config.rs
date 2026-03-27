use std::collections::HashSet;

use anyhow::{Context, Result, anyhow};
use http::HeaderName;

use crate::agent_config::{McpConfig, McpServerConfig, ResolvedMcpTransport, is_http_endpoint};

pub fn summarize_mcp_servers(servers: &[McpServerConfig], name_filter: Option<&str>) -> String {
    let mut entries = Vec::new();
    for server in servers {
        if let Some(filter) = name_filter {
            let filter = filter.trim();
            if !filter.is_empty() && server.name != filter {
                continue;
            }
        }
        entries.push(server);
    }

    if entries.is_empty() {
        "未找到 MCP server 配置".to_string()
    } else {
        entries
            .iter()
            .map(|server| format_server_summary(server))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub fn describe_mcp_servers(servers: &[McpServerConfig], name_filter: Option<&str>) -> String {
    let mut entries = Vec::new();
    for server in servers {
        if let Some(filter) = name_filter {
            let filter = filter.trim();
            if !filter.is_empty() && server.name != filter {
                continue;
            }
        }
        entries.push(server);
    }

    if entries.is_empty() {
        "未找到 MCP server 配置".to_string()
    } else {
        entries
            .iter()
            .map(|server| format_server_detail(server))
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

fn format_server_summary(server: &McpServerConfig) -> String {
    let marker = if server.enabled {
        "\x1b[32m●\x1b[0m"
    } else {
        "\x1b[31m●\x1b[0m"
    };
    format!("{marker} {}", server.name)
}

fn format_server_detail(server: &McpServerConfig) -> String {
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
    let transport = match server.resolved_transport() {
        ResolvedMcpTransport::Stdio => "stdio",
        ResolvedMcpTransport::Http => "http",
        ResolvedMcpTransport::Metadata => "metadata",
    };
    let endpoint = server.resolved_http_endpoint().unwrap_or("(none)");
    let auth = if server.auth_header.trim().is_empty() {
        "(none)"
    } else {
        "(set)"
    };
    let tags = if server.tags.is_empty() {
        "(none)".to_string()
    } else {
        server.tags.join(",")
    };
    let status = if server.enabled {
        "\x1b[32m● enabled\x1b[0m"
    } else {
        "\x1b[31m● disabled\x1b[0m"
    };

    format!(
        "mcp name: {name}\n  status: {status}\n  transport: {transport}\n  command: {command}\n  args: {args}\n  endpoint: {endpoint}\n  auth: {auth}\n  headers: {headers}\n  env: {env}\n  cwd: {cwd}\n  tags: {tags}",
        name = server.name,
        headers = server.headers.len(),
        env = server.env.len(),
        cwd = server.cwd_text().unwrap_or("(none)")
    )
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
        if server.command.trim().is_empty()
            && server.endpoint.trim().is_empty()
            && server.tags.iter().all(|tag| tag.trim().is_empty())
        {
            return Err(anyhow!(
                "mcp.servers 配置无效（command/endpoint/tags 不能同时为空）：{}",
                server.name
            ));
        }

        if !server.endpoint.trim().is_empty() && !is_http_endpoint(&server.endpoint) {
            return Err(anyhow!(
                "mcp.servers endpoint 必须为 http/https：{} endpoint={}",
                server.name,
                server.endpoint
            ));
        }

        for key in server.headers.keys() {
            let key = key.trim();
            if key.is_empty() {
                return Err(anyhow!("mcp.servers headers 包含空键：{}", server.name));
            }
            HeaderName::from_bytes(key.as_bytes()).with_context(|| {
                format!("mcp.servers headers 键无效：{} key={key}", server.name)
            })?;
        }
        if server.headers.values().any(|value| value.trim().is_empty()) {
            return Err(anyhow!("mcp.servers headers 包含空值：{}", server.name));
        }

        if server.env.keys().any(|key| key.trim().is_empty()) {
            return Err(anyhow!("mcp.servers env 包含空键：{}", server.name));
        }

        match server.resolved_transport() {
            ResolvedMcpTransport::Metadata => {}
            ResolvedMcpTransport::Stdio => {
                if server.command_text().is_empty() {
                    return Err(anyhow!(
                        "mcp.servers stdio 模式必须配置 command：{}",
                        server.name
                    ));
                }
                if is_http_endpoint(server.command_text()) {
                    return Err(anyhow!(
                        "mcp.servers stdio 模式 command 不能是 URL：{}",
                        server.name
                    ));
                }
                if !server.endpoint.trim().is_empty() {
                    return Err(anyhow!(
                        "mcp.servers stdio 模式不支持 endpoint：{}",
                        server.name
                    ));
                }
                if !server.auth_header.trim().is_empty() || !server.headers.is_empty() {
                    return Err(anyhow!(
                        "mcp.servers stdio 模式不支持 auth_header/headers：{}",
                        server.name
                    ));
                }
            }
            ResolvedMcpTransport::Http => {
                if server.resolved_http_endpoint().is_none() {
                    return Err(anyhow!(
                        "mcp.servers http 模式必须配置 endpoint 或 URL command：{}",
                        server.name
                    ));
                }
                if !server.args.is_empty() {
                    return Err(anyhow!("mcp.servers http 模式不支持 args：{}", server.name));
                }
                if !server.env.is_empty() || server.cwd_text().is_some() {
                    return Err(anyhow!(
                        "mcp.servers http 模式不支持 env/cwd：{}",
                        server.name
                    ));
                }
            }
        }
    }

    Ok(())
}
