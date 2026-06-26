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

    /// 删除指定 workspace（cwd）下的所有会话。
    ///
    /// 返回被删除的会话 id 列表（供调用方销毁交互 PTY）。删除后若会话列表
    /// 清空，会自动新建一个默认会话；若删除涉及当前活跃会话，会把它切换
    /// 到剩余列表中的第一个。
    pub fn delete_sessions_by_cwd(&mut self, cwd: &str) -> Result<Vec<String>> {
        // 收集待删除的会话 id（cwd 完全匹配）
        let deleted_ids: Vec<String> = self
            .store
            .session
            .sessions
            .iter()
            .filter(|s| s.cwd == cwd)
            .map(|s| s.id.clone())
            .collect();

        if deleted_ids.is_empty() {
            return Ok(deleted_ids);
        }

        let active_was_deleted = deleted_ids.contains(&self.store.session.active_session_id);

        // 从列表移除匹配会话并删除会话文件
        self.store.session.sessions.retain(|s| s.cwd != cwd);
        for id in &deleted_ids {
            let _ = self.remove_session_file(id);
        }

        if self.store.session.sessions.is_empty() {
            // 全部删空：新建一个默认会话作为活跃会话
            let mut session = Session::new(DEFAULT_SESSION_TITLE);
            session.cwd = self.store.session.workspace_dir.clone();
            session.trust_mode = self.store.agent.agent_config.default_trust_mode;
            self.store.session.active_session_id = session.id.clone();
            self.store.session.session_title_draft = session.title.clone();
            self.store.session.sessions.push(session);
            let current_id = self.store.session.sessions[0].id.clone();
            self.persist_session(&current_id)?;
        } else if active_was_deleted {
            // 活跃会话被删除：切到剩余列表的第一个
            self.store.session.active_session_id = self.store.session.sessions[0].id.clone();
            self.store.session.session_title_draft = self.store.session.sessions[0].title.clone();
        }

        // 同步活跃会话的 trust_mode 到运行时
        if let Some(trust_mode) = self.active_session().map(|session| session.trust_mode) {
            self.store.agent.agent_config.trust_mode = trust_mode;
            self.services
                .runtime
                .permission_gate()
                .set_trust_mode(trust_mode);
        }

        self.persist_app_only()?;
        Ok(deleted_ids)
    }
}
