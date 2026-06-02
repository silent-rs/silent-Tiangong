use super::super::*;

impl TiangongState {
    pub(in crate::app_state) fn rebuild_runtime_from_current_config(&mut self) {
        let config = self.store.provider.models_config.to_chat_provider_config();
        self.store.provider.model_config = config.clone();
        // 保留旧 RuntimeEngine 的共享信任模式引用和浏览器通道
        let shared_trust_mode = self
            .services
            .runtime
            .permission_gate()
            .shared_trust_mode_ref();
        let browser_tx = self.services.runtime.browser_tx();
        let context_limit =
            crate::core_config::resolve_context_limit(&self.store.provider.model_config.api_model);
        let new_runtime = RuntimeEngine::with_shared_trust_mode(
            SingleProviderClient::new(config),
            context_limit,
            self.store.agent.agent_config.clone(),
            shared_trust_mode,
        )
        .with_models_config(self.store.provider.models_config.clone());
        if let Some(tx) = browser_tx {
            new_runtime.set_browser_channel(tx);
        }
        self.services.runtime = new_runtime;
    }

    pub(in crate::app_state) fn replace_run_snapshot(
        &mut self,
        status: RunStatus,
        summary: impl Into<String>,
        last_error: Option<String>,
    ) {
        self.store.runtime.run = RunSnapshot {
            status,
            summary: summary.into(),
            last_session_id: self.store.runtime.run.last_session_id.clone(),
            last_task_id: self.store.runtime.run.last_task_id.clone(),
            last_duration_ms: self.store.runtime.run.last_duration_ms,
            last_result: self.store.runtime.run.last_result.clone(),
            last_plan: self.store.runtime.run.last_plan.clone(),
            last_tool_result: self.store.runtime.run.last_tool_result.clone(),
            last_error,
            last_usage: self.store.runtime.run.last_usage.clone(),
            updated_at: now_text(),
            approval_request_id: None,
        };
    }
}
