use super::super::super::*;

impl TiangongState {
    /// 为本地用户消息准备统一入口上下文。
    ///
    /// 统一处理：
    /// - 解析活动会话
    /// - 固定用户消息并立即持久化
    /// - 清空输入草稿
    /// - 更新运行状态
    /// - 返回用于创建/复用 TiangongCore 的会话快照
    pub fn prepare_active_user_message_ingress(
        &mut self,
        content: impl Into<String>,
    ) -> Result<(String, String, Session)> {
        self.prepare_active_user_message_ingress_with_media(content, Vec::new())
    }

    pub fn prepare_active_user_message_ingress_with_media(
        &mut self,
        content: impl Into<String>,
        media: Vec<tiangong_types::MediaAsset>,
    ) -> Result<(String, String, Session)> {
        // 调用方负责在持锁前完成附件归档（远程下载可能阻塞 60s，不能在状态锁内执行）。
        let content = content.into();
        let idx = self.ensure_active_session_index();
        let message_id = scru128::new().to_string();
        let session_id = self.store.session.sessions[idx].id.clone();
        self.store.session.sessions[idx].append_message_with_id_and_media(
            message_id.clone(),
            MessageRole::User,
            content,
            String::new(),
            media,
        );
        self.store.session.sessions[idx].updated_at = now_text();
        self.persist_session_and_app(&session_id)?;
        let session = self.store.session.sessions[idx].clone();
        self.store.session.input_draft.clear();
        self.store.runtime.run.status = tiangong_core::runtime::RunStatus::Executing;
        self.store.runtime.run.summary = "正在处理".to_string();
        self.store.runtime.run.last_session_id = Some(session.id.clone());
        let usage = session.total_usage();
        self.store.runtime.run.last_usage = (usage.total_tokens > 0).then_some(usage);
        self.store.runtime.run.updated_at = now_text();
        self.mark_pending_turn_for(session.id.clone());
        let mut runtime_session = session.clone();
        if runtime_session.cwd.trim().is_empty() {
            runtime_session.cwd = self.store.session.workspace_dir.clone();
        }
        Ok((session.id.clone(), message_id, runtime_session))
    }

    /// 为指定会话（而非全局活动会话）准备用户消息入口。
    ///
    /// 调用方先固定 session_id 再归档附件，避免归档期间活动会话切换导致串线。
    /// 若目标会话不存在则明确失败，不回退到活动会话。
    pub fn prepare_user_message_ingress_for_session(
        &mut self,
        session_id: &str,
        content: impl Into<String>,
        media: Vec<tiangong_types::MediaAsset>,
    ) -> Result<(String, String, Session)> {
        let content = content.into();
        let idx = self
            .store
            .session
            .sessions
            .iter()
            .position(|s| s.id == session_id)
            .ok_or_else(|| anyhow!("目标会话不存在：{session_id}"))?;
        let message_id = scru128::new().to_string();
        self.store.session.sessions[idx].append_message_with_id_and_media(
            message_id.clone(),
            MessageRole::User,
            content,
            String::new(),
            media,
        );
        self.store.session.sessions[idx].updated_at = now_text();
        self.persist_session_and_app(session_id)?;
        let session = self.store.session.sessions[idx].clone();
        self.store.session.input_draft.clear();
        self.store.runtime.run.status = tiangong_core::runtime::RunStatus::Executing;
        self.store.runtime.run.summary = "正在处理".to_string();
        self.store.runtime.run.last_session_id = Some(session.id.clone());
        let usage = session.total_usage();
        self.store.runtime.run.last_usage = (usage.total_tokens > 0).then_some(usage);
        self.store.runtime.run.updated_at = now_text();
        self.mark_pending_turn_for(session.id.clone());
        let mut runtime_session = session.clone();
        if runtime_session.cwd.trim().is_empty() {
            runtime_session.cwd = self.store.session.workspace_dir.clone();
        }
        Ok((session.id.clone(), message_id, runtime_session))
    }

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
        session.updated_at = now_text();
        self.persist_session_and_app(session_id)
    }
}
