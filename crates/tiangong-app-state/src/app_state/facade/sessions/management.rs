use super::super::super::*;

impl TiangongState {
    pub fn create_session(&mut self) {
        let mut session = Session::new("新对话");
        session.cwd = self.store.session.workspace_dir.clone();
        session.trust_mode = self.store.agent.agent_config.default_trust_mode;
        Self::apply_derived_context_metrics(&mut session, self.services.runtime.context_limit);
        self.store.agent.agent_config.trust_mode = session.trust_mode;
        self.store.session.active_session_id = session.id.clone();
        self.store.session.session_title_draft = session.title.clone();
        self.store
            .session
            .input_drafts
            .entry(session.id.clone())
            .or_default();
        self.store.session.sessions.push(session);
        self.resync_session_metadata();
        // 仅更新 app.json 中的 active_session_id，不持久化空会话文件
        // 会话文件将在用户发送第一条消息时自动持久化
        let _ = self.persist_app_only();
    }

    /// 把一个已构造好的 Session 加入列表（issue #245：收敛调用方对
    /// `sessions_mut().push(...)` 的直接操纵，如 connector 创建隔离会话）。
    /// 不改变 active_session_id；持久化由调用方触发。
    pub fn add_session(&mut self, session: Session) {
        self.store.session.sessions.push(session);
        self.resync_session_metadata();
    }

    /// 为草稿转正创建会话，但不改变全局活动会话。
    pub fn create_session_without_activation(
        &mut self,
        cwd: String,
        trust_mode: tiangong_core::permission::TrustMode,
        reasoning_effort: String,
    ) -> Result<Session> {
        let mut session = Session::new("新对话");
        session.cwd = cwd;
        session.cwd_mode = if session.cwd == self.store.session.workspace_dir {
            tiangong_core::session::SessionCwdMode::Inherit
        } else {
            tiangong_core::session::SessionCwdMode::Custom
        };
        session.trust_mode = trust_mode;
        session.reasoning_effort = Some(reasoning_effort);
        Self::apply_derived_context_metrics(&mut session, self.services.runtime.context_limit);
        let session_id = session.id.clone();
        self.store.session.sessions.push(session.clone());
        self.store
            .session
            .input_drafts
            .entry(session_id.clone())
            .or_default();
        self.resync_session_metadata();
        if let Err(error) = self.persist_app_only() {
            self.store
                .session
                .sessions
                .retain(|candidate| candidate.id != session_id);
            self.store.session.input_drafts.remove(&session_id);
            return Err(error);
        }
        Ok(session)
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
            let _ = self.persist_app_only();
            let _ = self.try_auto_resume_unfinished_plan_for_active_session();
        }
        self.resync_session_metadata();
    }

    pub fn save_active_session_title(&mut self) -> Result<()> {
        let (active_id, _) = self.apply_active_session_title_in_memory()?;
        self.persist_session_and_app(&active_id)
    }

    /// 按显式会话 ID 设置标题（仅内存镜像，不写 Session 文件）。
    ///
    /// 收敛 `rollback_session_title_mirror` 等调用方对 `sessions_mut()` 的直接操纵
    /// （issue #245）。持久化由调用方按需 `persist_session_and_app` 触发，
    /// metadata 由 persist 路径的 normalize 自动同步。
    /// 同步刷新 updated_at（标题变化属于会话更新）。
    pub fn set_session_title_in_memory(&mut self, session_id: &str, title: String) {
        if let Some(session) = self
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.title = title;
            session.updated_at = tiangong_core::session::now_text();
        }
        self.resync_session_metadata();
    }

    /// 校验标题草稿并只更新宿主内存镜像，不写 Session 文件。
    ///
    /// Desktop 在 Core 存活时先调用本方法，再把标题交给 Core 的单写者命令持久化；
    /// 没有 Core 的调用方仍可继续使用 [`Self::save_active_session_title`]。
    pub fn apply_active_session_title_in_memory(&mut self) -> Result<(String, String)> {
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
        let title = session.title.clone();
        self.store.session.session_title_draft = title.clone();
        self.resync_session_metadata();
        Ok((active_id, title))
    }

    pub fn delete_active_session(&mut self) -> Result<()> {
        let active_id = self.store.session.active_session_id.clone();
        self.delete_session_by_id(&active_id)
    }

    /// 按显式 ID 删除会话。调用方可以在等待 Core 停止期间仍固定原目标，
    /// 不会因用户切换活动会话而删错对象。
    pub fn delete_session_by_id(&mut self, session_id: &str) -> Result<()> {
        let Some(remove_idx) = self
            .store
            .session
            .sessions
            .iter()
            .position(|session| session.id == session_id)
        else {
            return Err(anyhow!("会话不存在，无法删除：{session_id}"));
        };
        let active_was_deleted = self.store.session.active_session_id == session_id;

        self.store.session.sessions.remove(remove_idx);
        self.store.session.input_drafts.remove(session_id);

        if self.store.session.sessions.is_empty() {
            let mut session = Session::new(DEFAULT_SESSION_TITLE);
            session.cwd = self.store.session.workspace_dir.clone();
            session.trust_mode = self.store.agent.agent_config.default_trust_mode;
            Self::apply_derived_context_metrics(&mut session, self.services.runtime.context_limit);
            self.store.session.active_session_id = session.id.clone();
            self.store.session.session_title_draft = session.title.clone();
            self.store
                .session
                .input_drafts
                .entry(session.id.clone())
                .or_default();
            self.store.session.sessions.push(session);
        } else if active_was_deleted {
            let next_idx = if remove_idx >= self.store.session.sessions.len() {
                self.store.session.sessions.len() - 1
            } else {
                remove_idx
            };
            self.store.session.active_session_id = self.store.session.sessions[next_idx].id.clone();
            self.store.session.session_title_draft =
                self.store.session.sessions[next_idx].title.clone();
        } else if let Some(active_title) = self.active_session().map(|active| active.title.clone())
        {
            self.store.session.session_title_draft = active_title;
        }
        if let Some(trust_mode) = self.active_session().map(|session| session.trust_mode) {
            self.store.agent.agent_config.trust_mode = trust_mode;
        }

        self.remove_session_file(session_id)?;
        if self.store.session.sessions.len() == 1
            && self.store.session.sessions[0].messages.is_empty()
        {
            let current_id = self.store.session.sessions[0].id.clone();
            self.persist_session(&current_id)?;
        }
        self.resync_session_metadata();
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
            self.store.session.input_drafts.remove(id);
        }

        if self.store.session.sessions.is_empty() {
            // 全部删空：新建一个默认会话作为活跃会话
            let mut session = Session::new(DEFAULT_SESSION_TITLE);
            session.cwd = self.store.session.workspace_dir.clone();
            session.trust_mode = self.store.agent.agent_config.default_trust_mode;
            Self::apply_derived_context_metrics(&mut session, self.services.runtime.context_limit);
            self.store.session.active_session_id = session.id.clone();
            self.store.session.session_title_draft = session.title.clone();
            self.store
                .session
                .input_drafts
                .entry(session.id.clone())
                .or_default();
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
        }

        self.resync_session_metadata();
        self.persist_app_only()?;
        Ok(deleted_ids)
    }
}
