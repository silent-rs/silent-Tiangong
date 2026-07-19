use std::sync::Arc;

use async_trait::async_trait;
use tiangong_scheduler::executor::SchedulerContext;

use crate::api::SharedState;
use crate::remote::backend::ServerCoreBackend;

/// Server 端调度器执行上下文
///
/// 通过注入的 Core 后端发送消息，通过 SharedState 管理会话。
pub struct ServerSchedulerContext {
    pub state: SharedState,
    pub core_backend: Arc<dyn ServerCoreBackend>,
}

#[async_trait]
impl SchedulerContext for ServerSchedulerContext {
    async fn send_message(&self, session_id: &str, content: String) -> anyhow::Result<()> {
        self.core_backend
            .send_message_and_wait(session_id, content, None, vec![])
            .await
            .map(|_| ())
    }

    async fn resolve_or_create_session(
        &self,
        requested_session_id: Option<&str>,
        trigger_name: &str,
    ) -> anyhow::Result<(String, bool)> {
        if let Some(sid) = requested_session_id {
            let state = self.state.lock().await;
            if state.session_metadata().iter().any(|m| m.id == *sid) {
                return Ok((sid.to_string(), false));
            }
        }

        let mut state = self.state.lock().await;
        let title = format!("定时任务：{}", trigger_name);
        let session = tiangong_core::session::Session::new_isolated(
            title,
            &tiangong_app_state::app_state::storage_root(),
        );
        let session_id = session.id.clone();
        state.add_session(session);
        state.persist_session(&session_id)?;
        Ok((session_id, true))
    }
}
