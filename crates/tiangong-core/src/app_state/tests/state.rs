use anyhow::Result;
use std::fs;

use super::super::repository::canonical_scru128_id;
use super::super::*;
use super::common::with_isolated_state;

#[test]
fn normalize_sessions_repairs_invalid_ids_and_active_session() -> Result<()> {
    with_isolated_state("tiangong-state-normalize", |_paths, state| {
        state.store.session.sessions = vec![Session::new("A"), Session::new("B")];
        state.store.session.sessions[0].id = "invalid-id".to_string();
        state.store.session.sessions[1].id = "invalid-id".to_string();
        state.store.session.active_session_id = "invalid-active-id".to_string();

        state.normalize_sessions_for_storage();

        assert_eq!(state.store.session.sessions.len(), 2);
        let first_id = state.store.session.sessions[0].id.clone();
        let second_id = state.store.session.sessions[1].id.clone();
        assert_eq!(canonical_scru128_id(&first_id), Some(first_id.clone()));
        assert_eq!(canonical_scru128_id(&second_id), Some(second_id.clone()));
        assert_ne!(first_id, second_id);
        assert_eq!(state.store.session.active_session_id, first_id);
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
        state.store.agent.agent_config.default_trust_mode = crate::permission::TrustMode::FullTrust;
        state.create_session();
        let first_id = state.active_session_id().to_string();
        assert_eq!(
            state.active_session_trust_mode(),
            crate::permission::TrustMode::FullTrust
        );

        state.set_trust_mode(crate::permission::TrustMode::Supervised)?;
        assert_eq!(
            state
                .sessions()
                .iter()
                .find(|session| session.id == first_id)
                .map(|session| session.trust_mode),
            Some(crate::permission::TrustMode::Supervised)
        );

        state.store.agent.agent_config.default_trust_mode = crate::permission::TrustMode::FullTrust;
        state.create_session();
        let second_id = state.active_session_id().to_string();
        assert_ne!(first_id, second_id);
        assert_eq!(
            state.active_session_trust_mode(),
            crate::permission::TrustMode::FullTrust
        );

        state.switch_session(&first_id);
        assert_eq!(
            state.active_session_trust_mode(),
            crate::permission::TrustMode::Supervised
        );
        state.switch_session(&second_id);
        assert_eq!(
            state.active_session_trust_mode(),
            crate::permission::TrustMode::FullTrust
        );
        Ok(())
    })
}

#[test]
fn reasoning_effort_is_session_scoped_when_present() -> Result<()> {
    with_isolated_state("tiangong-state-session-reasoning-effort", |paths, state| {
        state.store.agent.agent_config.reasoning_effort = "medium".to_string();
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

        state.store.agent.agent_config.reasoning_effort = "low".to_string();
        state.create_session();
        let second_id = state.active_session_id().to_string();
        assert_ne!(first_id, second_id);
        assert_eq!(state.active_session_reasoning_effort(), "low");

        state.switch_session(&first_id);
        assert_eq!(state.active_session_reasoning_effort(), "high");
        Ok(())
    })
}
