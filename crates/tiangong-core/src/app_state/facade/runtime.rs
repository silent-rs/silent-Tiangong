use super::super::*;

impl TiangongState {
    pub(in crate::app_state) fn rebuild_runtime_from_current_config(&mut self) {
        let config = self.store.provider.models_config.to_chat_provider_config();
        self.store.provider.model_config = config.clone();
        self.services.runtime = RuntimeEngine::new(
            SingleProviderClient::new(config),
            DEFAULT_CONTEXT_LIMIT,
            self.store.agent.agent_config.clone(),
        );
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
        };
    }
}
