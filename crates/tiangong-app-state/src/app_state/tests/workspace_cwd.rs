use anyhow::Result;

use super::super::*;
use super::common::with_isolated_state;
use tiangong_core::session::{MessageRole, SessionCwdMode};

#[test]
fn has_user_messages_returns_false_for_empty_session() -> Result<()> {
    with_isolated_state("tiangong-cwd-has-user-msg-empty", |_paths, state| {
        state.create_session();
        let session = state.active_session().unwrap();
        assert!(!session.has_user_messages());
        Ok(())
    })
}

#[test]
fn has_user_messages_returns_true_after_user_message() -> Result<()> {
    with_isolated_state("tiangong-cwd-has-user-msg-true", |_paths, state| {
        state.create_session();
        state.prepare_active_user_message_ingress("hello")?;
        let session = state.active_session().unwrap();
        assert!(session.has_user_messages());
        Ok(())
    })
}

#[test]
fn has_user_messages_returns_false_with_only_assistant_messages() -> Result<()> {
    with_isolated_state("tiangong-cwd-has-user-msg-assistant", |_paths, state| {
        state.create_session();
        let session_id = state.active_session_id().to_string();
        state
            .sessions_mut()
            .iter_mut()
            .find(|s| s.id == session_id)
            .unwrap()
            .append_message(MessageRole::Assistant, "hi there");
        let session = state.active_session().unwrap();
        assert!(!session.has_user_messages());
        Ok(())
    })
}

#[test]
fn update_workspace_dir_syncs_inherit_sessions_without_messages() -> Result<()> {
    with_isolated_state("tiangong-cwd-workspace-sync-inherit", |paths, state| {
        state.create_session();
        let session_id = state.active_session_id().to_string();
        let original_cwd = state.active_session().unwrap().cwd.clone();

        let new_dir = paths.workspace.join("new-project");
        std::fs::create_dir_all(&new_dir)?;
        state.update_workspace_dir(new_dir.to_string_lossy().to_string())?;

        let session = state
            .sessions()
            .iter()
            .find(|s| s.id == session_id)
            .unwrap();
        assert_eq!(session.cwd, new_dir.to_string_lossy().to_string());
        assert_ne!(session.cwd, original_cwd);
        assert_eq!(session.cwd_mode, SessionCwdMode::Inherit);
        Ok(())
    })
}

#[test]
fn update_workspace_dir_skips_inherit_sessions_with_messages() -> Result<()> {
    with_isolated_state(
        "tiangong-cwd-workspace-skip-has-messages",
        |paths, state| {
            state.create_session();
            let session_id = state.active_session_id().to_string();
            state.prepare_active_user_message_ingress("hello")?;

            let original_cwd = state
                .sessions()
                .iter()
                .find(|s| s.id == session_id)
                .unwrap()
                .cwd
                .clone();

            let new_dir = paths.workspace.join("new-project");
            std::fs::create_dir_all(&new_dir)?;
            state.update_workspace_dir(new_dir.to_string_lossy().to_string())?;

            let session = state
                .sessions()
                .iter()
                .find(|s| s.id == session_id)
                .unwrap();
            assert_eq!(session.cwd, original_cwd, "已有对话的会话 cwd 不应被更新");
            assert_eq!(session.cwd_mode, SessionCwdMode::Inherit);
            Ok(())
        },
    )
}

#[test]
fn update_workspace_dir_skips_custom_mode_sessions() -> Result<()> {
    with_isolated_state("tiangong-cwd-workspace-skip-custom", |paths, state| {
        state.create_session();
        let session_id = state.active_session_id().to_string();

        let custom_dir = paths.workspace.join("custom-dir");
        std::fs::create_dir_all(&custom_dir)?;
        let s = state
            .sessions_mut()
            .iter_mut()
            .find(|s| s.id == session_id)
            .unwrap();
        s.cwd = custom_dir.to_string_lossy().to_string();
        s.cwd_mode = SessionCwdMode::Custom;

        let new_dir = paths.workspace.join("new-project");
        std::fs::create_dir_all(&new_dir)?;
        state.update_workspace_dir(new_dir.to_string_lossy().to_string())?;

        let session = state
            .sessions()
            .iter()
            .find(|s| s.id == session_id)
            .unwrap();
        assert_eq!(
            session.cwd,
            custom_dir.to_string_lossy().to_string(),
            "Custom 模式会话 cwd 不应被更新"
        );
        assert_eq!(session.cwd_mode, SessionCwdMode::Custom);
        Ok(())
    })
}

#[test]
fn update_active_session_cwd_rejects_sessions_with_messages() -> Result<()> {
    with_isolated_state(
        "tiangong-cwd-session-reject-has-messages",
        |paths, state| {
            state.create_session();
            state.prepare_active_user_message_ingress("hello")?;

            let new_dir = paths.workspace.join("new-project");
            std::fs::create_dir_all(&new_dir)?;
            let result = state.update_active_session_cwd(new_dir.to_string_lossy().to_string());

            assert!(result.is_err(), "已有对话的会话应拒绝切换 cwd");
            assert!(result.unwrap_err().to_string().contains("已有对话"));
            Ok(())
        },
    )
}

#[test]
fn update_active_session_cwd_allows_empty_sessions() -> Result<()> {
    with_isolated_state("tiangong-cwd-session-allow-empty", |paths, state| {
        state.create_session();
        let session_id = state.active_session_id().to_string();

        let new_dir = paths.workspace.join("new-project");
        std::fs::create_dir_all(&new_dir)?;
        state.update_active_session_cwd(new_dir.to_string_lossy().to_string())?;

        let session = state
            .sessions()
            .iter()
            .find(|s| s.id == session_id)
            .unwrap();
        assert_eq!(session.cwd, new_dir.to_string_lossy().to_string());
        assert_eq!(session.cwd_mode, SessionCwdMode::Custom);
        Ok(())
    })
}

#[test]
fn update_active_session_cwd_rejects_isolated_sessions() -> Result<()> {
    with_isolated_state("tiangong-cwd-session-reject-isolated", |paths, state| {
        state.create_session();
        let session_id = state.active_session_id().to_string();
        let s = state
            .sessions_mut()
            .iter_mut()
            .find(|s| s.id == session_id)
            .unwrap();
        s.cwd_mode = SessionCwdMode::Isolated;

        let new_dir = paths.workspace.join("new-project");
        std::fs::create_dir_all(&new_dir)?;
        let result = state.update_active_session_cwd(new_dir.to_string_lossy().to_string());

        assert!(result.is_err(), "Isolated 模式会话应拒绝修改 cwd");
        assert!(result.unwrap_err().to_string().contains("隔离模式"));
        Ok(())
    })
}

#[test]
fn update_workspace_dir_mixed_sessions() -> Result<()> {
    with_isolated_state("tiangong-cwd-workspace-mixed", |paths, state| {
        // 会话 A：Inherit 模式，无对话 → 应被更新
        state.create_session();
        let id_a = state.active_session_id().to_string();

        // 会话 B：Inherit 模式，有对话 → 不应被更新
        state.create_session();
        let id_b = state.active_session_id().to_string();
        state.prepare_active_user_message_ingress("hello")?;

        // 会话 C：Custom 模式，无对话 → 不应被更新
        state.create_session();
        let id_c = state.active_session_id().to_string();
        let custom_dir = paths.workspace.join("custom");
        std::fs::create_dir_all(&custom_dir)?;
        let s = state
            .sessions_mut()
            .iter_mut()
            .find(|s| s.id == id_c)
            .unwrap();
        s.cwd = custom_dir.to_string_lossy().to_string();
        s.cwd_mode = SessionCwdMode::Custom;

        let new_dir = paths.workspace.join("new-workspace");
        std::fs::create_dir_all(&new_dir)?;
        state.update_workspace_dir(new_dir.to_string_lossy().to_string())?;

        let get_cwd = |id: &str, st: &TiangongState| -> String {
            st.sessions()
                .iter()
                .find(|s| s.id == id)
                .unwrap()
                .cwd
                .clone()
        };

        assert_eq!(
            get_cwd(&id_a, state),
            new_dir.to_string_lossy().to_string(),
            "Inherit 无对话会话应被更新"
        );
        assert_ne!(
            get_cwd(&id_b, state),
            new_dir.to_string_lossy().to_string(),
            "Inherit 有对话会话不应被更新"
        );
        assert_eq!(
            get_cwd(&id_c, state),
            custom_dir.to_string_lossy().to_string(),
            "Custom 会话不应被更新"
        );
        Ok(())
    })
}
