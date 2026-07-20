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

    async fn resolve_session_id(
        &self,
        requested_session_id: Option<&str>,
    ) -> anyhow::Result<(String, bool)> {
        if let Some(sid) = requested_session_id {
            let state = self.state.lock().await;
            if state.core_manager.session_exists(sid) {
                return Ok((sid.to_string(), false));
            }
        }
        Ok((scru128::new().to_string(), true))
    }
}
