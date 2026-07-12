use anyhow::Result;
use tiangong_core::context::organizer::ContextOrganizer;
use tiangong_llm::models_config::{ModelEntry, ModelsConfig, ProviderConfig, RoutingSlot};

use super::super::*;
use super::common::with_isolated_state;

#[test]
fn report_run_helpers_preserve_existing_snapshot_fields() -> Result<()> {
    with_isolated_state("tiangong-run-snapshot-helpers", |_paths, state| {
        state.store.runtime.run.last_session_id = Some("session-1".to_string());
        state.store.runtime.run.last_task_id = Some("task-1".to_string());
        state.store.runtime.run.last_duration_ms = Some(123);
        state.store.runtime.run.last_result = Some("previous-result".to_string());
        state.store.runtime.run.last_plan = Some("previous-plan".to_string());
        state.store.runtime.run.last_tool_result = Some("previous-tool".to_string());

        state.report_run_failed("失败摘要", "mock-error");
        assert!(matches!(state.store.runtime.run.status, RunStatus::Failed));
        assert_eq!(state.store.runtime.run.summary, "失败摘要");
        assert_eq!(
            state.store.runtime.run.last_error.as_deref(),
            Some("mock-error")
        );
        assert_eq!(
            state.store.runtime.run.last_session_id.as_deref(),
            Some("session-1")
        );
        assert_eq!(
            state.store.runtime.run.last_tool_result.as_deref(),
            Some("previous-tool")
        );

        state.report_run_idle("恢复空闲");
        assert!(matches!(state.store.runtime.run.status, RunStatus::Idle));
        assert_eq!(state.store.runtime.run.summary, "恢复空闲");
        assert!(state.store.runtime.run.last_error.is_none());
        assert_eq!(
            state.store.runtime.run.last_task_id.as_deref(),
            Some("task-1")
        );
        assert_eq!(
            state.store.runtime.run.last_plan.as_deref(),
            Some("previous-plan")
        );
        Ok(())
    })
}

#[test]
fn pending_messages_keep_queued_next_turn_after_current_completion() -> Result<()> {
    with_isolated_state("tiangong-pending-message-state", |_paths, state| {
        let session_id = state.active_session_id().to_string();
        state.mark_pending_message_for(&session_id, "message-a");
        state.accept_pending_message_for(&session_id, "message-a");
        state.mark_pending_message_for(&session_id, "message-b");

        assert!(state.has_active_turn_for(&session_id));
        state.complete_accepted_turn_for(&session_id);
        assert!(state.has_pending_turn_for(&session_id));
        assert!(!state.has_active_turn_for(&session_id));

        state.accept_pending_message_for(&session_id, "message-b");
        assert!(state.has_active_turn_for(&session_id));
        state.complete_accepted_turn_for(&session_id);
        assert!(!state.has_pending_turn_for(&session_id));
        Ok(())
    })
}

#[test]
fn derived_context_metrics_follow_chat_override_for_rebuild_create_and_reload() -> Result<()> {
    with_isolated_state("tiangong-derived-context-metrics", |paths, state| {
        let context_limit = 123_456;
        let model = ModelEntry {
            provider: "test-provider".to_string(),
            model: "unknown-context-model".to_string(),
            context_window: Some(context_limit),
            ..Default::default()
        };
        let mut models = ModelsConfig::default();
        models.providers.insert(
            "test-provider".to_string(),
            ProviderConfig {
                base_url: "https://example.invalid/v1".to_string(),
                api_key: String::new(),
                timeout_ms: 60_000,
                protocol: Default::default(),
            },
        );
        models.models.insert("chat".to_string(), model.clone());
        models.routing.insert(RoutingSlot::Chat, model);
        tiangong_config::io::save_models_config_at(&paths.fake_home.join(".tiangong"), &models)?;
        state.store.provider.models_config = models;
        state.rebuild_runtime_from_current_config();

        let threshold = ContextOrganizer::new(context_limit).token_threshold();
        assert_eq!(state.services.runtime.context_limit, context_limit);
        assert!(state.sessions().iter().all(|session| {
            session.context_limit_tokens == context_limit
                && session.compression_threshold_tokens == threshold
        }));
        let core_config = state.build_core_config_for_session_from_base(
            &tiangong_core::core_config::CoreConfig::default(),
            state.active_session_id(),
        );
        assert_eq!(core_config.context_limit, context_limit);

        state.create_session();
        let session_id = state.active_session_id().to_string();
        let session = state
            .sessions_mut()
            .iter_mut()
            .find(|session| session.id == session_id)
            .expect("new session");
        assert_eq!(session.context_limit_tokens, context_limit);
        assert_eq!(session.compression_threshold_tokens, threshold);
        session.current_tokens = 12_345;
        session.token_usage.total_tokens = 678;
        state.persist_session(&session_id)?;

        let session_path = paths
            .fake_home
            .join(".tiangong")
            .join("sessions")
            .join(format!("{session_id}.json"));
        let mut persisted: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&session_path)?)?;
        persisted["compression_threshold_tokens"] = serde_json::json!(999_999);
        persisted["context_limit_tokens"] = serde_json::json!(1_000_000);
        std::fs::write(&session_path, serde_json::to_vec_pretty(&persisted)?)?;

        assert!(state.reload_session_from_disk(&session_id)?);
        let restored = state
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
            .expect("restored session");
        assert_eq!(restored.context_limit_tokens, context_limit);
        assert_eq!(restored.compression_threshold_tokens, threshold);
        assert_eq!(restored.current_tokens, 12_345);
        assert_eq!(restored.token_usage.total_tokens, 678);

        state.persist_session(&session_id)?;
        let scrubbed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&session_path)?)?;
        assert!(scrubbed.get("compression_threshold_tokens").is_none());
        assert!(scrubbed.get("context_limit_tokens").is_none());

        let restarted = TiangongState::load_or_default();
        let restarted_session = restarted
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
            .expect("restarted session");
        assert_eq!(restarted.services.runtime.context_limit, context_limit);
        assert_eq!(restarted_session.context_limit_tokens, context_limit);
        assert_eq!(restarted_session.compression_threshold_tokens, threshold);
        assert_eq!(restarted_session.current_tokens, 12_345);
        assert_eq!(restarted_session.token_usage.total_tokens, 678);
        Ok(())
    })
}
