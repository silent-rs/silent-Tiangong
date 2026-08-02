//! MCP 查询操作（列表、详情、摘要、健康、缓存工具）。

use serde::{Deserialize, Serialize};

use crate::management::ServersResponse;
use crate::tool::McpToolMeta;
use crate::{Empty, McpOperation, NameFilterRequest};

pub const SERVER_LIST_OPERATION: &str = "mcp.server.list";
pub const SERVER_CACHED_TOOLS_OPERATION: &str = "mcp.server.cached_tools";
pub const SERVER_HEALTH_OPERATION: &str = "mcp.server.health";
pub const SERVER_SUMMARY_OPERATION: &str = "mcp.server.summary";
pub const SERVER_DETAIL_OPERATION: &str = "mcp.server.detail";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerNameRequest {
    pub name: String,
}

/// 单个 server 缓存的工具列表响应（@mcp 提及补全、前端展示用）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolsResponse {
    pub tools: Vec<McpToolMeta>,
}

/// 所有 server 的健康状态。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HealthResponse {
    pub statuses: Vec<McpServerHealthStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerHealthStatus {
    pub name: String,
    pub healthy: bool,
    pub tool_count: usize,
    pub last_error: Option<String>,
    pub server_version: Option<String>,
}

/// 文本摘要/详情响应（CLI `mcp list` / `mcp show`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TextResponse {
    pub text: String,
}

pub struct ServerList;
impl McpOperation for ServerList {
    const NAME: &'static str = SERVER_LIST_OPERATION;
    type Request = Empty;
    type Response = ServersResponse;
}

pub struct ServerCachedTools;
impl McpOperation for ServerCachedTools {
    const NAME: &'static str = SERVER_CACHED_TOOLS_OPERATION;
    type Request = ServerNameRequest;
    type Response = ToolsResponse;
}

pub struct ServerHealth;
impl McpOperation for ServerHealth {
    const NAME: &'static str = SERVER_HEALTH_OPERATION;
    type Request = Empty;
    type Response = HealthResponse;
}

pub struct ServerSummary;
impl McpOperation for ServerSummary {
    const NAME: &'static str = SERVER_SUMMARY_OPERATION;
    type Request = NameFilterRequest;
    type Response = TextResponse;
}

pub struct ServerDetail;
impl McpOperation for ServerDetail {
    const NAME: &'static str = SERVER_DETAIL_OPERATION;
    type Request = NameFilterRequest;
    type Response = TextResponse;
}
