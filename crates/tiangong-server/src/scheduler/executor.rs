use std::sync::Arc;

use crate::api::ServerAppContext;
use tiangong_core::scheduler::model::{Job, JobRun, JobRunStatus};
use tiangong_core::scheduler::store::JobStore;

/// 触发执行一个 Job：查找/创建 session → 发送消息 → 等待结果 → 记录 JobRun
pub async fn execute_job(app_ctx: Arc<ServerAppContext>, job: Job) {
    let run_id = scru128::new().to_string();
    let now = chrono::Local::now().naive_local().to_string();

    // 确定使用哪个 session
    let session_id = match resolve_session(&app_ctx, &job).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!("任务 {} 解析 session 失败：{e}", job.id);
            return;
        }
    };

    // 创建 JobRun 记录
    let run = JobRun {
        id: run_id.clone(),
        job_id: job.id.clone(),
        session_id: session_id.clone(),
        status: JobRunStatus::Running,
        started_at: now,
        finished_at: None,
        result_summary: None,
    };

    let store = match JobStore::open() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("任务 {} 打开 store 失败：{e}", job.id);
            return;
        }
    };

    if let Err(e) = store.insert_job_run(&run) {
        tracing::error!("任务 {} 记录执行失败：{e}", job.id);
        return;
    }

    // 构造消息：任务描述 + 触发上下文
    let message = format!(
        "[自动化任务触发]\n任务名称：{}\n任务描述：{}\n\n{}",
        job.name, job.description, job.payload
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
                tiangong_types::MessageContent::Text(t) => t.clone(),
                tiangong_types::MessageContent::Image { url, .. } => url.clone(),
                tiangong_types::MessageContent::Video { url, .. } => url.clone(),
                _ => String::new(),
            };
            let summary = truncate_summary(&text, 500);
            let _ = store.update_job_run_status(
                &run_id,
                &job.id,
                &JobRunStatus::Succeeded,
                Some(&finished_at),
                Some(&summary),
            );
            tracing::info!("任务 {} 执行成功", job.id);
        }
        Err(e) => {
            let err_msg = format!("执行失败：{e}");
            let _ = store.update_job_run_status(
                &run_id,
                &job.id,
                &JobRunStatus::Failed,
                Some(&finished_at),
                Some(&err_msg),
            );
            tracing::error!("任务 {} 执行失败：{e}", job.id);
        }
    }
}

/// 解析或创建 session
async fn resolve_session(app_ctx: &Arc<ServerAppContext>, job: &Job) -> anyhow::Result<String> {
    if let Some(ref sid) = job.session_id {
        let state = app_ctx.state.lock().await;
        if state.sessions().iter().any(|s| s.id == *sid) {
            return Ok(sid.clone());
        }
    }

    // 没有指定 session 或 session 不存在，创建新 session
    let mut state = app_ctx.state.lock().await;
    let title = format!("定时任务：{}", job.name);
    let session = tiangong_core::session::Session::new_isolated(title);
    let session_id = session.id.clone();
    state.sessions_mut().push(session);
    state.persist_session_and_app(&session_id)?;
    Ok(session_id)
}

fn truncate_summary(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_len).collect();
    format!("{truncated}...")
}
