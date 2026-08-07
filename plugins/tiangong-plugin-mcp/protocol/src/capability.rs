//! MCP 探测与重新配置操作。

use serde::{Deserialize, Serialize};

use crate::{Empty, McpOperation};

pub const SERVER_PROBE_OPERATION: &str = "mcp.server.probe";
pub const RECONFIGURE_OPERATION: &str = "mcp.reconfigure";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReconfigureRequest {
    /// 当前会话工作目录（用于 stdio MCP 子进程 current_dir 注入）。
    #[serde(default)]
    pub workspace: Option<String>,
}

pub struct ServerProbe;
impl McpOperation for ServerProbe {
    const NAME: &'static str = SERVER_PROBE_OPERATION;
    type Request = crate::query::ServerNameRequest;
    type Response = Empty;
}

pub struct Reconfigure;
impl McpOperation for Reconfigure {
    const NAME: &'static str = RECONFIGURE_OPERATION;
    type Request = ReconfigureRequest;
    type Response = Empty;
}
