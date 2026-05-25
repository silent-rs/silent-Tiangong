use silent::prelude::*;

use super::SharedAppContext;
use crate::scheduler::executor;
use tiangong_core::scheduler::model::TriggerType;
use tiangong_core::scheduler::store::JobStore;

/// POST /api/v1/webhooks/<token> — Webhook 触发端点
///
/// 通过 job id 作为 token 触发对应的 webhook 类型任务。
/// 如果 job 配置了 webhook_secret，需在请求头 `X-Webhook-Signature` 中传入签名。
#[allow(deprecated)]
pub async fn handle_webhook(req: Request) -> Result<Response> {
    let token: String = req.get_path_params("token")?;

    let store = open_store()?;
    let job = store
        .list_enabled_jobs_by_type(&TriggerType::Webhook)
        .map_err(|e| {
            SilentError::business_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("查询 webhook 任务失败：{e}"),
            )
        })?;

    let job = job.into_iter().find(|j| j.id == token).ok_or_else(|| {
        SilentError::business_error(StatusCode::NOT_FOUND, format!("Webhook '{token}' 不存在"))
    })?;

    // 验证签名（如果配置了 secret）
    if let Some(ref secret) = job.webhook_secret {
        let signature = req
            .headers()
            .get("X-Webhook-Signature")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if signature != secret {
            return Err(SilentError::business_error(
                StatusCode::UNAUTHORIZED,
                "签名验证失败".to_string(),
            ));
        }
    }

    let app_ctx = req.get_config::<SharedAppContext>()?.clone();
    let job_clone = job.clone();
    tokio::spawn(async move {
        executor::execute_job(app_ctx, job_clone).await;
    });

    Ok(Response::json(&serde_json::json!({
        "job_id": job.id,
        "status": "triggered",
    })))
}

fn open_store() -> Result<JobStore> {
    JobStore::open().map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("打开调度器存储失败：{e}"),
        )
    })
}
