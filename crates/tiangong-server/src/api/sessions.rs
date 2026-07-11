use silent::prelude::*;

use super::AuthToken;
use super::SharedAppContext;
use super::types::{MessageSummary, SessionSummary};
use crate::auth::{
    check_auth, ensure_remote_action, extract_remote_access, resolve_visible_session_id,
};

/// GET /api/v1/sessions — 会话列表
#[allow(deprecated)]
pub async fn list_sessions(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;
    let access = extract_remote_access(&req)?;
    ensure_remote_action(&access, access.role.can_observe(), "查看会话")?;

    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let app = app_ctx.state.lock().await;

    let visible_session_id = (!access.role.can_manage_sessions())
        .then(|| resolve_visible_session_id(&access, app.active_session_id(), None))
        .transpose()?;

    let sessions: Vec<SessionSummary> = app
        .sessions()
        .iter()
        .filter(|session| {
            visible_session_id
                .as_deref()
                .is_none_or(|visible_id| session.id == visible_id)
        })
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
    let access = extract_remote_access(&req)?;
    ensure_remote_action(&access, access.role.can_manage_sessions(), "创建会话")?;

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
    let access = extract_remote_access(&req)?;
    ensure_remote_action(&access, access.role.can_observe(), "查看会话")?;

    let requested_id: String = req.get_path_params("id")?;
    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let app = app_ctx.state.lock().await;
    let id = resolve_visible_session_id(&access, app.active_session_id(), Some(&requested_id))?;

    let session = app.sessions().iter().find(|s| s.id == id).ok_or_else(|| {
        SilentError::business_error(StatusCode::NOT_FOUND, format!("会话 '{id}' 不存在"))
    })?;

    let messages: Vec<MessageSummary> = session
        .messages
        .iter()
        .map(|m| MessageSummary {
            id: m.id.clone(),
            role: format!("{:?}", m.role).to_lowercase(),
            content: m.text_content(),
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

/// GET /api/v1/sessions/:id/cost — 会话成本详情
#[allow(deprecated)]
pub async fn get_session_cost(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;
    let access = extract_remote_access(&req)?;
    ensure_remote_action(&access, access.role.can_observe(), "查看会话成本")?;

    let requested_id: String = req.get_path_params("id")?;
    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let app = app_ctx.state.lock().await;
    let id = resolve_visible_session_id(&access, app.active_session_id(), Some(&requested_id))?;

    let session = app.sessions().iter().find(|s| s.id == id).ok_or_else(|| {
        SilentError::business_error(StatusCode::NOT_FOUND, format!("会话 '{id}' 不存在"))
    })?;

    let cost =
        tiangong_core::observe::build_session_cost(session.id.clone(), &session.task_records);
    Ok(Response::json(&cost))
}

/// DELETE /api/v1/sessions/:id — 删除会话
#[allow(deprecated)]
pub async fn delete_session(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;
    let access = extract_remote_access(&req)?;
    ensure_remote_action(&access, access.role.can_manage_sessions(), "删除会话")?;

    let id: String = req.get_path_params("id")?;
    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let deleted = app_ctx.cores.delete_session(&id).await.map_err(|error| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("删除会话失败：{error}"),
        )
    })?;
    if !deleted {
        return Err(SilentError::business_error(
            StatusCode::NOT_FOUND,
            format!("会话 '{id}' 不存在"),
        ));
    }

    Ok(Response::json(&serde_json::json!({
        "status": "deleted",
        "id": id,
    })))
}
