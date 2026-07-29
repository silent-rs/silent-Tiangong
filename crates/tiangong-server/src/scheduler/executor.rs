use std::sync::Arc;

use tiangong_scheduler::executor::SchedulerContext;

/// 执行定时任务（委托给 tiangong_scheduler::executor）
pub async fn execute_job(ctx: Arc<dyn SchedulerContext>, job: tiangong_scheduler::model::Job) {
    tiangong_scheduler::executor::execute_job(ctx, job).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ServerAppContext;
    use crate::remote::event::EventBus;
    use crate::scheduler::context::ServerSchedulerContext;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::Mutex;

    fn setup_app_ctx() -> Arc<ServerAppContext> {
        let state = tiangong_app_state::app_state::TiangongState::new();
        let core_manager = state.core_manager.clone();
        let storage_root = state.config.storage_root.clone();
        let state = Arc::new(Mutex::new(state));
        let event_bus = Arc::new(EventBus::default());
        Arc::new(ServerAppContext::new(
            state,
            core_manager,
            event_bus,
            storage_root,
        ))
    }

    #[tokio::test]
    async fn server_context_resolve_only_allocates_session_id() {
        let _storage_guard = crate::remote::core::test_support::STORAGE_TEST_LOCK
            .lock()
            .await;
        let root = TempDir::new().unwrap();
        let _home_guard =
            crate::remote::core::test_support::TestHomeGuard::new(&root.path().join("home"));
        let app_ctx = setup_app_ctx();
        let ctx = ServerSchedulerContext {
            state: app_ctx.state.clone(),
            core_backend: app_ctx.core_backend.clone(),
        };

        let (sid, created) = ctx.resolve_session_id(None).await.unwrap();
        assert!(created);
        assert!(!sid.is_empty());
        assert!(!app_ctx.state.lock().await.core_manager.session_exists(&sid));
    }
}
