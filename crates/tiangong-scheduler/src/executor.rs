use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;

use crate::model::{Job, JobRun, JobRunStatus, UpdateJobRequest};
use crate::store::JobStore;
use crate::webhook::model::{UpdateWebhookRequest, Webhook, WebhookRun, WebhookRunStatus};
use crate::webhook::store::WebhookStore;

/// 调度器执行上下文，抽象消息发送和会话管理能力
///
/// Server 端通过 ServerSchedulerContext 实现（使用 ServerCoreManager），
/// Desktop 端通过 DesktopSchedulerContext 实现（使用 TiangongApp 的状态）。
#[async_trait]
pub trait SchedulerContext: Send + Sync + 'static {
    /// 发送消息到指定会话（fire-and-forget），如 Core 不存在则自动创建。
    async fn send_message(&self, session_id: &str, content: String) -> anyhow::Result<()>;

    /// 解析已有会话 ID，或为下一次投递分配新 ID。
    async fn resolve_session_id(
        &self,
        requested_session_id: Option<&str>,
    ) -> anyhow::Result<(String, bool)>;
}

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
    Webhook { store: WebhookStore },
}

/// 消息发送器类型：接受 (session_id, message)，仅发送不等结果。
type MessageSender = Box<
    dyn Fn(String, String) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>
        + Send
        + Sync,
>;

/// 正在执行的 trigger_id 集合，防止同一任务重叠执行
static RUNNING: std::sync::LazyLock<std::sync::Mutex<HashSet<String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashSet::new()));

/// 执行定时任务
pub async fn execute_job(ctx: Arc<dyn SchedulerContext>, job: Job) {
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

    execute(ctx, params, RunTracker::Job { store }).await;
}

/// 执行 webhook 触发
pub async fn execute_webhook(ctx: Arc<dyn SchedulerContext>, webhook: Webhook) {
    let store = match WebhookStore::open() {
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

    execute(ctx, params, RunTracker::Webhook { store }).await;
}

/// 触发执行（生产入口）
async fn execute(ctx: Arc<dyn SchedulerContext>, params: ExecuteParams, tracker: RunTracker) {
    let ctx_clone = ctx.clone();
    let sender: MessageSender = Box::new(move |session_id, message| {
        let ctx = ctx_clone.clone();
        Box::pin(async move { ctx.send_message(&session_id, message).await })
    });

    execute_core(ctx.as_ref(), params, &tracker, &sender).await;
}

/// 核心执行逻辑
async fn execute_core(
    ctx: &dyn SchedulerContext,
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
    let (session_id, created_new) = match ctx.resolve_session_id(params.session_id.as_deref()).await
    {
        Ok(result) => result,
        Err(e) => {
            tracing::error!("触发 {} 解析 session 失败：{e}", params.trigger_id);
            return;
        }
    };

    // 记录开始执行（本轮使用的 session，即便随后失败也保留痕迹）
    insert_run(tracker, &run_id, &params.trigger_id, &session_id, &now);

    // 构造消息
    let message = format!(
        "[定时任务触发]\n任务名称：{}\n任务描述：{}\n\n{}",
        params.trigger_name, params.trigger_description, params.payload
    );

    // 发送消息到 core（不等结果）
    let result = sender(session_id.clone(), message).await;

    let finished_at = chrono::Local::now().naive_local().to_string();
    match result {
        Ok(()) => {
            // 投递成功后才把新 session_id 写回 job/webhook：失败时新会话很可能尚未
            // 落盘，立刻绑定会让后续触发因 session_exists==false 反复换新 id，
            // 彻底丢失关联（见 resolve_session_id 仅按磁盘文件判定）。
            if created_new {
                pin_session_to_tracker(tracker, &params.trigger_id, &session_id);
            }
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

/// 从 job store 加载已启用的 cron job 并注册到 silent scheduler
pub async fn restore_cron_jobs(ctx: Arc<dyn SchedulerContext>) {
    let store = match JobStore::open() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("恢复定时任务失败，跳过：{e}");
            return;
        }
    };

    let jobs = match store.list_enabled_cron_jobs() {
        Ok(jobs) => jobs,
        Err(e) => {
            tracing::warn!("查询定时任务失败，跳过：{e}");
            return;
        }
    };

    let mut scheduler = silent::SCHEDULER.lock().await;
    for job in jobs {
        let schedule = match &job.schedule {
            Some(s) => s.clone(),
            None => continue,
        };
        let job_clone = job.clone();
        let process_time = match schedule.try_into() {
            Ok(pt) => pt,
            Err(e) => {
                tracing::warn!("解析 cron 表达式失败 [{}]: {e}", job.schedule.unwrap());
                continue;
            }
        };
        let ctx_clone = ctx.clone();
        let task = silent::Task::create_with_action_async(
            job.id.clone(),
            process_time,
            job.name.clone(),
            Arc::new(move || {
                let job = job_clone.clone();
                let ctx = ctx_clone.clone();
                Box::pin(async move {
                    tracing::info!("定时任务触发：{} [{}]", job.name, job.id);
                    execute_job(ctx, job).await;
                    Ok(())
                })
            }),
        );
        if let Err(e) = scheduler.add_task(task) {
            tracing::warn!("注册定时任务失败 [{}]：{e}", job.id);
        } else {
            tracing::info!("已恢复定时任务：{} [{}]", job.name, job.id);
        }
    }
}

// ── 内部方法 ──────────────────────────────────────────────────

/// 将 session_id 写回 job/webhook，确保后续执行复用同一会话
fn pin_session_to_tracker(tracker: &RunTracker, trigger_id: &str, session_id: &str) {
    match tracker {
        RunTracker::Job { store } => {
            let req = UpdateJobRequest {
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
            let req = UpdateWebhookRequest {
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
            let run = JobRun {
                id: run_id.to_string(),
                job_id: trigger_id.to_string(),
                session_id: session_id.to_string(),
                status: JobRunStatus::Running,
                started_at: started_at.to_string(),
                finished_at: None,
                result_summary: None,
            };
            if let Err(e) = store.insert_job_run(&run) {
                tracing::error!("记录 JobRun 失败：{e}");
            }
        }
        RunTracker::Webhook { store } => {
            let run = WebhookRun {
                id: run_id.to_string(),
                webhook_id: trigger_id.to_string(),
                session_id: session_id.to_string(),
                status: WebhookRunStatus::Running,
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
                &JobRunStatus::Succeeded,
                Some(finished_at),
                Some(summary),
            );
        }
        RunTracker::Webhook { store } => {
            let _ = store.update_run_status(
                run_id,
                trigger_id,
                &WebhookRunStatus::Succeeded,
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
                &JobRunStatus::Failed,
                Some(finished_at),
                Some(error),
            );
        }
        RunTracker::Webhook { store } => {
            let _ = store.update_run_status(
                run_id,
                trigger_id,
                &WebhookRunStatus::Failed,
                Some(finished_at),
                Some(error),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use tempfile::TempDir;

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

    struct MockContext {
        sessions: StdMutex<Vec<String>>,
    }

    #[async_trait]
    impl SchedulerContext for MockContext {
        async fn send_message(&self, _session_id: &str, _content: String) -> anyhow::Result<()> {
            Ok(())
        }

        async fn resolve_session_id(
            &self,
            requested_session_id: Option<&str>,
        ) -> anyhow::Result<(String, bool)> {
            if let Some(sid) = requested_session_id {
                let sessions = self.sessions.lock().unwrap();
                if sessions.iter().any(|s| s == sid) {
                    return Ok((sid.to_string(), false));
                }
            }
            let session_id = scru128::new().to_string();
            self.sessions.lock().unwrap().push(session_id.clone());
            Ok((session_id, true))
        }
    }

    fn setup_job_store(dir: &TempDir, job_id: &str) -> (JobStore, Job) {
        let store = JobStore::open_at(dir.path().to_path_buf()).unwrap();
        let job = Job {
            id: job_id.to_string(),
            name: "测试任务".to_string(),
            description: "测试".to_string(),
            trigger_type: crate::model::TriggerType::Cron,
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
        let dir = TempDir::new().unwrap();
        let (store, _job) = setup_job_store(&dir, "test-job-succeed");
        let store_path = dir.path().to_path_buf();

        let ctx = Arc::new(MockContext {
            sessions: StdMutex::new(vec![]),
        });
        let params = ExecuteParams {
            trigger_id: "test-job-succeed".to_string(),
            trigger_name: "测试任务".to_string(),
            trigger_description: "测试".to_string(),
            session_id: None,
            payload: "hello".to_string(),
        };

        execute_core(
            ctx.as_ref(),
            params,
            &RunTracker::Job { store },
            &mock_sender_ok(),
        )
        .await;

        let reader = JobStore::open_at(store_path).unwrap();
        let runs = reader.list_job_runs("test-job-succeed", 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert!(matches!(runs[0].status, JobRunStatus::Succeeded));
        assert!(runs[0].finished_at.is_some());
    }

    #[tokio::test]
    async fn execute_failure_records_run_as_failed() {
        let dir = TempDir::new().unwrap();
        let (store, _) = setup_job_store(&dir, "test-job-failure");
        let store_path = dir.path().to_path_buf();

        let ctx = Arc::new(MockContext {
            sessions: StdMutex::new(vec![]),
        });
        let params = ExecuteParams {
            trigger_id: "test-job-failure".to_string(),
            trigger_name: "测试任务".to_string(),
            trigger_description: "测试".to_string(),
            session_id: None,
            payload: "hello".to_string(),
        };

        execute_core(
            ctx.as_ref(),
            params,
            &RunTracker::Job { store },
            &mock_sender_err("core 不存在"),
        )
        .await;

        let reader = JobStore::open_at(store_path).unwrap();
        let runs = reader.list_job_runs("test-job-failure", 10).unwrap();
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
    async fn concurrent_execution_is_skipped() {
        let dir = TempDir::new().unwrap();
        let (_store, _) = setup_job_store(&dir, "test-job-concurrent");
        let store_path = dir.path().to_path_buf();

        let slow_sender: MessageSender = Box::new(|_sid, _msg| {
            Box::pin(async {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                Ok(())
            })
        });

        let ctx = Arc::new(MockContext {
            sessions: StdMutex::new(vec![]),
        });

        let params1 = ExecuteParams {
            trigger_id: "test-job-concurrent".to_string(),
            trigger_name: "测试任务".to_string(),
            trigger_description: "测试".to_string(),
            session_id: None,
            payload: "hello".to_string(),
        };
        let params2 = ExecuteParams {
            trigger_id: "test-job-concurrent".to_string(),
            trigger_name: "测试任务".to_string(),
            trigger_description: "测试".to_string(),
            session_id: None,
            payload: "hello".to_string(),
        };

        let store1 = JobStore::open_at(store_path.clone()).unwrap();
        let store2 = JobStore::open_at(store_path.clone()).unwrap();

        let ctx_clone = ctx.clone();
        let h1 = tokio::spawn(async move {
            execute_core(
                ctx_clone.as_ref(),
                params1,
                &RunTracker::Job { store: store1 },
                &slow_sender,
            )
            .await;
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let ctx_clone2 = ctx.clone();
        let h2 = tokio::spawn(async move {
            execute_core(
                ctx_clone2.as_ref(),
                params2,
                &RunTracker::Job { store: store2 },
                &mock_sender_ok(),
            )
            .await;
        });

        h1.await.unwrap();
        h2.await.unwrap();

        let reader = JobStore::open_at(store_path).unwrap();
        let runs = reader.list_job_runs("test-job-concurrent", 10).unwrap();
        assert_eq!(runs.len(), 1, "并发执行应被跳过，只有一条记录");
    }

    // 投递成功才把新 session_id 写回 Job：失败时不绑定，下轮重新创建。
    #[tokio::test]
    async fn pins_session_id_only_after_successful_delivery() {
        let dir = TempDir::new().unwrap();
        let (store, _) = setup_job_store(&dir, "test-job-pin-success");
        let store_path = dir.path().to_path_buf();

        let ctx = Arc::new(MockContext {
            sessions: StdMutex::new(vec![]),
        });
        let params = ExecuteParams {
            trigger_id: "test-job-pin-success".to_string(),
            trigger_name: "测试任务".to_string(),
            trigger_description: "测试".to_string(),
            session_id: None,
            payload: "hello".to_string(),
        };

        execute_core(
            ctx.as_ref(),
            params,
            &RunTracker::Job { store },
            &mock_sender_ok(),
        )
        .await;

        let reader = JobStore::open_at(store_path).unwrap();
        let job = reader.get_job("test-job-pin-success").unwrap().unwrap();
        assert!(
            job.session_id.is_some(),
            "投递成功后应把新 session_id 写回 Job"
        );
    }

    // 投递失败不得绑定新 session_id：否则该 id 多半未落盘，下次触发会因
    // session_exists==false 反复换新 id，关联彻底丢失。
    #[tokio::test]
    async fn does_not_pin_session_id_on_delivery_failure() {
        let dir = TempDir::new().unwrap();
        let (store, _) = setup_job_store(&dir, "test-job-pin-failure");
        let store_path = dir.path().to_path_buf();

        let ctx = Arc::new(MockContext {
            sessions: StdMutex::new(vec![]),
        });
        let params = ExecuteParams {
            trigger_id: "test-job-pin-failure".to_string(),
            trigger_name: "测试任务".to_string(),
            trigger_description: "测试".to_string(),
            session_id: None,
            payload: "hello".to_string(),
        };

        execute_core(
            ctx.as_ref(),
            params,
            &RunTracker::Job { store },
            &mock_sender_err("core 投递失败"),
        )
        .await;

        let reader = JobStore::open_at(store_path).unwrap();
        let job = reader.get_job("test-job-pin-failure").unwrap().unwrap();
        assert!(
            job.session_id.is_none(),
            "投递失败时不得把未落盘的 session_id 写回 Job"
        );
    }

    // 复用既有会话（created_new=false）时不应触发写回。
    #[tokio::test]
    async fn reuse_existing_session_does_not_rewrite_job() {
        let dir = TempDir::new().unwrap();
        let existing_session = "session-existing";
        let (store, _job) = setup_job_store(&dir, "test-job-reuse");
        // 预置既有 session_id，模拟任务已绑定到一个落盘会话
        let store_path = dir.path().to_path_buf();
        {
            let req = UpdateJobRequest {
                session_id: Some(existing_session.to_string()),
                ..Default::default()
            };
            store.update_job("test-job-reuse", &req).unwrap();
        }

        let ctx = Arc::new(MockContext {
            sessions: StdMutex::new(vec![existing_session.to_string()]),
        });
        let params = ExecuteParams {
            trigger_id: "test-job-reuse".to_string(),
            trigger_name: "测试任务".to_string(),
            trigger_description: "测试".to_string(),
            session_id: Some(existing_session.to_string()),
            payload: "hello".to_string(),
        };

        execute_core(
            ctx.as_ref(),
            params,
            &RunTracker::Job { store },
            &mock_sender_ok(),
        )
        .await;

        let reader = JobStore::open_at(store_path).unwrap();
        let job = reader.get_job("test-job-reuse").unwrap().unwrap();
        assert_eq!(job.session_id.as_deref(), Some(existing_session));
    }
}
