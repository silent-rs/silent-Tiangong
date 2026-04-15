use silent::prelude::*;

use super::AuthToken;
use super::SharedAppContext;
use super::types::{
    ApprovalListResponse, ApprovalResponseRequest, ApprovalResponseResult, ApprovalSummary,
};
use crate::auth::{
    check_auth, ensure_remote_action, extract_remote_access, resolve_visible_session_id,
};

#[allow(deprecated)]
pub async fn list_approvals(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;
    let access = extract_remote_access(&req)?;
    ensure_remote_action(&access, access.role.can_approve(), "查看审批请求")?;

    let app = req.get_config::<SharedAppContext>()?.clone();
    let session_id = {
        let state = app.state.lock().await;
        resolve_visible_session_id(&access, state.active_session_id(), None)?
    };

    let (session_id, approvals) = app
        .cores
        .list_pending_approvals(&session_id)
        .await
        .map_err(|e| {
            SilentError::business_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("查询审批请求失败：{e}"),
            )
        })?;

    let items = approvals
        .into_iter()
        .map(|approval| ApprovalSummary {
            request_id: approval.request_id,
            tool_name: approval.tool_name,
            args_summary: approval.tool_args_summary,
            created_at: approval.created_at,
        })
        .collect();

    Ok(Response::json(&ApprovalListResponse { session_id, items }))
}

#[allow(deprecated)]
pub async fn respond_approval(mut req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;
    let access = extract_remote_access(&req)?;
    ensure_remote_action(&access, access.role.can_approve(), "响应审批")?;

    let app = req.get_config::<SharedAppContext>()?.clone();
    let body: ApprovalResponseRequest = req.json_parse().await?;

    let session_id = {
        let state = app.state.lock().await;
        resolve_visible_session_id(
            &access,
            state.active_session_id(),
            body.session_id.as_deref(),
        )?
    };

    let session_id = app
        .cores
        .respond_approval(&session_id, body.request_id.clone(), body.approved)
        .await
        .map_err(|e| {
            SilentError::business_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("提交审批响应失败：{e}"),
            )
        })?;

    Ok(Response::json(&ApprovalResponseResult {
        session_id,
        request_id: body.request_id,
        approved: body.approved,
    }))
}
