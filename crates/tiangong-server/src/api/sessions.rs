use silent::prelude::*;

use super::AuthToken;
use super::SharedAppContext;
use super::types::{MessageSummary, SessionSummary};
use crate::auth::check_auth;

/// GET /api/v1/sessions — 会话列表
#[allow(deprecated)]
pub async fn list_sessions(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let app = app_ctx.state.lock().await;

    let sessions: Vec<SessionSummary> = app
        .sessions()
        .iter()
        .map(|s| SessionSummary {
            id: s.id.clone(),
            title: s.title.clone(),
            message_count: s.messages.len(),
            created_at: s.created_at.clone(),
            updated_at: s.updated_at.clone(),
        })
        .collect();

    Ok(Response::json(&serde_json::json!({
        "total": sessions.len(),
        "items": sessions,
    })))
}

/// POST /api/v1/sessions — 创建新会话
#[allow(deprecated)]
pub async fn create_session(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let mut app = app_ctx.state.lock().await;

    app.create_session();
    let session_id = app.active_session_id().to_string();

    Ok(Response::json(&serde_json::json!({
        "session_id": session_id,
    }))
    .with_status(StatusCode::CREATED))
}

/// GET /api/v1/sessions/:id — 会话详情（消息列表）
#[allow(deprecated)]
pub async fn get_session(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let app = app_ctx.state.lock().await;

    let session = app.sessions().iter().find(|s| s.id == id).ok_or_else(|| {
        SilentError::business_error(StatusCode::NOT_FOUND, format!("会话 '{id}' 不存在"))
    })?;

    let messages: Vec<MessageSummary> = session
        .messages
        .iter()
        .map(|m| MessageSummary {
            id: m.id.clone(),
            role: format!("{:?}", m.role).to_lowercase(),
            content: m.content.clone(),
            created_at: m.created_at.clone(),
        })
        .collect();

    Ok(Response::json(&serde_json::json!({
        "id": session.id,
        "title": session.title,
        "messages": messages,
        "created_at": session.created_at,
        "updated_at": session.updated_at,
    })))
}

/// DELETE /api/v1/sessions/:id — 删除会话
#[allow(deprecated)]
pub async fn delete_session(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let mut app = app_ctx.state.lock().await;

    // 先切换到目标会话再删除
    let exists = app.sessions().iter().any(|s| s.id == id);
    if !exists {
        return Err(SilentError::business_error(
            StatusCode::NOT_FOUND,
            format!("会话 '{id}' 不存在"),
        ));
    }

    app.switch_session(&id);
    app.delete_active_session().map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("删除会话失败：{e}"),
        )
    })?;

    Ok(Response::json(&serde_json::json!({
        "status": "deleted",
        "id": id,
    })))
}
