use silent::prelude::*;

use super::{AuthToken, SharedAppContext};
use crate::auth::check_auth;

/// GET /api/v1/mcp — MCP server 列表
///
/// 复用 ServerAppContext 持有的共享 MCP plugin 实例（与运行中 core 同一实例），
/// 确保返回的配置与 core 实际使用的工具状态一致。
#[allow(deprecated)]
pub async fn list_mcp(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let servers: Vec<serde_json::Value> = app_ctx
        .mcp_plugin
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
