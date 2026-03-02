use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use tokio::process::Command;
use tokio::runtime::Builder as TokioRuntimeBuilder;
use tokio::time::timeout;

use crate::core::agent_config::McpServerConfig;

use super::util::truncate_text;

#[derive(Debug, Clone)]
pub struct McpResourceMeta {
    pub server: String,
    pub uri: String,
}

pub trait McpClient {
    fn list_resources(
        &self,
        server: &McpServerConfig,
        timeout_ms: u64,
    ) -> Result<Vec<McpResourceMeta>>;
    fn read_resource(
        &self,
        server: &McpServerConfig,
        resource: &McpResourceMeta,
        timeout_ms: u64,
    ) -> Result<String>;
}

#[derive(Debug, Clone, Default)]
pub struct LocalMcpClient;

impl McpClient for LocalMcpClient {
    fn list_resources(
        &self,
        server: &McpServerConfig,
        timeout_ms: u64,
    ) -> Result<Vec<McpResourceMeta>> {
        if !server.enabled {
            return Ok(Vec::new());
        }

        if server.command.trim().is_empty() {
            return Ok(server
                .tags
                .iter()
                .filter_map(|tag| {
                    let tag = tag.trim();
                    if tag.is_empty() {
                        None
                    } else {
                        Some(McpResourceMeta {
                            server: server.name.clone(),
                            uri: format!("mcp://{}/{}", server.name, tag),
                        })
                    }
                })
                .collect());
        }

        let output = run_mcp_command(server, timeout_ms, "list-resources", None)?;
        parse_resource_list_output(server, &output)
    }

    fn read_resource(
        &self,
        server: &McpServerConfig,
        resource: &McpResourceMeta,
        timeout_ms: u64,
    ) -> Result<String> {
        if !server.enabled {
            return Err(anyhow!("MCP server 未启用：{}", server.name));
        }

        if server.command.trim().is_empty() {
            return Ok(format!(
                "resource={} from_server={} command={} args={}",
                resource.uri,
                server.name,
                "(empty)",
                if server.args.is_empty() {
                    "(none)".to_string()
                } else {
                    server.args.join(" ")
                }
            ));
        }

        let output = run_mcp_command(server, timeout_ms, "read-resource", Some(&resource.uri))?;
        Ok(parse_read_resource_output(&output))
    }
}

fn run_mcp_command(
    server: &McpServerConfig,
    timeout_ms: u64,
    action: &str,
    resource_uri: Option<&str>,
) -> Result<String> {
    let command = server.command.trim();
    if command.is_empty() {
        return Err(anyhow!("MCP server command 为空：{}", server.name));
    }

    let runtime = TokioRuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .context("初始化 MCP 运行时失败")?;
    let output = runtime.block_on(async {
        let mut cmd = Command::new(command);
        cmd.args(&server.args)
            .arg(action)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(uri) = resource_uri {
            cmd.arg(uri);
        }
        timeout(Duration::from_millis(timeout_ms), cmd.output()).await
    });

    let output = match output {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            return Err(anyhow!(
                "MCP命令执行失败：server={} command={} error={}",
                server.name,
                command,
                err
            ));
        }
        Err(_) => {
            return Err(anyhow!(
                "MCP命令执行超时：server={} action={} timeout_ms={}",
                server.name,
                action,
                timeout_ms
            ));
        }
    };

    if !output.status.success() {
        let exit_code = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stderr = if stderr.is_empty() {
            "(empty)".to_string()
        } else {
            truncate_text(&stderr, 160)
        };
        return Err(anyhow!(
            "MCP命令返回失败：server={} action={} exit_code={} stderr={}",
            server.name,
            action,
            exit_code,
            stderr
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        return Err(anyhow!(
            "MCP命令输出为空：server={} action={}",
            server.name,
            action
        ));
    }
    Ok(stdout)
}

fn parse_resource_list_output(
    server: &McpServerConfig,
    output: &str,
) -> Result<Vec<McpResourceMeta>> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    if let Ok(json) = serde_json::from_str::<Value>(trimmed) {
        let resources = parse_resource_list_from_json(&server.name, &json);
        if !resources.is_empty() {
            return Ok(resources);
        }
    }

    let resources = trimmed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| McpResourceMeta {
            server: server.name.clone(),
            uri: line.to_string(),
        })
        .collect::<Vec<_>>();
    Ok(resources)
}

fn parse_resource_list_from_json(server_name: &str, value: &Value) -> Vec<McpResourceMeta> {
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(parse_resource_uri)
            .map(|uri| McpResourceMeta {
                server: server_name.to_string(),
                uri,
            })
            .collect(),
        Value::Object(map) => {
            if let Some(resources) = map.get("resources") {
                return parse_resource_list_from_json(server_name, resources);
            }
            parse_resource_uri(value)
                .map(|uri| {
                    vec![McpResourceMeta {
                        server: server_name.to_string(),
                        uri,
                    }]
                })
                .unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

fn parse_resource_uri(value: &Value) -> Option<String> {
    match value {
        Value::String(uri) => {
            let uri = uri.trim();
            if uri.is_empty() {
                None
            } else {
                Some(uri.to_string())
            }
        }
        Value::Object(map) => {
            let uri = map
                .get("uri")
                .and_then(Value::as_str)
                .or_else(|| map.get("name").and_then(Value::as_str))
                .unwrap_or("")
                .trim();
            if uri.is_empty() {
                None
            } else {
                Some(uri.to_string())
            }
        }
        _ => None,
    }
}

fn parse_read_resource_output(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    if let Ok(json) = serde_json::from_str::<Value>(trimmed)
        && let Some(content) = parse_content_from_json(&json)
    {
        return content;
    }

    trimmed.to_string()
}

fn parse_content_from_json(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.trim().to_string()).filter(|text| !text.is_empty()),
        Value::Array(items) => {
            let parts = items
                .iter()
                .filter_map(parse_content_from_json)
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        Value::Object(map) => {
            for key in ["content", "text", "data"] {
                if let Some(raw) = map.get(key)
                    && let Some(parsed) = parse_content_from_json(raw)
                {
                    return Some(parsed);
                }
            }

            if let Some(contents) = map.get("contents") {
                return parse_content_from_json(contents);
            }
            None
        }
        _ => None,
    }
}
