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
        .then(|| resolve_visible_session_id(&access, app.active_session_id.as_str(), None))
        .transpose()?;

    let sessions: Vec<SessionSummary> = app
        .core_manager
        .list_session_metadata()
        .iter()
        .filter(|metadata| {
            visible_session_id
                .as_deref()
                .is_none_or(|visible_id| metadata.id == visible_id)
        })
        .map(|metadata| SessionSummary {
            id: metadata.id.clone(),
            title: metadata.title.clone(),
            message_count: metadata.message_count,
            created_at: metadata.created_at.clone(),
            updated_at: metadata.updated_at.clone(),
        })
        .collect();

    Ok(Response::json(&serde_json::json!({
        "total": sessions.len(),
        "items": sessions,
    })))
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
    let (id, core_manager, session_exists) = {
        let app = app_ctx.state.lock().await;
        let id = resolve_visible_session_id(
            &access,
            app.active_session_id.as_str(),
            Some(&requested_id),
        )?;
        let exists = app.core_manager.session_exists(&id);
        (id, app.core_manager.clone(), exists)
    };
    if !session_exists {
        return Err(SilentError::business_error(
            StatusCode::NOT_FOUND,
            format!("会话 '{id}' 不存在"),
        ));
    }
    // 消息内容需完整 Session；从磁盘 load（issue #245：真相源归磁盘）。
    let session = core_manager.load_session(&id).map_err(|error| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("加载会话失败：{error}"),
        )
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
    let (id, core_manager, session_exists) = {
        let app = app_ctx.state.lock().await;
        let id = resolve_visible_session_id(
            &access,
            app.active_session_id.as_str(),
            Some(&requested_id),
        )?;
        let exists = app.core_manager.session_exists(&id);
        (id, app.core_manager.clone(), exists)
    };

    if !session_exists {
        return Err(SilentError::business_error(
            StatusCode::NOT_FOUND,
            format!("会话 '{id}' 不存在"),
        ));
    }
    // task_records 是完整 Session 字段；从磁盘 load（issue #245）。
    let session = core_manager.load_session(&id).map_err(|error| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("加载会话失败：{error}"),
        )
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
    let deleted = app_ctx
        .core_backend
        .delete_session(&id)
        .await
        .map_err(|error| {
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
