use std::sync::mpsc;
use std::time::Instant;

use anyhow::Result;

use super::super::repository::{canonical_scru128_id, new_scru128_string};
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
fn ensure_pending_turn_assistant_message_creates_and_binds_message() -> Result<()> {
    with_isolated_state("tiangong-turn-assistant-message", |_paths, state| {
        let session_id = state.store.session.active_session_id.clone();
        let task_id = new_scru128_string();

        let session = state
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            .expect("默认会话应存在");
        session.append_message(MessageRole::User, "测试输入");
        let user_message_id = session
            .messages
            .last()
            .map(|message| message.id.clone())
            .expect("用户消息应创建成功");
        session.start_task(
            task_id.clone(),
            user_message_id,
            String::new(),
            "测试输入".to_string(),
        );

        let (_tx, rx) = mpsc::channel();
        state.store.runtime.pending_turn = Some(PendingTurn {
            session_id: session_id.clone(),
            task_id: task_id.clone(),
            assistant_message_id: None,
            started_at: Instant::now(),
            rx,
        });

        let service = state.services.turn_service;
        let (bound_session_id, assistant_message_id) = service
            .ensure_pending_turn_assistant_message(state)
            .expect("应创建 assistant message");

        assert_eq!(bound_session_id, session_id);
        assert_eq!(
            state
                .store
                .runtime
                .pending_turn
                .as_ref()
                .and_then(|pending| pending.assistant_message_id.as_ref())
                .map(String::as_str),
            Some(assistant_message_id.as_str())
        );

        let session = state
            .store
            .session
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("会话应仍存在");
        let assistant_message = session
            .messages
            .iter()
            .find(|message| message.id == assistant_message_id)
            .expect("应能找到 assistant message");
        assert_eq!(assistant_message.role, MessageRole::Assistant);
        assert!(assistant_message.content.is_empty());
        assert!(assistant_message.reasoning_content.is_empty());
        assert_eq!(
            session
                .task_records
                .last()
                .map(|record| record.assistant_message_id.as_str()),
            Some(assistant_message_id.as_str())
        );
        Ok(())
    })
}
