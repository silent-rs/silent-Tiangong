use anyhow::Result;

use super::super::SessionInputDraft;
use super::common::with_isolated_state;

#[test]
fn drafts_are_isolated_and_stale_updates_are_ignored() -> Result<()> {
    with_isolated_state("tiangong-session-draft-isolation", |_paths, state| {
        let first_id = state.active_session_id().to_string();
        state.create_session();
        let second_id = state.active_session_id().to_string();

        state.set_session_input_draft(
            &first_id,
            SessionInputDraft {
                text: "A 草稿".to_string(),
                revision: 2,
                ..SessionInputDraft::default()
            },
        )?;
        state.set_session_input_draft(
            &second_id,
            SessionInputDraft {
                text: "B 草稿".to_string(),
                revision: 3,
                ..SessionInputDraft::default()
            },
        )?;

        let (stale, applied) = state.set_session_input_draft_with_outcome(
            &first_id,
            SessionInputDraft {
                text: "迟到的旧草稿".to_string(),
                revision: 1,
                ..SessionInputDraft::default()
            },
        )?;

        assert!(!applied);
        assert_eq!(stale.text, "A 草稿");
        assert_eq!(state.session_input_draft(&first_id).text, "A 草稿");
        assert_eq!(state.session_input_draft(&second_id).text, "B 草稿");
        Ok(())
    })
}

#[test]
fn begin_accepts_older_sent_revision_and_finish_preserves_newer_draft() -> Result<()> {
    with_isolated_state("tiangong-session-draft-revision", |_paths, state| {
        let session_id = state.active_session_id().to_string();
        state.set_session_input_draft(
            &session_id,
            SessionInputDraft {
                text: "准备发送".to_string(),
                revision: 4,
                ..SessionInputDraft::default()
            },
        )?;
        state.set_session_input_draft(
            &session_id,
            SessionInputDraft {
                text: "等待期间的新输入".to_string(),
                revision: 5,
                ..SessionInputDraft::default()
            },
        )?;

        state.begin_session_send(&session_id, 4)?;
        assert!(state.session_input_draft(&session_id).is_sending);
        let after_old_success = state.finish_session_send(&session_id, 4, true)?;
        assert_eq!(after_old_success.text, "等待期间的新输入");
        assert_eq!(after_old_success.revision, 5);
        assert!(!after_old_success.is_sending);

        state.begin_session_send(&session_id, 5)?;
        let cleared = state.finish_session_send(&session_id, 5, true)?;
        assert!(cleared.text.is_empty());
        assert_eq!(cleared.revision, 6);
        Ok(())
    })
}

#[test]
fn draft_migration_keeps_text_and_revision() -> Result<()> {
    with_isolated_state("tiangong-session-draft-migrate", |_paths, state| {
        let temporary_id = scru128::new().to_string();
        state.set_session_input_draft(
            &temporary_id,
            SessionInputDraft {
                text: "临时会话内容".to_string(),
                revision: 7,
                ..SessionInputDraft::default()
            },
        )?;
        state.create_session();
        let real_id = state.active_session_id().to_string();

        let migrated = state.migrate_session_input_draft(&temporary_id, &real_id)?;
        assert_eq!(migrated.text, "临时会话内容");
        assert_eq!(migrated.revision, 7);
        assert!(state.session_input_draft(&temporary_id).text.is_empty());
        assert_eq!(state.session_input_draft(&real_id).text, "临时会话内容");
        Ok(())
    })
}

#[test]
fn sending_state_is_returned_at_runtime_but_persisted_as_idle() -> Result<()> {
    with_isolated_state("tiangong-session-draft-sending-persist", |_paths, state| {
        let session_id = state.active_session_id().to_string();
        state.set_session_input_draft(
            &session_id,
            SessionInputDraft {
                text: "发送中".to_string(),
                revision: 9,
                ..SessionInputDraft::default()
            },
        )?;
        state.begin_session_send(&session_id, 9)?;
        assert!(state.session_input_draft(&session_id).is_sending);

        let app_json = std::fs::read_to_string(&state.repository.paths().app_storage_path)?;
        let value: serde_json::Value = serde_json::from_str(&app_json)?;
        assert_eq!(
            value["input_drafts"][&session_id]["is_sending"],
            serde_json::Value::Bool(false)
        );
        Ok(())
    })
}
