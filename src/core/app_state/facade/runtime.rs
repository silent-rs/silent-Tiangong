use super::super::*;

impl TiangongState {
    pub(in crate::core::app_state) fn rebuild_runtime_from_current_config(&mut self) {
        self.services.runtime = RuntimeEngine::new(
            SingleProviderClient::new(self.store.provider.model_config.clone()),
            DEFAULT_CONTEXT_LIMIT,
            self.store.agent.agent_config.clone(),
        );
    }

    pub(in crate::core::app_state) fn replace_run_snapshot(
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
