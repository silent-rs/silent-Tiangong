use silent::prelude::*;

use super::AuthToken;
use crate::auth::check_auth;

/// POST /api/v1/server/shutdown — 优雅关闭服务
#[allow(deprecated)]
pub async fn shutdown(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    tracing::info!("收到 shutdown 请求，准备关闭服务");

    // 在后台异步发送关闭信号，给响应一个返回的机会
    tokio::spawn(async {
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        std::process::exit(0);
    });

    Ok(Response::json(&serde_json::json!({
        "status": "shutting_down"
    })))
}
