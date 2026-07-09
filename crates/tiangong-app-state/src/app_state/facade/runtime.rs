use tiangong_core::core_config::ModelEndpoint;
use tiangong_core::models_config::RoutingSlot;

use super::super::*;

impl TiangongState {
    pub(in crate::app_state) fn rebuild_runtime_from_current_config(&mut self) {
        let endpoint = self
            .store
            .provider
            .models_config
            .resolve_slot(RoutingSlot::Chat)
            .map(ModelEndpoint::from_resolved)
            .unwrap_or_default();
        self.store.provider.model_endpoint = endpoint.clone();
        // 保留旧 RuntimeEngine 的共享信任模式引用和 tool_overrides
        let shared_trust_mode = self
            .services
            .runtime
            .permission_gate()
            .shared_trust_mode_ref();
        let tool_overrides = self.services.runtime.tool_overrides();
        let tool_spec_providers = self.services.runtime.tool_spec_providers();
        let prompt_section_providers = self.services.runtime.prompt_section_providers();
        let storage_dir = tiangong_config::io::storage_root();
        let context_limit = tiangong_config::io::resolve_context_limit_at(
            &storage_dir,
            &self.store.provider.model_endpoint.model,
        );
        let new_runtime = RuntimeEngine::with_shared_trust_mode(
            SingleProviderClient::new(endpoint),
            context_limit,
            self.store.agent.agent_config.clone(),
            shared_trust_mode,
            crate::app_state::repository::storage_root(),
        )
        .with_models_config(self.store.provider.models_config.clone());
        for (name, handler) in tool_overrides {
            new_runtime.register_tool_override(&name, handler);
        }
        for provider in tool_spec_providers {
            new_runtime.register_tool_spec_provider(provider);
        }
        for provider in prompt_section_providers {
            new_runtime.register_prompt_section_provider(provider);
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
