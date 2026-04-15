use silent::prelude::*;
use silent::ws::WebSocketParts;
use tiangong_types::RemoteRole;

pub const REMOTE_ROLE_HEADER: &str = "x-tiangong-role";
pub const REMOTE_SESSION_HEADER: &str = "x-tiangong-session-id";

#[derive(Debug, Clone)]
pub struct RemoteAccessContext {
    pub role: RemoteRole,
    pub session_scope: Option<String>,
}

/// 从请求中提取 Bearer Token 并与预配置 token 比对。
/// 如果 expected_token 为 None，则跳过认证（无 token 配置时视为开放）。
/// 返回 Ok(()) 表示通过，Err 表示 401。
pub fn check_auth(
    req: &Request,
    expected_token: Option<&str>,
) -> std::result::Result<(), SilentError> {
    let Some(expected) = expected_token else {
        // 未配置 token，免认证
        return Ok(());
    };

    check_auth_headers(req.headers(), expected)
}

pub fn check_ws_auth(
    parts: &WebSocketParts,
    expected_token: Option<&str>,
) -> std::result::Result<(), SilentError> {
    let Some(expected) = expected_token else {
        return Ok(());
    };
    check_auth_headers(parts.headers(), expected)
}

fn check_auth_headers(
    headers: &silent::header::HeaderMap<silent::header::HeaderValue>,
    expected: &str,
) -> std::result::Result<(), SilentError> {
    let auth_header = headers.get("authorization").and_then(|v| v.to_str().ok());

    match auth_header {
        Some(value) if value.starts_with("Bearer ") => {
            let provided = &value[7..];
            if provided == expected {
                Ok(())
            } else {
                Err(SilentError::business_error(
                    StatusCode::UNAUTHORIZED,
                    "认证失败：Token 不匹配".to_string(),
                ))
            }
        }
        _ => Err(SilentError::business_error(
            StatusCode::UNAUTHORIZED,
            "认证失败：缺少 Bearer Token".to_string(),
        )),
    }
}

pub fn extract_remote_access(
    req: &Request,
) -> std::result::Result<RemoteAccessContext, SilentError> {
    extract_remote_access_headers(req.headers())
}

pub fn extract_remote_access_from_ws(
    parts: &WebSocketParts,
) -> std::result::Result<RemoteAccessContext, SilentError> {
    extract_remote_access_headers(parts.headers())
}

fn extract_remote_access_headers(
    headers: &silent::header::HeaderMap<silent::header::HeaderValue>,
) -> std::result::Result<RemoteAccessContext, SilentError> {
    let role = match headers
        .get(REMOTE_ROLE_HEADER)
        .and_then(|v| v.to_str().ok())
    {
        None | Some("") => RemoteRole::Controller,
        Some(raw) => serde_json::from_str::<RemoteRole>(&format!("\"{}\"", raw.to_lowercase()))
            .map_err(|_| {
                SilentError::business_error(
                    StatusCode::BAD_REQUEST,
                    format!("无效的远程角色：{raw}"),
                )
            })?,
    };
    let session_scope = headers
        .get(REMOTE_SESSION_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned);
    Ok(RemoteAccessContext {
        role,
        session_scope,
    })
}

pub fn ensure_remote_action(
    access: &RemoteAccessContext,
    allowed: bool,
    action: &str,
) -> std::result::Result<(), SilentError> {
    if allowed {
        return Ok(());
    }

    Err(SilentError::business_error(
        StatusCode::FORBIDDEN,
        format!(
            "权限不足：{}角色不允许{}",
            access.role.display_name(),
            action
        ),
    ))
}

pub fn resolve_visible_session_id(
    access: &RemoteAccessContext,
    active_session_id: &str,
    requested_session_id: Option<&str>,
) -> std::result::Result<String, SilentError> {
    if access.role.can_manage_sessions() {
        return Ok(requested_session_id
            .unwrap_or(active_session_id)
            .to_string());
    }

    let visible_session_id = access.session_scope.as_deref().unwrap_or(active_session_id);
    if let Some(requested) = requested_session_id
        && requested != visible_session_id
    {
        return Err(SilentError::business_error(
            StatusCode::FORBIDDEN,
            format!(
                "权限不足：{}角色只能访问会话 '{}'",
                access.role.display_name(),
                visible_session_id
            ),
        ));
    }

    Ok(visible_session_id.to_string())
}
