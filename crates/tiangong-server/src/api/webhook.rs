use silent::prelude::*;

use super::AuthToken;
use super::SharedAppContext;
use crate::auth::check_auth;
use crate::webhook::model::{CreateWebhookRequest, UpdateWebhookRequest, Webhook, open_store};

/// GET /api/v1/webhooks — Webhook 列表
#[allow(deprecated)]
pub async fn list_webhooks(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let store = open_store()?;
    let webhooks = store.list().map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("查询 Webhook 列表失败：{e}"),
        )
    })?;

    Ok(Response::json(&serde_json::json!({
        "total": webhooks.len(),
        "items": webhooks,
    })))
}

/// POST /api/v1/webhooks — 创建 Webhook
#[allow(deprecated)]
pub async fn create_webhook(mut req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let body: CreateWebhookRequest = req.json_parse().await?;

    let now = chrono::Local::now().naive_local().to_string();
    let webhook = Webhook {
        id: scru128::new().to_string(),
        name: body.name,
        description: body.description,
        session_id: body.session_id,
        payload: body.payload,
        secret: body.secret,
        enabled: body.enabled,
        created_at: now.clone(),
        updated_at: now,
    };

    let store = open_store()?;
    store.insert(&webhook).map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("创建 Webhook 失败：{e}"),
        )
    })?;

    Ok(Response::json(&webhook).with_status(StatusCode::CREATED))
}

/// GET /api/v1/webhooks/<id> — Webhook 详情
#[allow(deprecated)]
pub async fn get_webhook(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let store = open_store()?;
    let webhook = store.get(&id).map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("查询 Webhook 失败：{e}"),
        )
    })?;

    match webhook {
        Some(w) => Ok(Response::json(&w)),
        None => Err(SilentError::business_error(
            StatusCode::NOT_FOUND,
            format!("Webhook '{id}' 不存在"),
        )),
    }
}

/// PUT /api/v1/webhooks/<id> — 更新 Webhook
#[allow(deprecated)]
pub async fn update_webhook(mut req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let body: UpdateWebhookRequest = req.json_parse().await?;

    let store = open_store()?;
    let updated = store.update(&id, &body).map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("更新 Webhook 失败：{e}"),
        )
    })?;

    if !updated {
        return Err(SilentError::business_error(
            StatusCode::NOT_FOUND,
            format!("Webhook '{id}' 不存在"),
        ));
    }

    let webhook = store.get(&id).map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("查询 Webhook 失败：{e}"),
        )
    })?;

    Ok(Response::json(&webhook))
}

/// DELETE /api/v1/webhooks/<id> — 删除 Webhook
#[allow(deprecated)]
pub async fn delete_webhook(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let store = open_store()?;
    let deleted = store.delete(&id).map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("删除 Webhook 失败：{e}"),
        )
    })?;

    if !deleted {
        return Err(SilentError::business_error(
            StatusCode::NOT_FOUND,
            format!("Webhook '{id}' 不存在"),
        ));
    }

    Ok(Response::json(&serde_json::json!({
        "status": "deleted",
        "id": id,
    })))
}

/// POST /api/v1/webhooks/<id>/trigger — 手动触发 Webhook
#[allow(deprecated)]
pub async fn trigger_webhook(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let store = open_store()?;
    let webhook = store.get(&id).map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("查询 Webhook 失败：{e}"),
        )
    })?;

    let webhook = webhook.ok_or_else(|| {
        SilentError::business_error(StatusCode::NOT_FOUND, format!("Webhook '{id}' 不存在"))
    })?;

    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let webhook_clone = webhook.clone();
    tokio::spawn(async move {
        crate::webhook::executor::execute_webhook(app_ctx, webhook_clone).await;
    });

    Ok(Response::json(&serde_json::json!({
        "webhook_id": webhook.id,
        "session_id": webhook.session_id,
        "status": "triggered",
    })))
}

/// GET /api/v1/webhooks/<id>/runs — Webhook 执行历史
#[allow(deprecated)]
pub async fn list_webhook_runs(mut req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let limit: usize = req
        .params()
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);

    let store = open_store()?;
    let webhook_exists = store.get(&id).map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("查询 Webhook 失败：{e}"),
        )
    })?;

    if webhook_exists.is_none() {
        return Err(SilentError::business_error(
            StatusCode::NOT_FOUND,
            format!("Webhook '{id}' 不存在"),
        ));
    }

    let runs = store.list_runs(&id, limit).map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("查询执行历史失败：{e}"),
        )
    })?;

    Ok(Response::json(&serde_json::json!({
        "total": runs.len(),
        "items": runs,
    })))
}

/// POST /api/v1/webhooks/<id>/invoke — 外部触发 Webhook（无需认证）
///
/// 通过 webhook id 触发执行。如果配置了 secret，需在请求头 `X-Webhook-Signature` 中传入签名。
#[allow(deprecated)]
pub async fn invoke_webhook(mut req: Request) -> Result<Response> {
    let id: String = req.get_path_params("id")?;
    let store = open_store()?;
    let webhook = store.get(&id).map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("查询 Webhook 失败：{e}"),
        )
    })?;

    let webhook = webhook.ok_or_else(|| {
        SilentError::business_error(StatusCode::NOT_FOUND, format!("Webhook '{id}' 不存在"))
    })?;

    if !webhook.enabled {
        return Err(SilentError::business_error(
            StatusCode::BAD_REQUEST,
            format!("Webhook '{id}' 已禁用"),
        ));
    }

    // 验证签名（如果配置了 secret，使用 HMAC-SHA256）
    if let Some(ref secret) = webhook.secret {
        let signature = req
            .headers()
            .get("X-Webhook-Signature")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let body_bytes = http_body_util::BodyExt::collect(req.take_body())
            .await
            .map_err(|e| {
                SilentError::business_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("读取请求体失败：{e}"),
                )
            })?
            .to_bytes();
        let expected = compute_hmac_sha256(secret, &body_bytes);
        if !constant_time_eq(signature.as_bytes(), expected.as_bytes()) {
            return Err(SilentError::business_error(
                StatusCode::UNAUTHORIZED,
                "签名验证失败".to_string(),
            ));
        }
    }

    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let webhook_clone = webhook.clone();
    tokio::spawn(async move {
        crate::webhook::executor::execute_webhook(app_ctx, webhook_clone).await;
    });

    Ok(Response::json(&serde_json::json!({
        "webhook_id": webhook.id,
        "status": "triggered",
    })))
}

fn compute_hmac_sha256(secret: &str, data: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC key length valid");
    mac.update(data);
    hex::encode(mac.finalize().into_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.ct_eq(b).into()
}
