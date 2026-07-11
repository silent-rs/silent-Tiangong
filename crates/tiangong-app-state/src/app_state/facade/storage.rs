use super::super::repository::*;
use super::super::*;

impl TiangongState {
    pub fn persist_session_and_app(&mut self, session_id: &str) -> Result<()> {
        self.normalize_sessions_for_storage();
        self.services
            .repository
            .persist_session(&self.store, session_id)?;
        self.services.repository.persist_app_only(&self.store)
    }

    pub fn persist_session(&mut self, session_id: &str) -> Result<()> {
        self.normalize_sessions_for_storage();
        self.services
            .repository
            .persist_session(&self.store, session_id)
    }

    /// 从 Core 已写入的权威会话文件刷新内存镜像，不反向写盘。
    pub fn reload_session_from_disk(&mut self, session_id: &str) -> Result<bool> {
        let fallback_trust_mode = self
            .store
            .session
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .map(|session| session.trust_mode)
            .unwrap_or_default();
        let Some(mut loaded) = self
            .services
            .repository
            .load_session_from_disk(session_id, fallback_trust_mode)?
        else {
            return Ok(false);
        };
        if let Some(existing) = self
            .store
            .session
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            // 宿主可在执行期间修改这些会话元数据；Core 只拥有消息/上下文执行状态。
            // 终态重载时保留宿主值，再由调用方统一落盘，避免异步标题或 Tab 更新丢失。
            loaded.title = existing.title.clone();
            loaded.tabs = existing.tabs.clone();
            loaded.active_tab_id = existing.active_tab_id.clone();
            loaded.cwd = existing.cwd.clone();
            loaded.cwd_mode = existing.cwd_mode.clone();
            loaded.trust_mode = existing.trust_mode;
            loaded.reasoning_effort = existing.reasoning_effort.clone();
            loaded.updated_at = loaded.updated_at.max(existing.updated_at.clone());
            // Token 用量由宿主按流事件累计，Core 文件不维护这部分运行指标。
            loaded.token_usage = existing.token_usage.clone();
            loaded.agent_token_usage = existing.agent_token_usage.clone();
            loaded.current_tokens = loaded.current_tokens.max(existing.current_tokens);
            loaded.active_agent_current_tokens = existing.active_agent_current_tokens;
            loaded.active_agent_id = existing.active_agent_id.clone();
            loaded.agent_current_tokens = existing.agent_current_tokens.clone();
            loaded.compression_threshold_tokens = loaded
                .compression_threshold_tokens
                .max(existing.compression_threshold_tokens);
            loaded.context_limit_tokens = loaded
                .context_limit_tokens
                .max(existing.context_limit_tokens);
            *existing = loaded;
        } else {
            self.store.session.sessions.push(loaded);
        }
        Ok(true)
    }

    pub(in crate::app_state) fn persist_app_only(&mut self) -> Result<()> {
        self.normalize_sessions_for_storage();
        self.services.repository.persist_app_only(&self.store)
    }

    pub(in crate::app_state) fn persist_to_disk(&mut self) -> Result<()> {
        self.normalize_sessions_for_storage();
        self.services.repository.persist_to_disk(&self.store)
    }

    pub(in crate::app_state) fn remove_session_file(&self, session_id: &str) -> Result<()> {
        self.services.repository.remove_session_file(session_id)
    }

    pub(in crate::app_state) fn load_from_disk(&self) -> Result<Option<LoadedState>> {
        self.services.repository.load_from_disk()
    }

    pub(in crate::app_state) fn load_from_legacy_disk(&self) -> Result<Option<LoadedState>> {
        self.services.repository.load_from_legacy_disk()
    }

    pub fn ensure_active_session_index(&mut self) -> usize {
        if let Some(idx) = self
            .store
            .session
            .sessions
            .iter()
            .position(|session| session.id == self.store.session.active_session_id)
        {
            return idx;
        }

        let mut session = Session::new(DEFAULT_SESSION_TITLE);
        session.cwd = self.store.session.workspace_dir.clone();
        self.store.session.active_session_id = session.id.clone();
        self.store.session.sessions.push(session);
        self.store.session.sessions.len() - 1
    }

    /// 自动恢复未完成的计划（已迁移到 TiangongCore 管理）
    pub(in crate::app_state) fn try_auto_resume_unfinished_plan_for_active_session(
        &mut self,
    ) -> Result<bool> {
        // TiangongCore 统一管理执行，不再从 TiangongState 启动 turn
        Ok(false)
    }

    pub(in crate::app_state) fn recover_interrupted_tasks(&mut self) -> usize {
        let mut recovered = 0usize;
        for session in &mut self.store.session.sessions {
            let recovered_in_session = session.recover_interrupted_tasks();
            if recovered_in_session > 0 {
                recovered += recovered_in_session;
                session.append_message(
                    MessageRole::System,
                    format!(
                        "检测到 {} 个未完成任务，已在启动时恢复为失败状态",
                        recovered_in_session
                    ),
                );
            }
        }
        recovered
    }

    pub(in crate::app_state) fn normalize_sessions_for_storage(&mut self) {
        let mut seen = HashSet::new();

        for session in &mut self.store.session.sessions {
            let mut session_id =
                canonical_scru128_id(&session.id).unwrap_or_else(new_scru128_string);
            while seen.contains(&session_id) {
                session_id = new_scru128_string();
            }
            session.id = session_id.clone();
            seen.insert(session_id);
        }

        if self.store.session.sessions.is_empty() {
            self.store.session.active_session_id.clear();
            return;
        }

        if let Some(active_id) = canonical_scru128_id(&self.store.session.active_session_id)
            && seen.contains(&active_id)
        {
            self.store.session.active_session_id = active_id;
            return;
        }

        self.store.session.active_session_id = self
            .store
            .session
            .sessions
            .first()
            .map(|session| session.id.clone())
            .unwrap_or_default();
    }
}
