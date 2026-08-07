//! Agent MCP 工具链路操作（tool_specs / handle_tool）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Empty, McpOperation};

pub const LIST_TOOLS_OPERATION: &str = "mcp.list_tools";
pub const EXECUTE_TOOL_OPERATION: &str = "mcp.execute_tool";

/// MCP server 暴露的工具元数据（capability 缓存项）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolMeta {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub input_schema: Value,
    #[serde(default)]
    pub argument_summaries: Vec<McpToolArgumentSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolArgumentSummary {
    pub name: String,
    pub type_name: String,
    pub required: bool,
    pub description: String,
    pub default_value: String,
    pub enum_values: Vec<String>,
}

/// tool_specs 拉取响应：每个 server 名 → 其工具列表。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListToolsResponse {
    /// (server_name, tools) 列表。
    pub servers: Vec<NamedTools>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NamedTools {
    pub server: String,
    pub tools: Vec<McpToolMeta>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecuteToolRequest {
    /// MCP server 名。
    pub server_name: String,
    /// 该 server 上的工具名（不含 `mcp__{server}__` 前缀）。
    pub tool_name: String,
    /// 工具参数（JSON 对象）。
    #[serde(default)]
    pub arguments: Value,
    /// 当前会话工作目录（stdio MCP 子进程 current_dir 注入）。
    #[serde(default)]
    pub workspace: Option<String>,
}

/// MCP 工具执行结果。
///
/// 保留与 core `ToolResult` 同构的字段，便于 sidecar 直接构造、wasm 直接透传。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecuteToolResponse {
    pub ok: bool,
    pub summary: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i64,
    pub duration_ms: u64,
    pub tool_name: String,
    pub arguments: Vec<String>,
}

/// 通用环境变量集合（exec_env 回传）。
pub type EnvMap = BTreeMap<String, String>;

pub struct ListTools;
impl McpOperation for ListTools {
    const NAME: &'static str = LIST_TOOLS_OPERATION;
    type Request = Empty;
    type Response = ListToolsResponse;
}

pub struct ExecuteTool;
impl McpOperation for ExecuteTool {
    const NAME: &'static str = EXECUTE_TOOL_OPERATION;
    type Request = ExecuteToolRequest;
    type Response = ExecuteToolResponse;
}
