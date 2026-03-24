use super::super::super::*;

impl TiangongState {
    pub fn create_session(&mut self) {
        let session = Session::new("新对话");
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
        session.updated_at = now_text();
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
            let session = Session::new(DEFAULT_SESSION_TITLE);
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
