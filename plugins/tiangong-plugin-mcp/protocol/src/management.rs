//! MCP server 管理 CRUD 操作。

use serde::{Deserialize, Serialize};

use crate::config::{
    McpConfig, McpServerConfig, RegisterMcpServerRequest, UpdateConfigEntryRequest,
};
use crate::{Empty, McpOperation, MessageResponse};

pub const CONFIG_GET_OPERATION: &str = "mcp.config.get";
pub const CONFIG_SNAPSHOT_OPERATION: &str = "mcp.config.snapshot";
pub const CONFIG_UPDATE_ENTRY_OPERATION: &str = "mcp.config.update_entry";
pub const SERVER_REGISTER_OPERATION: &str = "mcp.server.register";
pub const SERVER_UPDATE_OPERATION: &str = "mcp.server.update";
pub const SERVER_REMOVE_OPERATION: &str = "mcp.server.remove";
pub const SERVER_SET_ENABLED_OPERATION: &str = "mcp.server.set_enabled";
pub const SERVER_MERGE_DISK_OPERATION: &str = "mcp.server.merge_disk";

/// 更新已有 server 的请求（name 作为主键，其余字段就地更新）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateServerRequest {
    pub name: String,
    #[serde(flatten)]
    pub request: RegisterMcpServerRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoveServerRequest {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetEnabledRequest {
    pub name: String,
    pub enabled: bool,
}

pub struct ConfigGet;
impl McpOperation for ConfigGet {
    const NAME: &'static str = CONFIG_GET_OPERATION;
    type Request = Empty;
    type Response = McpConfig;
}

pub struct ConfigSnapshot;
impl McpOperation for ConfigSnapshot {
    const NAME: &'static str = CONFIG_SNAPSHOT_OPERATION;
    type Request = Empty;
    type Response = McpConfig;
}

pub struct UpdateConfigEntry;
impl McpOperation for UpdateConfigEntry {
    const NAME: &'static str = CONFIG_UPDATE_ENTRY_OPERATION;
    type Request = UpdateConfigEntryRequest;
    type Response = MessageResponse;
}

pub struct ServerRegister;
impl McpOperation for ServerRegister {
    const NAME: &'static str = SERVER_REGISTER_OPERATION;
    type Request = RegisterMcpServerRequest;
    type Response = MessageResponse;
}

pub struct ServerUpdate;
impl McpOperation for ServerUpdate {
    const NAME: &'static str = SERVER_UPDATE_OPERATION;
    type Request = UpdateServerRequest;
    type Response = MessageResponse;
}

pub struct ServerRemove;
impl McpOperation for ServerRemove {
    const NAME: &'static str = SERVER_REMOVE_OPERATION;
    type Request = RemoveServerRequest;
    type Response = MessageResponse;
}

pub struct ServerSetEnabled;
impl McpOperation for ServerSetEnabled {
    const NAME: &'static str = SERVER_SET_ENABLED_OPERATION;
    type Request = SetEnabledRequest;
    type Response = MessageResponse;
}

pub struct ServerMergeDisk;
impl McpOperation for ServerMergeDisk {
    const NAME: &'static str = SERVER_MERGE_DISK_OPERATION;
    type Request = Empty;
    type Response = MessageResponse;
}

/// server 列表响应（入口层展示用）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServersResponse {
    pub servers: Vec<McpServerConfig>,
}
