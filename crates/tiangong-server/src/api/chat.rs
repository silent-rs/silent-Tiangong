use silent::prelude::*;

use super::AuthToken;
use super::SharedState;
use super::types::{ChatRequest, ChatResponse};
use crate::auth::check_auth;

/// POST /api/v1/chat — 发送消息并获取 AI 回复
#[allow(deprecated)]
pub async fn chat(mut req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let state = req.get_config::<SharedState>()?.clone();
    let body: ChatRequest = req.json_parse().await?;

    let mut app = state.lock().await;

    // 如果指定了 session_id，则切换到对应会话
    if let Some(ref sid) = body.session_id {
        app.switch_session(sid);
    }

    let session_id = app.active_session_id().to_string();

    // 设置输入并发送
    app.update_draft(body.message);
    if let Err(e) = app.send_current_input() {
        return Err(SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("发送消息失败：{e}"),
        ));
    }

    // 轮询等待执行完成（最多 300 秒）
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(300);
    loop {
        app.poll_pending_turn();

        if !app.has_pending_turn() {
            break;
        }

        if tokio::time::Instant::now() >= deadline {
            return Err(SilentError::business_error(
                StatusCode::GATEWAY_TIMEOUT,
                "执行超时".to_string(),
            ));
        }

        // 释放锁后短暂等待，避免忙等
        drop(app);
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        app = state.lock().await;
    }

    // 获取最后一条 assistant 消息作为回复
    let response_text = app
        .active_session()
        .and_then(|session| {
            session
                .messages
                .iter()
                .rev()
                .find(|m| matches!(m.role, tiangong_core::session::MessageRole::Assistant))
                .map(|m| m.content.clone())
        })
        .unwrap_or_default();

    Ok(Response::json(&ChatResponse {
        session_id,
        response: response_text,
    }))
}
