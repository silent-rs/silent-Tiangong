use silent::prelude::*;

use super::AuthToken;
use crate::auth::check_auth;
use tiangong_core::scheduler::model::{
    CreateJobRequest, Job, JobRunStatus, TriggerType, UpdateJobRequest,
};
use tiangong_core::scheduler::store::JobStore;

/// GET /api/v1/jobs — Job 列表
#[allow(deprecated)]
pub async fn list_jobs(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
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
#[allow(deprecated)]
pub async fn create_job(mut req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let body: CreateJobRequest = req.json_parse().await?;
    validate_create_request(&body)?;

    let now = chrono::Local::now().naive_local().to_string();
    let job = Job {
        id: scru128::new().to_string(),
        name: body.name,
        description: body.description,
        trigger_type: body.trigger_type,
        schedule: body.schedule,
        session_id: body.session_id,
        payload: body.payload,
        webhook_secret: body.webhook_secret,
        polling_url: body.polling_url,
        enabled: body.enabled,
        created_at: now.clone(),
        updated_at: now,
    };

    let store = open_store()?;
    store.insert_job(&job).map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("创建任务失败：{e}"),
        )
    })?;

    Ok(Response::json(&job).with_status(StatusCode::CREATED))
}

/// GET /api/v1/jobs/<id> — Job 详情
#[allow(deprecated)]
pub async fn get_job(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
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
#[allow(deprecated)]
pub async fn update_job(mut req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let body: UpdateJobRequest = req.json_parse().await?;

    let store = open_store()?;
    let updated = store.update_job(&id, &body).map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("更新任务失败：{e}"),
        )
    })?;

    if !updated {
        return Err(SilentError::business_error(
            StatusCode::NOT_FOUND,
            format!("任务 '{id}' 不存在"),
        ));
    }

    let job = store.get_job(&id).map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("查询任务失败：{e}"),
        )
    })?;

    Ok(Response::json(&job))
}

/// DELETE /api/v1/jobs/<id> — 删除 Job
#[allow(deprecated)]
pub async fn delete_job(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let store = open_store()?;
    let deleted = store.delete_job(&id).map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("删除任务失败：{e}"),
        )
    })?;

    if !deleted {
        return Err(SilentError::business_error(
            StatusCode::NOT_FOUND,
            format!("任务 '{id}' 不存在"),
        ));
    }

    Ok(Response::json(&serde_json::json!({
        "status": "deleted",
        "id": id,
    })))
}

/// GET /api/v1/jobs/<id>/runs — Job 执行历史
#[allow(deprecated)]
pub async fn list_job_runs(mut req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
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
#[allow(deprecated)]
pub async fn trigger_job(req: Request) -> Result<Response> {
    let token = req.get_config::<AuthToken>()?.clone();
    check_auth(&req, token.0.as_deref())?;

    let id: String = req.get_path_params("id")?;
    let store = open_store()?;
    let job = store.get_job(&id).map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("查询任务失败：{e}"),
        )
    })?;

    let job = job.ok_or_else(|| {
        SilentError::business_error(StatusCode::NOT_FOUND, format!("任务 '{id}' 不存在"))
    })?;

    let now = chrono::Local::now().naive_local().to_string();
    let run_id = scru128::new().to_string();
    let session_id = job
        .session_id
        .clone()
        .unwrap_or_else(|| scru128::new().to_string());

    let run = tiangong_core::scheduler::model::JobRun {
        id: run_id.clone(),
        job_id: id.clone(),
        session_id: session_id.clone(),
        status: JobRunStatus::Running,
        started_at: now,
        finished_at: None,
        result_summary: None,
    };

    store.insert_job_run(&run).map_err(|e| {
        SilentError::business_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("记录执行失败：{e}"),
        )
    })?;

    // TODO: 实际触发执行（接入 RuntimeEngine）

    Ok(Response::json(&serde_json::json!({
        "run_id": run_id,
        "session_id": session_id,
        "status": "running",
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

fn validate_create_request(req: &CreateJobRequest) -> Result<()> {
    match req.trigger_type {
        TriggerType::Cron if req.schedule.is_none() => Err(SilentError::business_error(
            StatusCode::BAD_REQUEST,
            "Cron 类型任务必须提供 schedule 字段".to_string(),
        )),
        TriggerType::Webhook if req.webhook_secret.is_none() => Err(SilentError::business_error(
            StatusCode::BAD_REQUEST,
            "Webhook 类型任务必须提供 webhook_secret 字段".to_string(),
        )),
        TriggerType::Polling if req.polling_url.is_none() => Err(SilentError::business_error(
            StatusCode::BAD_REQUEST,
            "Polling 类型任务必须提供 polling_url 字段".to_string(),
        )),
        _ => Ok(()),
    }
}
