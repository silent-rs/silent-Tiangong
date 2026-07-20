use anyhow::{anyhow, Result};
use tiangong_app_state::app_state::{CoreManager, TiangongState};
use tiangong_core::session::{now_text, MessageRole, Session, SessionCwdMode};
use tiangong_types::{ContentBlock, TrustMode};

pub(crate) fn remove_session_state(
    state: &mut TiangongState,
    manager: &CoreManager,
    session_id: &str,
) {
    state.input_caches.remove(session_id);
    state.pending_turns.remove(session_id);
    if state.active_session_id == session_id {
        state.active_session_id = manager
            .list_session_metadata()
            .into_iter()
            .find(|metadata| metadata.id != session_id)
            .map(|metadata| metadata.id)
            .unwrap_or_default();
    }
}

pub(crate) fn remove_failed_message(
    manager: &CoreManager,
    session_id: &str,
    message_id: &str,
) -> Result<()> {
    let mut session = manager
        .load_session(session_id)
        .map_err(|error| anyhow!("加载会话失败：{error}"))?;
    if let Some(index) = session
        .messages
        .iter()
        .position(|message| message.id == message_id)
    {
        session.messages.remove(index);
        session.summary_up_to = session.summary_up_to.min(session.messages.len());
        session.updated_at = now_text();
        session
            .try_persist_to_disk()
            .map_err(|error| anyhow!("回滚消息失败：{error}"))?;
    }
    Ok(())
}

pub(crate) fn edit_prepared_user_message(
    state: &mut TiangongState,
    manager: &CoreManager,
    session_id: &str,
    message_id: &str,
    prepared: Vec<ContentBlock>,
) -> Result<(Session, Session)> {
    if crate::state_ops::has_pending_turn(state, session_id) {
        return Err(anyhow!("目标会话正在执行，暂时不能编辑重发"));
    }
    let mut session = manager
        .load_session(session_id)
        .map_err(|error| anyhow!("加载会话失败：{error}"))?;
    let original = session.clone();
    if !session.update_prepared_user_message(message_id, prepared) {
        return Err(anyhow!("消息不存在：{message_id}"));
    }
    session.truncate_after_message(message_id);
    session.updated_at = now_text();
    session
        .try_persist_to_disk()
        .map_err(|error| anyhow!("编辑持久化失败：{error}"))?;
    crate::state_ops::mark_pending_message(state, session_id, message_id);
    if session.cwd.trim().is_empty() {
        session.cwd = state.workspace_dir.clone();
    }
    Ok((original, session))
}

pub(crate) fn restore_session(snapshot: Session) -> Result<()> {
    snapshot
        .try_persist_to_disk()
        .map_err(|error| anyhow!("恢复会话失败：{error}"))
}

pub(crate) fn update_title(
    manager: &CoreManager,
    session_id: &str,
    title: String,
) -> Result<String> {
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err(anyhow!("会话标题不能为空"));
    }
    let mut session = manager
        .load_session(session_id)
        .map_err(|error| anyhow!("加载会话失败：{error}"))?;
    let previous = std::mem::replace(&mut session.title, title);
    session.updated_at = now_text();
    session
        .try_persist_to_disk()
        .map_err(|error| anyhow!("保存会话标题失败：{error}"))?;
    Ok(previous)
}

pub(crate) fn update_trust_mode(
    manager: &CoreManager,
    session_id: &str,
    trust_mode: TrustMode,
) -> Result<TrustMode> {
    let mut session = manager
        .load_session(session_id)
        .map_err(|error| anyhow!("加载会话失败：{error}"))?;
    let previous = session.trust_mode;
    session.trust_mode = trust_mode;
    session.updated_at = now_text();
    session
        .try_persist_to_disk()
        .map_err(|error| anyhow!("保存会话信任模式失败：{error}"))?;
    Ok(previous)
}

pub(crate) fn update_reasoning_effort(
    manager: &CoreManager,
    session_id: &str,
    reasoning_effort: Option<String>,
) -> Result<Option<String>> {
    let mut session = manager
        .load_session(session_id)
        .map_err(|error| anyhow!("加载会话失败：{error}"))?;
    let previous = std::mem::replace(&mut session.reasoning_effort, reasoning_effort);
    session.updated_at = now_text();
    session
        .try_persist_to_disk()
        .map_err(|error| anyhow!("保存会话思考强度失败：{error}"))?;
    Ok(previous)
}

pub(crate) fn update_session_cwd(
    manager: &CoreManager,
    session_id: &str,
    cwd: String,
) -> Result<()> {
    let mut session = manager
        .load_session(session_id)
        .map_err(|error| anyhow!("加载会话失败：{error}"))?;
    if session.cwd_mode == SessionCwdMode::Isolated {
        return Err(anyhow!("隔离模式会话不允许修改工作目录"));
    }
    if session.has_user_messages() {
        return Err(anyhow!("已有对话的会话不允许切换工作目录"));
    }
    session.cwd = cwd;
    session.cwd_mode = SessionCwdMode::Custom;
    session.updated_at = now_text();
    session
        .try_persist_to_disk()
        .map_err(|error| anyhow!("保存会话工作目录失败：{error}"))
}

pub(crate) fn update_workspace_dir(
    state: &mut TiangongState,
    manager: &CoreManager,
    workspace_dir: String,
) -> Result<()> {
    state.workspace_dir = workspace_dir.clone();
    for metadata in manager.list_session_metadata() {
        if metadata.cwd_mode != SessionCwdMode::Inherit {
            continue;
        }
        let Ok(mut session) = manager.load_session(&metadata.id) else {
            continue;
        };
        if session.has_user_messages() {
            continue;
        }
        session.cwd = workspace_dir.clone();
        session.updated_at = now_text();
        session
            .try_persist_to_disk()
            .map_err(|error| anyhow!("保存会话工作目录失败：{error}"))?;
    }
    Ok(())
}

pub(crate) fn title_generation_input(manager: &CoreManager, session_id: &str) -> Option<String> {
    let session = manager.load_session(session_id).ok()?;
    if session.title != "新对话" && !session.title.starts_with("会话 ") {
        return None;
    }
    session
        .messages
        .iter()
        .find(|message| message.role == MessageRole::User)
        .map(|message| message.text_content())
}
