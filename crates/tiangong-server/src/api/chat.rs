use silent::prelude::*;

use super::AuthToken;
use super::SharedAppContext;
use super::types::{ChatRequest, ChatResponse};
use crate::auth::{
    check_auth, ensure_remote_action, extract_remote_access, resolve_visible_session_id,
};
use tiangong_core::session::now_text;
use tiangong_types::{IncomingMessage, MessageContent};

/// POST /api/v1/chat — 发送消息并获取 AI 回复
#[allow(deprecated)]
pub async fn chat(mut req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;
    let access = extract_remote_access(&req)?;
    ensure_remote_action(&access, access.role.can_send_message(), "发送消息")?;

    let app = req.get_config::<SharedAppContext>()?.clone();
    let body: ChatRequest = req.json_parse().await?;

    let session_id = {
        let state = app.state.lock().await;
        resolve_visible_session_id(
            &access,
            state.active_session_id(),
            body.session_id.as_deref(),
        )?
    };

    let outgoing = app
        .router
        .handle_incoming(IncomingMessage {
            id: scru128::new().to_string(),
            connector: "server-api".to_string(),
            channel_id: session_id.clone(),
            sender_id: "http-client".to_string(),
            sender_role: access.role,
            content: MessageContent::Text(body.message),
            media: Vec::new(),
            reply_to: None,
            timestamp: now_text(),
        })
        .await
        .map_err(|e| {
            SilentError::business_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("发送消息失败：{e}"),
            )
        })?;

    let response_text = match outgoing.content {
        MessageContent::Text(text) => text,
        _ => String::new(),
    };

    Ok(Response::json(&ChatResponse {
        session_id,
        response: response_text,
    }))
}
