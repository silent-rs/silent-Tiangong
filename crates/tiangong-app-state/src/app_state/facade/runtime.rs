use tiangong_core::context::organizer::ContextOrganizer;
use tiangong_llm::ModelEndpoint;
use tiangong_llm::models_config::{ModelsConfig, RoutingSlot};

use super::super::*;

impl TiangongState {
    pub(in crate::app_state) fn resolve_chat_context_limit(
        models_config: &ModelsConfig,
        model_name: &str,
    ) -> usize {
        if model_name.is_empty() {
            return tiangong_core::core_config::default_context_limit();
        }
        let chat_override = models_config
            .resolve_slot(RoutingSlot::Chat)
            .and_then(|resolved| resolved.context_window);
        tiangong_config::io::resolve_context_limit_with_override(
            &tiangong_config::io::storage_root(),
            model_name,
            chat_override,
        )
    }

    pub(in crate::app_state) fn apply_derived_context_metrics(
        session: &mut Session,
        context_limit: usize,
    ) {
        session.context_limit_tokens = context_limit;
        session.compression_threshold_tokens =
            ContextOrganizer::new(context_limit).token_threshold();
    }

    pub(in crate::app_state) fn rebuild_runtime_from_current_config(&mut self) {
        let endpoint = self
            .store
            .provider
            .models_config
            .resolve_slot(RoutingSlot::Chat)
            .map(ModelEndpoint::from_resolved)
            .unwrap_or_default();
        self.store.provider.model_endpoint = endpoint.clone();
        // 保留旧 RuntimeEngine 的任务隔离信任模式解析句柄和 tool_overrides
        let trust_mode = self
            .store
            .session
            .sessions
            .iter()
            .find(|s| s.id == self.store.session.active_session_id)
            .map(|s| s.trust_mode)
            .unwrap_or_default();
        let tool_overrides = self.services.runtime.tool_overrides();
        let tool_spec_providers = self.services.runtime.tool_spec_providers();
        let prompt_section_providers = self.services.runtime.prompt_section_providers();
        let context_limit = Self::resolve_chat_context_limit(
            &self.store.provider.models_config,
            &self.store.provider.model_endpoint.model,
        );
        let new_runtime = RuntimeEngine::with_shared_trust_mode(
            SingleProviderClient::new(endpoint),
            context_limit,
            self.store.agent.agent_config.clone(),
            trust_mode,
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
        for session in &mut self.store.session.sessions {
            Self::apply_derived_context_metrics(session, context_limit);
        }
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
