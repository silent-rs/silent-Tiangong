use std::sync::Arc;

use tiangong_scheduler::executor::SchedulerContext;

/// 执行定时任务（委托给 tiangong_scheduler::executor）
pub async fn execute_job(
    ctx: Arc<dyn SchedulerContext>,
    job: tiangong_core::scheduler::model::Job,
) {
    tiangong_scheduler::executor::execute_job(ctx, job).await;
}

/// 执行 webhook 触发（委托给 tiangong_scheduler::executor）
pub async fn execute_webhook(
    ctx: Arc<dyn SchedulerContext>,
    webhook: crate::webhook::model::Webhook,
) {
    tiangong_scheduler::executor::execute_webhook(ctx, webhook).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ServerAppContext;
    use crate::remote::event::EventBus;
    use crate::scheduler::context::ServerSchedulerContext;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tiangong_config::CoreConfigProvider;
    use tiangong_core::app_state::TiangongState;
    use tokio::sync::Mutex;

    fn setup_app_ctx() -> (TempDir, Arc<ServerAppContext>) {
        let dir = TempDir::new().unwrap();
        let state = Arc::new(Mutex::new(TiangongState::load_or_default()));
        let config = CoreConfigProvider::new(tiangong_config::CoreConfig::default());
        let event_bus = Arc::new(EventBus::default());
        let app_ctx = Arc::new(ServerAppContext::new(state, config, event_bus));
        (dir, app_ctx)
    }

    #[tokio::test]
    async fn server_context_resolve_creates_session() {
        let (_dir, app_ctx) = setup_app_ctx();
        let ctx = ServerSchedulerContext {
            state: app_ctx.state.clone(),
            cores: app_ctx.cores.clone(),
        };

        let (sid, created) = ctx
            .resolve_or_create_session(None, "测试任务")
            .await
            .unwrap();
        assert!(created);
        assert!(!sid.is_empty());

        // 再次调用应复用
        let (sid2, created2) = ctx
            .resolve_or_create_session(Some(&sid), "测试任务")
            .await
            .unwrap();
        assert!(!created2);
        assert_eq!(sid, sid2);
    }
}
