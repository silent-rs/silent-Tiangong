use super::super::super::*;

impl TiangongState {
    /// 保存 TiangongCore 退出时返回的 session
    ///
    /// 将 Core 的最终 session 合并到 TiangongState 并持久化。
    /// 如果该 session 已存在则替换，否则插入。
    pub fn save_core_session(&mut self, session: Session) {
        let session_id = session.id.clone();
        if let Some(existing) = self
            .store
            .session
            .sessions
            .iter_mut()
            .find(|s| s.id == session_id)
        {
            let existing_cwd = existing.cwd.clone();
            let existing_cwd_mode = existing.cwd_mode.clone();
            let existing_trust_mode = existing.trust_mode;
            *existing = session;
            if existing_cwd_mode == crate::session::SessionCwdMode::Inherit {
                existing.cwd = existing_cwd;
                existing.cwd_mode = existing_cwd_mode;
            }
            existing.trust_mode = existing_trust_mode;
        } else {
            self.store.session.sessions.insert(0, session);
            self.store.session.active_session_id = session_id.clone();
        }
        let _ = self.persist_session_and_app(&session_id);
    }

    pub fn create_session(&mut self) {
        let mut session = Session::new("新对话");
        session.cwd = self.store.session.workspace_dir.clone();
        session.trust_mode = self.store.agent.agent_config.default_trust_mode;
        self.store.agent.agent_config.trust_mode = session.trust_mode;
        self.services
            .runtime
            .permission_gate()
            .set_trust_mode(session.trust_mode);
        self.store.session.active_session_id = session.id.clone();
        self.store.session.session_title_draft = session.title.clone();
        self.store.session.sessions.push(session);
        // 仅更新 app.json 中的 active_session_id，不持久化空会话文件
        // 会话文件将在用户发送第一条消息时自动持久化
        let _ = self.persist_app_only();
    }

    pub fn switch_session(&mut self, session_id: &str) {
        if let Some(session) = self
            .store
            .session
            .sessions
            .iter()
            .find(|session| session.id == session_id)
        {
            self.store.session.active_session_id = session_id.to_string();
            self.store.session.session_title_draft = session.title.clone();
            self.store.agent.agent_config.trust_mode = session.trust_mode;
            self.services
                .runtime
                .permission_gate()
                .set_trust_mode(session.trust_mode);
            let _ = self.persist_app_only();
            let _ = self.try_auto_resume_unfinished_plan_for_active_session();
        }
    }

    pub fn save_active_session_title(&mut self) -> Result<()> {
        let new_title = self.store.session.session_title_draft.trim();
        if new_title.is_empty() {
            return Err(anyhow!("会话标题不能为空"));
        }

        let active_id = self.store.session.active_session_id.clone();
        let Some(session) = self
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == active_id)
        else {
            return Err(anyhow!("当前会话不存在，无法重命名"));
        };

        session.title = new_title.to_string();
        self.store.session.session_title_draft = session.title.clone();
        self.persist_session_and_app(&active_id)
    }

    pub fn delete_active_session(&mut self) -> Result<()> {
        let active_id = self.store.session.active_session_id.clone();
        let Some(remove_idx) = self
            .store
            .session
            .sessions
            .iter()
            .position(|session| session.id == active_id)
        else {
            return Err(anyhow!("当前会话不存在，无法删除"));
        };

        self.store.session.sessions.remove(remove_idx);

        if self.store.session.sessions.is_empty() {
            let mut session = Session::new(DEFAULT_SESSION_TITLE);
            session.cwd = self.store.session.workspace_dir.clone();
            session.trust_mode = self.store.agent.agent_config.default_trust_mode;
            self.store.session.active_session_id = session.id.clone();
            self.store.session.session_title_draft = session.title.clone();
            self.store.session.sessions.push(session);
        } else {
            let next_idx = if remove_idx >= self.store.session.sessions.len() {
                self.store.session.sessions.len() - 1
            } else {
                remove_idx
            };
            self.store.session.active_session_id = self.store.session.sessions[next_idx].id.clone();
            self.store.session.session_title_draft =
                self.store.session.sessions[next_idx].title.clone();
        }
        if let Some(trust_mode) = self.active_session().map(|session| session.trust_mode) {
            self.store.agent.agent_config.trust_mode = trust_mode;
            self.services
                .runtime
                .permission_gate()
                .set_trust_mode(trust_mode);
        }

        self.remove_session_file(&active_id)?;
        if self.store.session.sessions.len() == 1
            && self.store.session.sessions[0].messages.is_empty()
        {
            let current_id = self.store.session.sessions[0].id.clone();
            self.persist_session(&current_id)?;
        }
        self.persist_app_only()
    }
}
