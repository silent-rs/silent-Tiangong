//! MCP 工具执行适配器（sidecar 版，不依赖 core）。
//!
//! 将动态 MCP 工具缓存转换为 protocol 工具规格列表，并执行单个 MCP 工具调用。

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Result, anyhow};
use serde_json::Value;

use tiangong_plugin_mcp_protocol::config::{McpConfig, McpServerConfig};
use tiangong_plugin_mcp_protocol::tool::{
    ExecuteToolResponse, ListToolsResponse, McpToolMeta, NamedTools,
};

use crate::client::{LocalMcpClient, McpClient};

#[derive(Debug, Clone)]
pub struct McpFunctionTarget {
    pub server_name: String,
    pub tool_name: String,
}

/// 构建 sidecar 的 `list_tools` 响应：每个健康 server → 其工具列表。
pub fn list_tools_response(
    mcp_config: &McpConfig,
    active: Vec<(String, Vec<McpToolMeta>)>,
) -> ListToolsResponse {
    if !mcp_config.enabled {
        return ListToolsResponse::default();
    }
    let mut servers = Vec::new();
    for (server_name, server_tools) in active {
        if !mcp_config
            .servers
            .iter()
            .any(|server| server.enabled && server.name == server_name)
        {
            continue;
        }
        let tools: Vec<McpToolMeta> = server_tools
            .into_iter()
            .filter(|tool| !tool.name.trim().is_empty())
            .collect();
        if !tools.is_empty() {
            servers.push(NamedTools {
                server: server_name,
                tools,
            });
        }
    }
    ListToolsResponse { servers }
}

/// 工具名→目标绑定（用于 sidecar 内部按名查找执行目标）。
pub fn build_targets(
    mcp_config: &McpConfig,
    active: Vec<(String, Vec<McpToolMeta>)>,
) -> HashMap<String, McpFunctionTarget> {
    if !mcp_config.enabled {
        return HashMap::new();
    }
    let mut bindings = HashMap::new();
    for (server_name, server_tools) in active {
        if !mcp_config
            .servers
            .iter()
            .any(|server| server.enabled && server.name == server_name)
        {
            continue;
        }
        for tool in server_tools {
            let raw_tool_name = tool.name.trim();
            if raw_tool_name.is_empty() {
                continue;
            }
            let function_name = resolve_mcp_function_name(&server_name, raw_tool_name);
            bindings.insert(
                function_name,
                McpFunctionTarget {
                    server_name: server_name.clone(),
                    tool_name: raw_tool_name.to_string(),
                },
            );
        }
    }
    bindings
}

/// 生成 MCP 工具的 LLM 可见函数名：`mcp__{server}__{tool}`。
pub fn resolve_mcp_function_name(server_name: &str, tool_name: &str) -> String {
    format!(
        "mcp__{}__{}",
        sanitize_fn_name(server_name),
        sanitize_fn_name(tool_name)
    )
}

fn sanitize_fn_name(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "mcp_tool".to_string()
    } else {
        trimmed
    }
}

/// 执行单个 MCP 工具调用。
pub async fn execute_tool(
    target: &McpFunctionTarget,
    args: Value,
    mcp_config: &McpConfig,
    workspace: Option<PathBuf>,
) -> Result<ExecuteToolResponse> {
    let started = Instant::now();
    let server = find_mcp_server(mcp_config, &target.server_name).ok_or_else(|| {
        anyhow!(
            "MCP server 不存在或未启用：server={} tool={}",
            target.server_name,
            target.tool_name
        )
    })?;
    let client = LocalMcpClient { workspace };
    match client
        .call_tool(
            server,
            &target.tool_name,
            args.clone(),
            mcp_config.timeout_ms,
        )
        .await
    {
        Ok(stdout) => Ok(ExecuteToolResponse {
            ok: true,
            summary: format!(
                "MCP工具调用成功：server={} tool={}",
                target.server_name, target.tool_name
            ),
            stdout,
            stderr: String::new(),
            exit_code: 0,
            duration_ms: elapsed_ms_u64(started.elapsed().as_millis()),
            tool_name: format!("mcp::{}::{}", target.server_name, target.tool_name),
            arguments: vec![serde_json::to_string(&args).unwrap_or_default()],
        }),
        Err(err) => Ok(ExecuteToolResponse {
            ok: false,
            summary: format!(
                "MCP工具调用失败：server={} tool={} error={}",
                target.server_name, target.tool_name, err
            ),
            stdout: String::new(),
            stderr: err.to_string(),
            exit_code: 1,
            duration_ms: elapsed_ms_u64(started.elapsed().as_millis()),
            tool_name: format!("mcp::{}::{}", target.server_name, target.tool_name),
            arguments: vec![serde_json::to_string(&args).unwrap_or_default()],
        }),
    }
}

fn find_mcp_server<'a>(config: &'a McpConfig, name: &str) -> Option<&'a McpServerConfig> {
    if !config.enabled {
        return None;
    }
    config
        .servers
        .iter()
        .find(|server| server.enabled && server.name == name)
}

fn elapsed_ms_u64(ms: u128) -> u64 {
    u64::try_from(ms).unwrap_or(u64::MAX)
}
