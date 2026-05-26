use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::api::ServerAppContext;
use tiangong_core::scheduler::store::JobStore;

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

/// 消息发送器类型：接受 (session_id, message)，仅发送不等结果
type MessageSender = Box<
    dyn Fn(String, String) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>
        + Send
        + Sync,
>;

/// 正在执行的 trigger_id 集合，防止同一任务重叠执行
static RUNNING: std::sync::LazyLock<std::sync::Mutex<HashSet<String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashSet::new()));

/// 触发执行（生产入口：使用 ServerCoreManager.send_message）
pub async fn execute(app_ctx: Arc<ServerAppContext>, params: ExecuteParams, tracker: RunTracker) {
    let cores = app_ctx.cores.clone();
    let sender: MessageSender = Box::new(move |session_id, message| {
        let cores = cores.clone();
        Box::pin(async move { cores.send_message(&session_id, message, None, vec![]).await })
    });

    execute_core(&app_ctx, params, &tracker, &sender).await;
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

/// 核心执行逻辑（可测试入口）
pub(crate) async fn execute_core(
    app_ctx: &Arc<ServerAppContext>,
    params: ExecuteParams,
    tracker: &RunTracker,
    sender: &MessageSender,
) {
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
    let (session_id, created_new) = match resolve_session(app_ctx, &params).await {
        Ok(result) => result,
        Err(e) => {
            tracing::error!("触发 {} 解析 session 失败：{e}", params.trigger_id);
            return;
        }
    };

    // 首次执行时将 session_id 写回 job/webhook，后续复用同一会话
    if created_new {
        pin_session_to_tracker(tracker, &params.trigger_id, &session_id);
    }

    // 记录开始执行
    insert_run(tracker, &run_id, &params.trigger_id, &session_id, &now);

    // 构造消息
    let message = format!(
        "[自动化任务触发]\n任务名称：{}\n任务描述：{}\n\n{}",
        params.trigger_name, params.trigger_description, params.payload
    );

    // 发送消息到 core（不等结果）
    let result = sender(session_id, message).await;

    let finished_at = chrono::Local::now().naive_local().to_string();
    match result {
        Ok(()) => {
            update_run_success(
                tracker,
                &run_id,
                &params.trigger_id,
                &finished_at,
                "消息已发送至会话",
            );
            tracing::info!("触发 {} 消息已发送", params.trigger_id);
        }
        Err(e) => {
            let err_msg = format!("发送失败：{e}");
            update_run_failed(tracker, &run_id, &params.trigger_id, &finished_at, &err_msg);
            tracing::error!("触发 {} 发送失败：{e}", params.trigger_id);
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ServerAppContext;
    use crate::remote::event::EventBus;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tiangong_config::CoreConfigProvider;
    use tiangong_core::app_state::TiangongState;
    use tiangong_core::scheduler::model::{Job, JobRunStatus, TriggerType};
    use tiangong_core::session::Session;
    use tokio::sync::Mutex;

    fn mock_sender_ok() -> MessageSender {
        Box::new(|_sid, _msg| Box::pin(async { Ok(()) }))
    }

    fn mock_sender_err(msg: &str) -> MessageSender {
        let msg = msg.to_string();
        Box::new(move |_sid, _msg| {
            let msg = msg.clone();
            Box::pin(async move { Err(anyhow::anyhow!("{msg}")) })
        })
    }

    fn setup_app_ctx() -> (TempDir, Arc<ServerAppContext>) {
        let dir = TempDir::new().unwrap();
        let state = Arc::new(Mutex::new(TiangongState::load_or_default()));
        let config = CoreConfigProvider::new(tiangong_config::CoreConfig::default());
        let event_bus = Arc::new(EventBus::default());
        let app_ctx = Arc::new(ServerAppContext::new(state, config, event_bus));
        (dir, app_ctx)
    }

    fn setup_job_store(dir: &TempDir) -> (JobStore, Job) {
        let store = JobStore::open_at(dir.path().to_path_buf()).unwrap();
        let job = Job {
            id: "test-job-1".to_string(),
            name: "测试任务".to_string(),
            description: "测试".to_string(),
            trigger_type: TriggerType::Cron,
            schedule: Some("0 */1 * * * *".to_string()),
            session_id: None,
            payload: "hello".to_string(),
            enabled: true,
            created_at: chrono::Local::now().naive_local().to_string(),
            updated_at: chrono::Local::now().naive_local().to_string(),
        };
        store.insert_job(&job).unwrap();
        (store, job)
    }

    #[tokio::test]
    async fn execute_succeeds_and_records_run() {
        let (dir, app_ctx) = setup_app_ctx();
        let (store, _job) = setup_job_store(&dir);
        let store_path = dir.path().to_path_buf();

        let params = ExecuteParams {
            trigger_id: "test-job-1".to_string(),
            trigger_name: "测试任务".to_string(),
            trigger_description: "测试".to_string(),
            session_id: None,
            payload: "hello".to_string(),
        };

        execute_core(
            &app_ctx,
            params,
            &RunTracker::Job { store },
            &mock_sender_ok(),
        )
        .await;

        let reader = JobStore::open_at(store_path).unwrap();
        let runs = reader.list_job_runs("test-job-1", 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert!(matches!(runs[0].status, JobRunStatus::Succeeded));
        assert!(runs[0].finished_at.is_some());
    }

    #[tokio::test]
    async fn execute_failure_records_run_as_failed() {
        let (dir, app_ctx) = setup_app_ctx();
        let (store, _) = setup_job_store(&dir);
        let store_path = dir.path().to_path_buf();

        let params = ExecuteParams {
            trigger_id: "test-job-1".to_string(),
            trigger_name: "测试任务".to_string(),
            trigger_description: "测试".to_string(),
            session_id: None,
            payload: "hello".to_string(),
        };

        execute_core(
            &app_ctx,
            params,
            &RunTracker::Job { store },
            &mock_sender_err("core 不存在"),
        )
        .await;

        let reader = JobStore::open_at(store_path).unwrap();
        let runs = reader.list_job_runs("test-job-1", 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert!(matches!(runs[0].status, JobRunStatus::Failed));
        assert!(
            runs[0]
                .result_summary
                .as_deref()
                .unwrap()
                .contains("core 不存在")
        );
    }

    #[tokio::test]
    async fn session_is_reused_when_pinned() {
        let (dir, app_ctx) = setup_app_ctx();
        let (store, _) = setup_job_store(&dir);
        let store_path = dir.path().to_path_buf();

        // 第一次执行：创建 session 并 pin
        let params1 = ExecuteParams {
            trigger_id: "test-job-1".to_string(),
            trigger_name: "测试任务".to_string(),
            trigger_description: "测试".to_string(),
            session_id: None,
            payload: "hello".to_string(),
        };
        execute_core(
            &app_ctx,
            params1,
            &RunTracker::Job { store },
            &mock_sender_ok(),
        )
        .await;

        // 验证 session_id 被写回 job
        let reader = JobStore::open_at(store_path.clone()).unwrap();
        let job = reader.get_job("test-job-1").unwrap().unwrap();
        let pinned_session = job.session_id.clone().unwrap();
        assert!(!pinned_session.is_empty());

        // 在 state 中手动添加 pinned session（模拟 server 重启后的加载）
        {
            let mut state = app_ctx.state.lock().await;
            let session = Session::new_isolated("自动化任务：测试任务".to_string());
            let mut session = session;
            session.id = pinned_session.clone();
            state.sessions_mut().push(session);
        }

        // 第二次执行：应该复用同一个 session
        let store2 = JobStore::open_at(store_path.clone()).unwrap();
        let params2 = ExecuteParams {
            trigger_id: "test-job-1".to_string(),
            trigger_name: "测试任务".to_string(),
            trigger_description: "测试".to_string(),
            session_id: Some(pinned_session.clone()),
            payload: "hello again".to_string(),
        };
        execute_core(
            &app_ctx,
            params2,
            &RunTracker::Job { store: store2 },
            &mock_sender_ok(),
        )
        .await;

        let reader = JobStore::open_at(store_path).unwrap();
        let runs = reader.list_job_runs("test-job-1", 10).unwrap();
        assert_eq!(runs.len(), 2);
        assert!(
            runs.iter()
                .all(|r| matches!(r.status, JobRunStatus::Succeeded))
        );
        // 两次运行使用相同的 session
        assert_eq!(runs[0].session_id, runs[1].session_id);
    }

    #[tokio::test]
    async fn concurrent_execution_is_skipped() {
        let (dir, app_ctx) = setup_app_ctx();
        let (_store, _) = setup_job_store(&dir);
        let store_path = dir.path().to_path_buf();

        // 用 sleep 模拟长时间执行
        let slow_sender: MessageSender = Box::new(|_sid, _msg| {
            Box::pin(async {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                Ok(())
            })
        });

        let params1 = ExecuteParams {
            trigger_id: "test-job-1".to_string(),
            trigger_name: "测试任务".to_string(),
            trigger_description: "测试".to_string(),
            session_id: None,
            payload: "hello".to_string(),
        };
        let params2 = ExecuteParams {
            trigger_id: "test-job-1".to_string(),
            trigger_name: "测试任务".to_string(),
            trigger_description: "测试".to_string(),
            session_id: None,
            payload: "hello".to_string(),
        };

        let app_ctx_clone = app_ctx.clone();
        let store1 = JobStore::open_at(store_path.clone()).unwrap();

        // 启动第一个执行（慢执行）
        let h1 = tokio::spawn(async move {
            execute_core(
                &app_ctx_clone,
                params1,
                &RunTracker::Job { store: store1 },
                &slow_sender,
            )
            .await;
        });

        // 给一点时间让第一个执行获取锁
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let app_ctx_clone2 = app_ctx.clone();
        let store2 = JobStore::open_at(store_path.clone()).unwrap();

        // 第二个执行应被跳过
        let h2 = tokio::spawn(async move {
            execute_core(
                &app_ctx_clone2,
                params2,
                &RunTracker::Job { store: store2 },
                &mock_sender_ok(),
            )
            .await;
        });

        h1.await.unwrap();
        h2.await.unwrap();

        // 第二个执行被跳过，只有一条 run 记录
        let reader = JobStore::open_at(store_path).unwrap();
        let runs = reader.list_job_runs("test-job-1", 10).unwrap();
        assert_eq!(runs.len(), 1, "并发执行应被跳过，只有一条记录");
    }
}
