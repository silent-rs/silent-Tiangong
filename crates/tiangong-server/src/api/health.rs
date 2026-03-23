use silent::prelude::*;

/// GET /api/v1/health — 健康检查（不需要认证）
pub async fn health_check(_req: Request) -> Result<Response> {
    Ok(Response::json(&serde_json::json!({
        "status": "ok"
    })))
}
