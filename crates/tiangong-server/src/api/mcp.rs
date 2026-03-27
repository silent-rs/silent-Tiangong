use silent::prelude::*;

use super::AuthToken;
use super::SharedState;
use crate::auth::check_auth;

/// GET /api/v1/mcp — MCP server 列表
#[allow(deprecated)]
pub async fn list_mcp(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let state = req.get_config::<SharedState>()?.clone();
    let app = state.lock().await;

    let servers: Vec<serde_json::Value> = app
        .mcp_servers()
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
