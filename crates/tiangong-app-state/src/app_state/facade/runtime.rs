use tiangong_core::context::organizer::ContextOrganizer;
use tiangong_llm::ModelEndpoint;
use tiangong_llm::models_config::{ModelsConfig, RoutingSlot};

use crate::app_state::support::RuntimeConfig;

use super::super::*;

/// 解析当前 chat 模型的上下文窗口上限(纯函数,issue #245:从 impl 方法
/// 抽出,供 RuntimeConfig 派生与既有调用方共用)。
pub(in crate::app_state) fn resolve_chat_context_limit_inner(
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

impl TiangongState {
    pub(in crate::app_state) fn resolve_chat_context_limit(
        models_config: &ModelsConfig,
        model_name: &str,
    ) -> usize {
        resolve_chat_context_limit_inner(models_config, model_name)
    }

    pub(in crate::app_state) fn apply_derived_context_metrics(
        session: &mut Session,
        context_limit: usize,
    ) {
        session.context_limit_tokens = context_limit;
        session.compression_threshold_tokens =
            ContextOrganizer::new(context_limit).token_threshold();
    }

    /// 从当前 models_config 重算轻量运行时缓存与所有会话的派生上下文指标
    /// (issue #245:替代原 RuntimeEngine 重建)。
    ///
    /// 原 RuntimeEngine 持有的 client / tool_overrides / plugin provider registry
    /// 已不再需要——Core 每 turn 自行从 plugin 集合构造工具,app-state 不执行 turn。
    /// 这里只刷新:chat endpoint 镜像、context_limit 缓存、各会话派生量。
    pub(in crate::app_state) fn rebuild_runtime_from_current_config(&mut self) {
        let endpoint = self
            .store
            .provider
            .models_config
            .resolve_slot(RoutingSlot::Chat)
            .map(ModelEndpoint::from_resolved)
            .unwrap_or_default();
        self.store.provider.model_endpoint = endpoint.clone();
        let context_limit = resolve_chat_context_limit_inner(
            &self.store.provider.models_config,
            &self.store.provider.model_endpoint.model,
        );
        self.services.runtime = RuntimeConfig { context_limit };
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
