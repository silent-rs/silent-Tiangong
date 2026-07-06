//! MCP 工具执行适配器。
//!
//! 原属 `tiangong-core::agents::execution_mcp_agent`，MCP 管理插件化后迁入本 crate。
//! 将动态 MCP 工具缓存转换为静态 [`ToolSpec`]，并将每个 LLM 可见的函数名绑定回
//! `(server, tool)` 目标。

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use anyhow::{Result, anyhow};
use serde_json::Value;

use tiangong_core::model::{ToolCall, ToolSpec};
use tiangong_core::tool::{ToolExecutionRecord, ToolResult};

use crate::client::{LocalMcpClient, McpClient, McpToolMeta};
use crate::config::{McpConfig, McpServerConfig};

#[derive(Debug, Clone)]
pub struct McpFunctionTarget {
    pub server_name: String,
    pub tool_name: String,
}

/// 基于显式传入的 capability 缓存构建工具规格 + 目标绑定。
///
/// `active` 由调用方从 plugin 实例的 [`crate::capability::McpCapabilityIndex`]
/// 获取，保持实例隔离（不再读全局 static）。
pub fn execution_function_tools(
    mcp_config: &McpConfig,
    active: Vec<(String, Vec<McpToolMeta>)>,
    _reserved_names: HashSet<String>,
) -> (Vec<ToolSpec>, HashMap<String, McpFunctionTarget>) {
    // core 内置工具 spec 已全部迁出至进程内插件（fs/fetch/command/browser/terminal），
    // 此处仅收集 MCP 工具。plugin_injection / plan 控制等 synthetic tool 由 core/mod.rs
    // 工具汇总阶段单独注入。
    //
    // 命名策略：所有 MCP 工具统一使用 `mcp__{server}__{tool}` 前缀格式，天然避免与
    // 内置插件工具名（read_file / web_fetch / run_command 等）冲突，且来源可辨识。
    // 不再使用「无冲突时用原名、冲突加 _2 后缀」的旧规则——统一前缀更一致。
    // `_reserved_names` 参数保留以兼容签名，内部不再使用（前缀格式已无冲突风险）。
    build_tools_from_cache(mcp_config, active)
}

/// 内部：从 capability 缓存构建 (specs, bindings)。
fn build_tools_from_cache(
    mcp_config: &McpConfig,
    active: Vec<(String, Vec<McpToolMeta>)>,
) -> (Vec<ToolSpec>, HashMap<String, McpFunctionTarget>) {
    // 顶层 mcp.enabled=false 时完全不暴露 MCP 工具（全局禁用开关）。
    if !mcp_config.enabled {
        return (Vec::new(), HashMap::new());
    }
    let mut tools = Vec::new();
    let mut bindings = HashMap::new();

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

            let function_name = resolve_mcp_function_name(&server_name, raw_tool_name);

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

/// 生成 MCP 工具的 LLM 可见函数名：始终使用 `mcp__{server}__{tool}` 前缀格式。
///
/// 统一前缀天然避免与内置插件工具名冲突，且让 MCP 工具来源一目了然。
pub(crate) fn resolve_mcp_function_name(server_name: &str, tool_name: &str) -> String {
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

pub(crate) fn function_tool_from_mcp_tool(
    function_name: &str,
    server_name: &str,
    tool: &crate::client::McpToolMeta,
) -> ToolSpec {
    ToolSpec {
        name: function_name.to_string(),
        description: format!(
            "MCP调用：server={} tool={} description={}",
            server_name, tool.name, tool.description
        ),
        input_schema: if tool.input_schema.is_object() {
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

pub async fn execute_mcp_tool_call(
    call: &ToolCall,
    target: &McpFunctionTarget,
    mcp_config: &McpConfig,
) -> Result<ToolResult> {
    execute_mcp_tool_call_with_args(target, normalize_mcp_call_arguments(call), mcp_config).await
}

pub async fn execute_mcp_tool_call_with_args(
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
    match client
        .call_tool(
            server,
            &target.tool_name,
            args.clone(),
            mcp_config.timeout_ms,
        )
        .await
    {
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

pub fn normalize_mcp_call_arguments(call: &ToolCall) -> Value {
    if call.arguments.is_object() {
        call.arguments.clone()
    } else {
        serde_json::json!({})
    }
}

pub fn resolve_mcp_tool_call_from_run_command(
    call: &ToolCall,
    mcp_targets: &HashMap<String, McpFunctionTarget>,
    mcp_config: &McpConfig,
    active: &[(String, Vec<McpToolMeta>)],
) -> Option<(McpFunctionTarget, Value)> {
    // 兼容 run_command 与 run_shell：模型可能把 MCP 工具名误作为 shell 命令调用。
    if !matches!(call.name.as_str(), "run_command" | "run_shell") {
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
        .or_else(|| resolve_unique_mcp_target_by_raw_name(&tool_name, mcp_config, active))?;
    Some((target, serde_json::json!({})))
}

pub fn resolve_unique_mcp_target_by_raw_name(
    tool_name: &str,
    mcp_config: &McpConfig,
    active: &[(String, Vec<McpToolMeta>)],
) -> Option<McpFunctionTarget> {
    let mut hit_server = None::<String>;
    for (server_name, tools) in active {
        if !mcp_config
            .servers
            .iter()
            .any(|server| server.enabled && &server.name == server_name)
        {
            continue;
        }
        if !tools.iter().any(|tool| tool.name.trim() == tool_name) {
            continue;
        }
        if hit_server.is_some() {
            return None;
        }
        hit_server = Some(server_name.clone());
    }
    hit_server.map(|server_name| McpFunctionTarget {
        server_name,
        tool_name: tool_name.to_string(),
    })
}

fn find_mcp_server<'a>(config: &'a McpConfig, name: &str) -> Option<&'a McpServerConfig> {
    // 顶层 mcp.enabled=false 时禁止执行任何 MCP 工具（执行兜底，防止 config 切换后
    // 已有 targets 仍可调用）。
    if !config.enabled {
        return None;
    }
    config
        .servers
        .iter()
        .find(|server| server.enabled && server.name == name)
}

/// 拆分命令字符串为参数列表（支持引号、转义）。
fn split_command_parts(raw: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    for ch in raw.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        match ch {
            '\\' if !in_single => {
                escaped = true;
            }
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if escaped || in_single || in_double {
        return None;
    }
    if !current.is_empty() {
        out.push(current);
    }
    if out.is_empty() { None } else { Some(out) }
}

fn elapsed_ms_u64(ms: u128) -> u64 {
    u64::try_from(ms).unwrap_or(u64::MAX)
}
