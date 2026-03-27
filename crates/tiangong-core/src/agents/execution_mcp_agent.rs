use std::collections::{HashMap, HashSet};
use std::time::Instant;

use anyhow::{Result, anyhow};
use serde_json::Value;

use crate::agent_config::{McpConfig, McpServerConfig};
use crate::mcp::{LocalMcpClient, McpClient, McpToolMeta, cached_active_tools};
use crate::model::{FunctionToolSpec, ModelFunctionCall};
use crate::tool::{ToolExecutionRecord, ToolResult};

use super::execution_tool_agent::{basic_file_function_tools, elapsed_ms_u64, split_command_parts};

#[derive(Debug, Clone)]
pub(crate) struct McpFunctionTarget {
    pub(crate) server_name: String,
    pub(crate) tool_name: String,
}
pub(crate) fn execution_function_tools(
    mcp_config: &McpConfig,
) -> (Vec<FunctionToolSpec>, HashMap<String, McpFunctionTarget>) {
    let mut tools = basic_file_function_tools();
    let mut reserved_names = tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<HashSet<_>>();
    let mut bindings = HashMap::new();

    let active = cached_active_tools();
    let mut tool_name_count = HashMap::<String, usize>::new();
    for (_server, server_tools) in &active {
        for tool in server_tools {
            let name = tool.name.trim();
            if name.is_empty() {
                continue;
            }
            *tool_name_count.entry(name.to_string()).or_insert(0) += 1;
        }
    }

    for (server_name, server_tools) in active {
        for tool in server_tools {
            let raw_tool_name = tool.name.trim();
            if raw_tool_name.is_empty() {
                continue;
            }
            if !mcp_config
                .servers
                .iter()
                .any(|server| server.enabled && server.name == server_name)
            {
                continue;
            }

            let mut function_name = resolve_mcp_function_name(
                &server_name,
                raw_tool_name,
                &tool_name_count,
                &reserved_names,
            );
            let mut suffix = 2usize;
            while reserved_names.contains(&function_name) {
                function_name = format!("{}_{}", function_name, suffix);
                suffix += 1;
            }
            reserved_names.insert(function_name.clone());

            tools.push(function_tool_from_mcp_tool(
                &function_name,
                &server_name,
                &tool,
            ));
            bindings.insert(
                function_name,
                McpFunctionTarget {
                    server_name: server_name.clone(),
                    tool_name: raw_tool_name.to_string(),
                },
            );
        }
    }

    (tools, bindings)
}

fn resolve_mcp_function_name(
    server_name: &str,
    tool_name: &str,
    tool_name_count: &HashMap<String, usize>,
    reserved_names: &HashSet<String>,
) -> String {
    let count = *tool_name_count.get(tool_name).unwrap_or(&1);
    if count == 1 && !reserved_names.contains(tool_name) {
        return tool_name.to_string();
    }
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

fn function_tool_from_mcp_tool(
    function_name: &str,
    server_name: &str,
    tool: &McpToolMeta,
) -> FunctionToolSpec {
    FunctionToolSpec {
        name: function_name.to_string(),
        description: format!(
            "MCP调用：server={} tool={} description={}",
            server_name, tool.name, tool.description
        ),
        parameters: if tool.input_schema.is_object() {
            tool.input_schema.clone()
        } else {
            serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            })
        },
    }
}

pub(crate) fn execute_mcp_tool_call(
    call: &ModelFunctionCall,
    target: &McpFunctionTarget,
    mcp_config: &McpConfig,
) -> Result<ToolResult> {
    execute_mcp_tool_call_with_args(target, normalize_mcp_call_arguments(call), mcp_config)
}

pub(crate) fn execute_mcp_tool_call_with_args(
    target: &McpFunctionTarget,
    args: Value,
    mcp_config: &McpConfig,
) -> Result<ToolResult> {
    let started = Instant::now();
    let server = find_mcp_server(mcp_config, &target.server_name).ok_or_else(|| {
        anyhow!(
            "MCP server 不存在或未启用：server={} tool={}",
            target.server_name,
            target.tool_name
        )
    })?;
    let client = LocalMcpClient;
    match client.call_tool(
        server,
        &target.tool_name,
        args.clone(),
        mcp_config.timeout_ms,
    ) {
        Ok(stdout) => Ok(ToolResult {
            ok: true,
            summary: format!(
                "MCP工具调用成功：server={} tool={}",
                target.server_name, target.tool_name
            ),
            stdout,
            stderr: String::new(),
            exit_code: 0,
            execution: Some(ToolExecutionRecord {
                tool_name: format!("mcp::{}::{}", target.server_name, target.tool_name),
                args: vec![serde_json::to_string(&args).unwrap_or_default()],
                duration_ms: elapsed_ms_u64(started.elapsed().as_millis()),
                ok: true,
                exit_code: 0,
                summary: format!(
                    "MCP工具调用成功：server={} tool={}",
                    target.server_name, target.tool_name
                ),
            }),
        }),
        Err(err) => Ok(ToolResult {
            ok: false,
            summary: format!(
                "MCP工具调用失败：server={} tool={} error={}",
                target.server_name, target.tool_name, err
            ),
            stdout: String::new(),
            stderr: err.to_string(),
            exit_code: 1,
            execution: Some(ToolExecutionRecord {
                tool_name: format!("mcp::{}::{}", target.server_name, target.tool_name),
                args: vec![serde_json::to_string(&args).unwrap_or_default()],
                duration_ms: elapsed_ms_u64(started.elapsed().as_millis()),
                ok: false,
                exit_code: 1,
                summary: format!(
                    "MCP工具调用失败：server={} tool={}",
                    target.server_name, target.tool_name
                ),
            }),
        }),
    }
}

pub(crate) fn build_internal_tool_error_result(tool_name: &str, error: &str) -> ToolResult {
    let message = format!("工具路由失败：tool={} error={}", tool_name, error);
    ToolResult {
        ok: false,
        summary: message.clone(),
        stdout: String::new(),
        stderr: error.to_string(),
        exit_code: 1,
        execution: Some(ToolExecutionRecord {
            tool_name: tool_name.to_string(),
            args: Vec::new(),
            duration_ms: 0,
            ok: false,
            exit_code: 1,
            summary: message,
        }),
    }
}

pub(crate) fn normalize_mcp_call_arguments(call: &ModelFunctionCall) -> Value {
    if call.arguments.is_object() {
        call.arguments.clone()
    } else {
        serde_json::json!({})
    }
}

pub(crate) fn resolve_mcp_tool_call_from_run_command(
    call: &ModelFunctionCall,
    mcp_targets: &HashMap<String, McpFunctionTarget>,
    mcp_config: &McpConfig,
) -> Option<(McpFunctionTarget, Value)> {
    if call.name != "run_command" {
        return None;
    }
    if call
        .arguments
        .get("args")
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty())
    {
        return None;
    }
    if call
        .arguments
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|text| !text.is_empty())
    {
        return None;
    }
    let raw_cmd = call
        .arguments
        .get("cmd")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    let mut parts = split_command_parts(raw_cmd)?;
    if parts.len() != 1 {
        return None;
    }
    let tool_name = parts.remove(0);
    let target = mcp_targets
        .get(&tool_name)
        .cloned()
        .or_else(|| resolve_unique_mcp_target_by_raw_name(&tool_name, mcp_config))?;
    Some((target, serde_json::json!({})))
}

pub(crate) fn resolve_unique_mcp_target_by_raw_name(
    tool_name: &str,
    mcp_config: &McpConfig,
) -> Option<McpFunctionTarget> {
    let mut hit_server = None::<String>;
    for (server_name, tools) in cached_active_tools() {
        if !mcp_config
            .servers
            .iter()
            .any(|server| server.enabled && server.name == server_name)
        {
            continue;
        }
        if !tools.iter().any(|tool| tool.name.trim() == tool_name) {
            continue;
        }
        if hit_server.is_some() {
            return None;
        }
        hit_server = Some(server_name);
    }
    hit_server.map(|server_name| McpFunctionTarget {
        server_name,
        tool_name: tool_name.to_string(),
    })
}

fn find_mcp_server<'a>(config: &'a McpConfig, name: &str) -> Option<&'a McpServerConfig> {
    config
        .servers
        .iter()
        .find(|server| server.enabled && server.name == name)
}
