use silent::prelude::*;

use super::AuthToken;
use crate::auth::check_auth;
use tiangong_scheduler::store::JobStore;

/// GET /api/v1/jobs — Job 列表
pub async fn list_jobs(req: Request) -> Result<Response> {
    let token = req.get_state::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let store = open_store()?;
    let jobs = store.list_jobs().map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("查询任务列表失败：{e}"),
        )
    })?;

    Ok(Response::json(&serde_json::json!({
        "total": jobs.len(),
        "items": jobs,
    })))
}

/// POST /api/v1/jobs — 创建 Job
///
/// 经 scheduler sidecar 创建，确保 cron 调度同步注册新任务。
pub async fn create_job(mut req: Request) -> Result<Response> {
    let token = req.get_state::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let body: serde_json::Value = req.json_parse().await?;
    if body.get("schedule").and_then(|v| v.as_str()).is_none() {
        return Err(SilentError::business_error(
            StatusCode::BAD_REQUEST,
            "Cron 类型任务必须提供 schedule 字段".to_string(),
        ));
    }

    let response = invoke_scheduler_sidecar("create_job", body)?;
    let job = response
        .get("job")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    Ok(Response::json(&job).with_status(StatusCode::CREATED))
}

/// GET /api/v1/jobs/<id> — Job 详情
pub async fn get_job(req: Request) -> Result<Response> {
    let token = req.get_state::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let store = open_store()?;
    let job = store.get_job(&id).map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("查询任务失败：{e}"),
        )
    })?;

    match job {
        Some(j) => Ok(Response::json(&j)),
        None => Err(SilentError::business_error(
            StatusCode::NOT_FOUND,
            format!("任务 '{id}' 不存在"),
        )),
    }
}

/// PUT /api/v1/jobs/<id> — 更新 Job
///
/// 经 scheduler sidecar 更新，确保 cron 调度同步刷新。
pub async fn update_job(mut req: Request) -> Result<Response> {
    let token = req.get_state::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let mut body: serde_json::Value = req.json_parse().await?;
    // 把路径 id 合并进 body，供 sidecar 的 update_job 使用
    if let Some(obj) = body.as_object_mut() {
        obj.insert("id".to_string(), serde_json::Value::String(id.clone()));
    }

    match invoke_scheduler_sidecar("update_job", body) {
        Ok(response) => {
            let job = response
                .get("job")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            Ok(Response::json(&job))
        }
        Err(e) => Err(map_sidecar_error(e, &id)),
    }
}

/// DELETE /api/v1/jobs/<id> — 删除 Job
///
/// 经 scheduler sidecar 删除，确保 cron 调度同步移除。
pub async fn delete_job(req: Request) -> Result<Response> {
    let token = req.get_state::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let payload = serde_json::json!({ "id": id });
    match invoke_scheduler_sidecar("delete_job", payload) {
        Ok(_) => Ok(Response::json(&serde_json::json!({
            "status": "deleted",
            "id": id,
        }))),
        Err(e) => Err(map_sidecar_error(e, &id)),
    }
}

/// GET /api/v1/jobs/<id>/runs — Job 执行历史
pub async fn list_job_runs(mut req: Request) -> Result<Response> {
    let token = req.get_state::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let limit: usize = req
        .params()
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);

    let store = open_store()?;
    let job_exists = store.get_job(&id).map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("查询任务失败：{e}"),
        )
    })?;

    if job_exists.is_none() {
        return Err(SilentError::business_error(
            StatusCode::NOT_FOUND,
            format!("任务 '{id}' 不存在"),
        ));
    }

    let runs = store.list_job_runs(&id, limit).map_err(|e| {
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

/// POST /api/v1/jobs/<id>/trigger — 手动触发 Job
///
/// 经 scheduler sidecar 触发，sidecar 负责实际执行（HTTP 投递 + 写执行记录）。
pub async fn trigger_job(req: Request) -> Result<Response> {
    let token = req.get_state::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let payload = serde_json::json!({ "id": id });
    match invoke_scheduler_sidecar("trigger_job", payload) {
        Ok(response) => {
            let job = response
                .get("job")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            Ok(Response::json(&serde_json::json!({
                "job_id": job.get("id").and_then(|v| v.as_str()).unwrap_or(&id),
                "session_id": job.get("session_id").and_then(|v| v.as_str()),
                "status": "triggered",
            })))
        }
        Err(e) => Err(map_sidecar_error(e, &id)),
    }
}

fn open_store() -> Result<JobStore> {
    JobStore::open().map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("打开调度器存储失败：{e}"),
        )
    })
}

/// 调用 scheduler sidecar 的指定操作。
fn invoke_scheduler_sidecar(
    operation: &str,
    payload: serde_json::Value,
) -> std::result::Result<serde_json::Value, anyhow::Error> {
    tiangong_plugin_runtime::registry::invoke_sidecar(
        &tiangong_config::io::storage_root(),
        "scheduler",
        operation,
        payload,
    )
}

/// 把 sidecar 错误映射为合适的 HTTP 状态（任务不存在 → 404）。
fn map_sidecar_error(error: anyhow::Error, id: &str) -> SilentError {
    let message = format!("{error}");
    if message.contains("不存在") {
        SilentError::business_error(StatusCode::NOT_FOUND, format!("任务 '{id}' 不存在"))
    } else {
        SilentError::business_error(StatusCode::INTERNAL_SERVER_ERROR, message)
    }
}
