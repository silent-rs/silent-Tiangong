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

    pub(in crate::app_state) fn persist_app_only(&mut self) -> Result<()> {
        self.normalize_sessions_for_storage();
        self.services.repository.persist_app_only(&self.store)
    }

    /// 仅持久化 agent 配置（skills.json、mcp.json）+ MCP 依赖锁。
    /// 只应由显式修改 agent_config 的操作调用，避免多进程覆盖。
    pub(in crate::app_state) fn persist_agent_configs_only(&self) -> Result<()> {
        self.services
            .repository
            .persist_agent_configs(&self.store.agent.agent_config)?;
        self.services
            .repository
            .sync_mcp_dependency_lock(&self.store.agent.agent_config)
    }

    pub(in crate::app_state) fn persist_to_disk(&mut self) -> Result<()> {
        self.normalize_sessions_for_storage();
        self.services.repository.persist_to_disk(&self.store)
    }

    pub(in crate::app_state) fn remove_session_file(&self, session_id: &str) -> Result<()> {
        self.services.repository.remove_session_file(session_id)
    }

    pub(in crate::app_state) fn sync_mcp_dependency_lock(&self) -> Result<()> {
        self.services
            .repository
            .sync_mcp_dependency_lock(&self.store.agent.agent_config)
    }

    pub(in crate::app_state) fn load_from_disk(&self) -> Result<Option<LoadedState>> {
        self.services.repository.load_from_disk()
    }

    pub(in crate::app_state) fn load_from_legacy_disk(&self) -> Result<Option<LoadedState>> {
        self.services.repository.load_from_legacy_disk()
    }

    /// 扫描磁盘会话目录，将新出现的会话加载到内存（不覆盖已有会话）。
    /// 用于 Tauri app 感知 server 等外部进程创建的会话。
    pub fn sync_sessions_from_disk(&mut self) {
        let Ok(disk_ids) = self.services.repository.list_session_ids_from_dir() else {
            return;
        };
        let existing_ids: HashSet<String> = self
            .store
            .session
            .sessions
            .iter()
            .map(|s| s.id.clone())
            .collect();
        let trust_mode = self.store.agent.agent_config.default_trust_mode;
        for id in disk_ids {
            if existing_ids.contains(&id) {
                continue;
            }
            if let Ok(Some(session)) = self
                .services
                .repository
                .load_session_from_disk(&id, trust_mode)
                && session.parent_session_id.is_none()
            {
                self.store.session.sessions.push(session);
            }
        }
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
