use silent::prelude::*;

use super::AuthToken;
use super::SharedAppContext;
use crate::auth::check_auth;

/// POST /api/v1/server/shutdown — 优雅关闭服务
///
/// 先停止所有 bot 子进程（含 supervisor，避免孤儿重启，issue #286 阶段 2），
/// 再退出进程。退出后 OS 自动释放 Server Bot 管理所有权锁，Desktop 可接管。
#[allow(deprecated)]
pub async fn shutdown(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    tracing::info!("收到 shutdown 请求，准备停止 bot 后关闭服务");

    // 立即进入 draining（review 问题6）：拒绝新的 Bot 写操作，保证移交期间无并发写入。
    app_ctx
        .draining
        .store(true, std::sync::atomic::Ordering::Release);
    // 在后台任务中：先 stop_all（bot 退出），再 exit，给响应留出返回窗口。
    let bot_runtime = app_ctx.bot_runtime.clone();
    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        tracing::info!("shutdown：停止所有 bot...");
        bot_runtime.stop_all().await;
        tracing::info!("shutdown：bot 已停止，退出进程");
        std::process::exit(0);
    });

    Ok(Response::json(&serde_json::json!({
        "status": "shutting_down"
    })))
}
