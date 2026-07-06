use silent::prelude::*;

use super::AuthToken;
use crate::auth::check_auth;

/// GET /api/v1/mcp — MCP server 列表
///
/// MCP 配置由 tiangong-plugin-mcp 自管（~/.tiangong/mcp.json），server 无状态
/// 读取：每次请求构造临时 plugin 实例读取最新配置（文件共享，多进程一致）。
#[allow(deprecated)]
pub async fn list_mcp(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let mcp_plugin = tiangong_plugin_mcp::McpPlugin::new();
    let servers: Vec<serde_json::Value> = mcp_plugin
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
