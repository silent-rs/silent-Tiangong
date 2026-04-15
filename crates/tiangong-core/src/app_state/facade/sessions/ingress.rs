use super::super::super::*;

impl TiangongState {
    /// 向指定会话注入外部事件消息，并立即持久化。
    pub fn append_session_message(
        &mut self,
        session_id: &str,
        role: MessageRole,
        content: impl Into<String>,
    ) -> Result<()> {
        let content = content.into();
        let Some(session) = self
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        else {
            return Err(anyhow!("目标会话不存在：{session_id}"));
        };

        session.append_message(role, content);
        self.persist_session_and_app(session_id)
    }
}
