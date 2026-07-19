use tiangong_llm::ModelEndpoint;
use tiangong_llm::models_config::RoutingSlot;

use super::super::*;

impl TiangongState {
    /// 从当前 models_config 刷新 chat endpoint 镜像（provider_label 等读取）。
    ///
    /// issue #245:不再缓存 context_limit / 反写 Session 派生量——context_limit
    /// 由 Core 从 CoreConfig(模型配置派生)每 turn 读取,app-state 不参与。
    pub(in crate::app_state) fn refresh_chat_endpoint(&mut self) {
        self.store.provider.model_endpoint = self
            .store
            .provider
            .models_config
            .resolve_slot(RoutingSlot::Chat)
            .map(ModelEndpoint::from_resolved)
            .unwrap_or_default();
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
