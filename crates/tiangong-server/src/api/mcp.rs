use silent::prelude::*;

use super::{AuthToken, SharedAppContext};
use crate::auth::check_auth;

/// GET /api/v1/mcp — MCP server 列表
///
/// 经运行时 sidecar 通道拉取 MCP server 列表。
pub async fn list_mcp(req: Request) -> Result<Response> {
    let token = req.get_state::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let _app_ctx = req.get_state::<SharedAppContext>()?.clone();
    let response: tiangong_plugin_mcp_protocol::management::ServersResponse =
        serde_json::from_value(tiangong_plugin_runtime::registry::invoke_sidecar(
            &tiangong_config::io::storage_root(),
            "mcp",
            "mcp.server.list",
            serde_json::json!({}),
        )?)?;
    let servers: Vec<serde_json::Value> = response
        .servers
        .iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name,
                "command": s.command,
                "args": s.args,
                "enabled": s.enabled,
                "tags": s.tags,
            })
        })
        .collect();

    Ok(Response::json(&serde_json::json!({
        "total": servers.len(),
        "items": servers,
    })))
}
