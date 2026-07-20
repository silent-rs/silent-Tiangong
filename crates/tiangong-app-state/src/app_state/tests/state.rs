use anyhow::Result;
use std::fs;

use super::super::repository::canonical_scru128_id;
use super::super::*;
use super::common::with_isolated_state;

#[test]
fn normalize_sessions_repairs_invalid_ids_and_active_session() -> Result<()> {
    with_isolated_state("tiangong-state-normalize", |_paths, state| {
        state.sessions = vec![Session::new("A"), Session::new("B")];
        state.sessions[0].id = "invalid-id".to_string();
        state.sessions[1].id = "invalid-id".to_string();
        state.active_session_id = "invalid-active-id".to_string();

        state.normalize_sessions_for_storage();

        assert_eq!(state.sessions.len(), 2);
        let first_id = state.sessions[0].id.clone();
        let second_id = state.sessions[1].id.clone();
        assert_eq!(canonical_scru128_id(&first_id), Some(first_id.clone()));
        assert_eq!(canonical_scru128_id(&second_id), Some(second_id.clone()));
        assert_ne!(first_id, second_id);
        assert_eq!(state.active_session_id, first_id);
        Ok(())
    })
}

#[test]
fn prepare_active_user_message_ingress_persists_message_immediately() -> Result<()> {
    with_isolated_state("tiangong-state-ingress-persist", |paths, state| {
        state.create_session();

        let (session_id, message_id, session) =
            state.prepare_active_user_message_ingress("立即固定这条消息")?;

        assert_eq!(state.active_session_id(), session_id);
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.messages[0].id, message_id);
        assert_eq!(session.messages[0].text_content(), "立即固定这条消息");

        let session_path = paths
            .fake_home
            .join(".tiangong")
            .join("sessions")
            .join(format!("{session_id}.json"));
        assert!(session_path.exists());

        let persisted: Session = serde_json::from_str(&fs::read_to_string(session_path)?)?;
        assert_eq!(persisted.messages.len(), 1);
        assert_eq!(persisted.messages[0].id, message_id);
        assert_eq!(persisted.messages[0].text_content(), "立即固定这条消息");
        Ok(())
    })
}

#[test]
fn trust_mode_is_session_scoped_and_default_only_initializes_new_sessions() -> Result<()> {
    with_isolated_state("tiangong-state-session-trust-mode", |_paths, state| {
        state.agent_config.default_trust_mode = tiangong_core::permission::TrustMode::FullTrust;
        state.create_session();
        let first_id = state.active_session_id().to_string();
        assert_eq!(
            state.active_session_trust_mode(),
            tiangong_core::permission::TrustMode::FullTrust
        );

        state.set_trust_mode(tiangong_core::permission::TrustMode::Supervised)?;
        assert_eq!(
            state
                .sessions()
                .iter()
                .find(|session| session.id == first_id)
                .map(|session| session.trust_mode),
            Some(tiangong_core::permission::TrustMode::Supervised)
        );

        state.agent_config.default_trust_mode = tiangong_core::permission::TrustMode::FullTrust;
        state.create_session();
        let second_id = state.active_session_id().to_string();
        assert_ne!(first_id, second_id);
        assert_eq!(
            state.active_session_trust_mode(),
            tiangong_core::permission::TrustMode::FullTrust
        );

        state.switch_session(&first_id);
        assert_eq!(
            state.active_session_trust_mode(),
            tiangong_core::permission::TrustMode::Supervised
        );
        state.switch_session(&second_id);
        assert_eq!(
            state.active_session_trust_mode(),
            tiangong_core::permission::TrustMode::FullTrust
        );
        Ok(())
    })
}

#[test]
fn reasoning_effort_is_session_scoped_when_present() -> Result<()> {
    with_isolated_state("tiangong-state-session-reasoning-effort", |paths, state| {
        state.agent_config.reasoning_effort = "medium".to_string();
        state.create_session();
        let first_id = state.active_session_id().to_string();

        state.set_reasoning_effort("high".to_string())?;
        assert_eq!(state.active_session_reasoning_effort(), "high");
        assert_eq!(
            state
                .sessions()
                .iter()
                .find(|session| session.id == first_id)
                .and_then(|session| session.reasoning_effort.as_deref()),
            Some("high")
        );

        let session_path = paths
            .fake_home
            .join(".tiangong")
            .join("sessions")
            .join(format!("{first_id}.json"));
        let persisted: Session = serde_json::from_str(&fs::read_to_string(session_path)?)?;
        assert_eq!(persisted.reasoning_effort.as_deref(), Some("high"));

        state.agent_config.reasoning_effort = "low".to_string();
        state.create_session();
        let second_id = state.active_session_id().to_string();
        assert_ne!(first_id, second_id);
        assert_eq!(state.active_session_reasoning_effort(), "low");

        state.switch_session(&first_id);
        assert_eq!(state.active_session_reasoning_effort(), "high");
        Ok(())
    })
}

#[test]
fn draft_session_creation_does_not_change_active_session() -> Result<()> {
    with_isolated_state(
        "tiangong-state-background-session-create",
        |_paths, state| {
            let active_id = state.active_session_id().to_string();
            let cwd = state.workspace_dir().to_string();
            let created = state.create_session_without_activation(
                cwd.clone(),
                tiangong_core::permission::TrustMode::FullTrust,
                "high".to_string(),
            )?;

            assert_eq!(state.active_session_id(), active_id);
            assert_ne!(created.id, active_id);
            assert_eq!(created.cwd, cwd);
            assert_eq!(
                created.trust_mode,
                tiangong_core::permission::TrustMode::FullTrust
            );
            assert_eq!(created.reasoning_effort.as_deref(), Some("high"));
            assert!(
                state
                    .sessions()
                    .iter()
                    .any(|session| session.id == created.id)
            );
            Ok(())
        },
    )
}

/// 验证 session 字段写入路径会同步刷新 SessionMetadata（issue #245 P2-A）。
///
/// `build_core_config_for_session_from_base` 已改为读 metadata，所以 metadata 必须
/// 与 sessions 保持一致——本测试覆盖 trust_mode / reasoning_effort / cwd / title。
#[test]
fn session_metadata_stays_in_sync_with_session_writes() -> Result<()> {
    use tiangong_core::session::SessionCwdMode;
    with_isolated_state("tiangong-state-metadata-sync", |_paths, state| {
        state.agent_config.default_trust_mode = tiangong_core::permission::TrustMode::Supervised;
        state.create_session();
        let id = state.active_session_id().to_string();

        // trust_mode 写入应反映到 metadata。
        state.set_session_trust_mode_in_memory(
            &id,
            tiangong_core::permission::TrustMode::FullTrust,
        )?;
        let meta = state
            .session_metadata()
            .iter()
            .find(|m| m.id == id)
            .expect("metadata 缺失");
        assert_eq!(
            meta.trust_mode,
            tiangong_core::permission::TrustMode::FullTrust
        );

        // reasoning_effort 写入应反映到 metadata。
        state.set_session_reasoning_effort_in_memory(&id, "high".to_string())?;
        let meta = state
            .session_metadata()
            .iter()
            .find(|m| m.id == id)
            .expect("metadata 缺失");
        assert_eq!(meta.reasoning_effort.as_deref(), Some("high"));

        // 标题写入应反映到 metadata。
        state.update_session_title_draft("新标题".to_string());
        state.apply_active_session_title_in_memory()?;
        let meta = state
            .session_metadata()
            .iter()
            .find(|m| m.id == id)
            .expect("metadata 缺失");
        assert_eq!(meta.title, "新标题");

        // build_core_config_for_session_from_base 应读出 metadata 的 trust_mode。
        let base = tiangong_core::core_config::CoreConfig::default();
        let config = state.build_core_config_for_session_from_base(&base, &id);
        assert_eq!(
            config.trust_mode,
            tiangong_core::permission::TrustMode::FullTrust
        );
        assert_eq!(config.reasoning_effort, "high");

        // cwd 写入应反映到 metadata。
        state.update_session_cwd(&id, "/custom/cwd".to_string())?;
        let meta = state
            .session_metadata()
            .iter()
            .find(|m| m.id == id)
            .expect("metadata 缺失");
        assert_eq!(meta.cwd, "/custom/cwd");
        assert_eq!(meta.cwd_mode, SessionCwdMode::Custom);
        Ok(())
    })
}
