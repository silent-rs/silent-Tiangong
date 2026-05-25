use std::collections::HashSet;
use std::sync::Arc;

use crate::api::ServerAppContext;
use tiangong_core::scheduler::store::JobStore;
use tiangong_types::MessageContent;

/// 通用执行参数
pub struct ExecuteParams {
    pub trigger_id: String,
    pub trigger_name: String,
    pub trigger_description: String,
    pub session_id: Option<String>,
    pub payload: String,
}

/// 执行结果记录方式
pub enum RunTracker {
    /// 记录到 JobStore（定时任务）
    Job { store: JobStore },
    /// 记录到 WebhookStore（webhook 触发）
    Webhook {
        store: crate::webhook::store::WebhookStore,
    },
}

/// 正在执行的 trigger_id 集合，防止同一任务重叠执行
static RUNNING: std::sync::LazyLock<std::sync::Mutex<HashSet<String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashSet::new()));

/// 触发执行：查找/创建 session → 发送消息 → 等待结果 → 记录执行历史
pub async fn execute(app_ctx: Arc<ServerAppContext>, params: ExecuteParams, tracker: RunTracker) {
    // 防止同一任务重叠执行
    {
        let mut running = RUNNING.lock().unwrap();
        if running.contains(&params.trigger_id) {
            tracing::warn!("触发 {} 跳过：上一轮执行尚未完成", params.trigger_id);
            return;
        }
        running.insert(params.trigger_id.clone());
    }
    struct RunGuard(String);
    impl Drop for RunGuard {
        fn drop(&mut self) {
            RUNNING.lock().unwrap().remove(&self.0);
        }
    }
    let _guard = RunGuard(params.trigger_id.clone());

    let run_id = scru128::new().to_string();
    let now = chrono::Local::now().naive_local().to_string();

    // 确定使用哪个 session
    let (session_id, created_new) = match resolve_session(&app_ctx, &params).await {
        Ok(result) => result,
        Err(e) => {
            tracing::error!("触发 {} 解析 session 失败：{e}", params.trigger_id);
            return;
        }
    };

    // 首次执行时将 session_id 写回 job/webhook，后续复用同一会话
    if created_new {
        pin_session_to_tracker(&tracker, &params.trigger_id, &session_id);
    }

    // 记录开始执行
    insert_run(&tracker, &run_id, &params.trigger_id, &session_id, &now);

    // 构造消息
    let message = format!(
        "[自动化任务触发]\n任务名称：{}\n任务描述：{}\n\n{}",
        params.trigger_name, params.trigger_description, params.payload
    );

    // 通过 ServerCoreManager 发送消息并等待结果
    let result = app_ctx
        .cores
        .send_message_and_wait(&session_id, message, None, vec![])
        .await;

    let finished_at = chrono::Local::now().naive_local().to_string();
    match result {
        Ok((_sid, outgoing)) => {
            let text = match &outgoing.content {
                MessageContent::Text(t) => t.clone(),
                MessageContent::Image { url, .. } => url.clone(),
                MessageContent::Video { url, .. } => url.clone(),
                _ => String::new(),
            };
            let summary = truncate_summary(&text, 500);
            update_run_success(
                &tracker,
                &run_id,
                &params.trigger_id,
                &finished_at,
                &summary,
            );
            tracing::info!("触发 {} 执行成功", params.trigger_id);
        }
        Err(e) => {
            let err_msg = format!("执行失败：{e}");
            update_run_failed(
                &tracker,
                &run_id,
                &params.trigger_id,
                &finished_at,
                &err_msg,
            );
            tracing::error!("触发 {} 执行失败：{e}", params.trigger_id);
        }
    }
}

/// 执行定时任务（兼容旧接口）
pub async fn execute_job(
    app_ctx: Arc<ServerAppContext>,
    job: tiangong_core::scheduler::model::Job,
) {
    let store = match JobStore::open() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("任务 {} 打开 store 失败：{e}", job.id);
            return;
        }
    };

    let fresh = store.get_job(&job.id).ok().flatten().unwrap_or(job);

    let params = ExecuteParams {
        trigger_id: fresh.id.clone(),
        trigger_name: fresh.name.clone(),
        trigger_description: fresh.description.clone(),
        session_id: fresh.session_id.clone(),
        payload: fresh.payload.clone(),
    };

    execute(app_ctx, params, RunTracker::Job { store }).await;
}

/// 执行 webhook 触发
pub async fn execute_webhook(
    app_ctx: Arc<ServerAppContext>,
    webhook: crate::webhook::model::Webhook,
) {
    let store = match crate::webhook::store::WebhookStore::open() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Webhook {} 打开 store 失败：{e}", webhook.id);
            return;
        }
    };

    let fresh = store.get(&webhook.id).ok().flatten().unwrap_or(webhook);

    let params = ExecuteParams {
        trigger_id: fresh.id.clone(),
        trigger_name: fresh.name.clone(),
        trigger_description: fresh.description.clone(),
        session_id: fresh.session_id.clone(),
        payload: fresh.payload.clone(),
    };

    execute(app_ctx, params, RunTracker::Webhook { store }).await;
}

// ── 内部方法 ──────────────────────────────────────────────────

/// 将 session_id 写回 job/webhook，确保后续执行复用同一会话
fn pin_session_to_tracker(tracker: &RunTracker, trigger_id: &str, session_id: &str) {
    match tracker {
        RunTracker::Job { store } => {
            let req = tiangong_core::scheduler::model::UpdateJobRequest {
                session_id: Some(session_id.to_string()),
                ..Default::default()
            };
            if let Err(e) = store.update_job(trigger_id, &req) {
                tracing::warn!("任务 {} 写回 session_id 失败：{e}", trigger_id);
            } else {
                tracing::info!("任务 {} 已绑定会话 {}", trigger_id, session_id);
            }
        }
        RunTracker::Webhook { store } => {
            let req = tiangong_core::scheduler::webhook::model::UpdateWebhookRequest {
                session_id: Some(session_id.to_string()),
                ..Default::default()
            };
            if let Err(e) = store.update(trigger_id, &req) {
                tracing::warn!("Webhook {} 写回 session_id 失败：{e}", trigger_id);
            } else {
                tracing::info!("Webhook {} 已绑定会话 {}", trigger_id, session_id);
            }
        }
    }
}

fn insert_run(
    tracker: &RunTracker,
    run_id: &str,
    trigger_id: &str,
    session_id: &str,
    started_at: &str,
) {
    match tracker {
        RunTracker::Job { store } => {
            let run = tiangong_core::scheduler::model::JobRun {
                id: run_id.to_string(),
                job_id: trigger_id.to_string(),
                session_id: session_id.to_string(),
                status: tiangong_core::scheduler::model::JobRunStatus::Running,
                started_at: started_at.to_string(),
                finished_at: None,
                result_summary: None,
            };
            if let Err(e) = store.insert_job_run(&run) {
                tracing::error!("记录 JobRun 失败：{e}");
            }
        }
        RunTracker::Webhook { store } => {
            let run = crate::webhook::model::WebhookRun {
                id: run_id.to_string(),
                webhook_id: trigger_id.to_string(),
                session_id: session_id.to_string(),
                status: crate::webhook::model::WebhookRunStatus::Running,
                started_at: started_at.to_string(),
                finished_at: None,
                result_summary: None,
            };
            if let Err(e) = store.insert_run(&run) {
                tracing::error!("记录 WebhookRun 失败：{e}");
            }
        }
    }
}

fn update_run_success(
    tracker: &RunTracker,
    run_id: &str,
    trigger_id: &str,
    finished_at: &str,
    summary: &str,
) {
    match tracker {
        RunTracker::Job { store } => {
            let _ = store.update_job_run_status(
                run_id,
                trigger_id,
                &tiangong_core::scheduler::model::JobRunStatus::Succeeded,
                Some(finished_at),
                Some(summary),
            );
        }
        RunTracker::Webhook { store } => {
            let _ = store.update_run_status(
                run_id,
                trigger_id,
                &crate::webhook::model::WebhookRunStatus::Succeeded,
                Some(finished_at),
                Some(summary),
            );
        }
    }
}

fn update_run_failed(
    tracker: &RunTracker,
    run_id: &str,
    trigger_id: &str,
    finished_at: &str,
    error: &str,
) {
    match tracker {
        RunTracker::Job { store } => {
            let _ = store.update_job_run_status(
                run_id,
                trigger_id,
                &tiangong_core::scheduler::model::JobRunStatus::Failed,
                Some(finished_at),
                Some(error),
            );
        }
        RunTracker::Webhook { store } => {
            let _ = store.update_run_status(
                run_id,
                trigger_id,
                &crate::webhook::model::WebhookRunStatus::Failed,
                Some(finished_at),
                Some(error),
            );
        }
    }
}

/// 解析或创建 session，返回 (session_id, 是否新建)
async fn resolve_session(
    app_ctx: &Arc<ServerAppContext>,
    params: &ExecuteParams,
) -> anyhow::Result<(String, bool)> {
    if let Some(ref sid) = params.session_id {
        let state = app_ctx.state.lock().await;
        if state.sessions().iter().any(|s| s.id == *sid) {
            return Ok((sid.clone(), false));
        }
    }

    // 没有指定 session 或 session 不存在，创建新 session
    let mut state = app_ctx.state.lock().await;
    let title = format!("自动化任务：{}", params.trigger_name);
    let session = tiangong_core::session::Session::new_isolated(title);
    let session_id = session.id.clone();
    state.sessions_mut().push(session);
    state.persist_session(&session_id)?;
    Ok((session_id, true))
}

fn truncate_summary(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_len).collect();
    format!("{truncated}...")
}
