use anyhow::Result;

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
