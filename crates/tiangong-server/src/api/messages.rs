use silent::prelude::*;

use super::types::{ApiMessageContent, ConnectorMessageRequest, ConnectorMessageResponse};
use super::{AuthToken, SharedAppContext};
use crate::auth::{check_auth, ensure_remote_action, extract_remote_access};
use tiangong_core::session::now_text;
use tiangong_types::IncomingMessage;

/// POST /api/v1/messages — 外部 Bot / Connector 统一消息入口
#[allow(deprecated)]
pub async fn post_message(mut req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;
    let access = extract_remote_access(&req)?;
    ensure_remote_action(&access, access.role.can_send_message(), "发送消息")?;

    let app = req.get_config::<SharedAppContext>()?.clone();
    let body: ConnectorMessageRequest = req.json_parse().await?;
    let connector = body
        .connector
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("external-bot")
        .to_string();
    let channel_id = body.channel_id.trim().to_string();
    if channel_id.is_empty() {
        return Err(SilentError::business_error(
            StatusCode::BAD_REQUEST,
            "channel_id 不能为空".to_string(),
        ));
    }

    let content = resolve_content(body.message, body.content)?;
    let media = body.media;
    let (session_id, outgoing) = app
        .router
        .handle_incoming_with_session(IncomingMessage {
            id: body
                .message_id
                .filter(|id| !id.trim().is_empty())
                .unwrap_or_else(|| scru128::new().to_string()),
            connector: connector.clone(),
            channel_id: channel_id.clone(),
            sender_id: body
                .sender_id
                .filter(|id| !id.trim().is_empty())
                .unwrap_or_else(|| "external-user".to_string()),
            sender_role: access.role,
            content: content.into(),
            media,
            reply_to: body.reply_to,
            timestamp: now_text(),
        })
        .await
        .map_err(|e| {
            SilentError::business_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("处理外部消息失败：{e}"),
            )
        })?;

    let content = ApiMessageContent::from(outgoing.content);
    let message = content.text();
    Ok(Response::json(&ConnectorMessageResponse {
        session_id,
        connector,
        channel_id,
        reply_to: outgoing.reply_to,
        message,
        content,
    }))
}

fn resolve_content(
    message: Option<String>,
    content: Option<ApiMessageContent>,
) -> Result<ApiMessageContent> {
    match (message, content) {
        (Some(message), _) if !message.trim().is_empty() => Ok(ApiMessageContent::Text {
            text: message.trim().to_string(),
        }),
        (_, Some(content)) => Ok(content),
        _ => Err(SilentError::business_error(
            StatusCode::BAD_REQUEST,
            "message 或 content 必须提供一个".to_string(),
        )),
    }
}
