use super::super::super::*;

impl TiangongState {
    pub fn sessions(&self) -> &[Session] {
        &self.store.session.sessions
    }

    pub fn sessions_mut(&mut self) -> &mut Vec<Session> {
        &mut self.store.session.sessions
    }

    pub fn active_session_id(&self) -> &str {
        &self.store.session.active_session_id
    }

    pub fn active_session(&self) -> Option<&Session> {
        self.store
            .session
            .sessions
            .iter()
            .find(|session| session.id == self.store.session.active_session_id)
    }

    pub fn active_task_plans(&self) -> Vec<SessionTaskPlan> {
        let Some(session) = self.active_session() else {
            return Vec::new();
        };
        session.task_plans.to_vec()
    }

    pub fn has_pending_turn(&self) -> bool {
        !self.store.runtime.pending_turns.is_empty()
    }

    pub fn has_pending_turn_for(&self, session_id: &str) -> bool {
        self.store.runtime.pending_turns.contains_key(session_id)
    }

    pub fn pending_session_ids(&self) -> Vec<String> {
        self.store.runtime.pending_turns.keys().cloned().collect()
    }

    pub fn update_draft(&mut self, value: String) {
        self.store.session.input_draft = value;
    }

    pub fn provider_label(&self) -> String {
        self.services.runtime.provider_label()
    }

    pub fn models_config(&self) -> &crate::models_config::ModelsConfig {
        &self.store.provider.models_config
    }

    /// 根据当前应用状态构建供 TiangongCore 使用的最小配置快照
    pub fn build_core_config_from_base(
        &self,
        base: &crate::core_config::CoreConfig,
    ) -> crate::core_config::CoreConfig {
        crate::core_config::CoreConfig {
            llm: crate::core_config::LlmConfig::from_models_config(self.models_config()),
            mcp: self.agent_config().mcp.clone(),
            mcp_capabilities: base.mcp_capabilities.clone(),
            skills: self.agent_config().skills.clone(),
            trust_mode: self.agent_config().trust_mode,
            context_limit: base.context_limit,
        }
    }

    pub fn session_title_draft(&self) -> &str {
        &self.store.session.session_title_draft
    }

    pub fn update_session_title_draft(&mut self, value: String) {
        self.store.session.session_title_draft = value;
    }

    pub fn active_session_cwd(&self) -> &str {
        self.active_session().map(|s| s.cwd.as_str()).unwrap_or("")
    }

    pub fn update_active_session_cwd(&mut self, cwd: String) -> Result<()> {
        let active_id = self.store.session.active_session_id.clone();
        if let Some(session) = self
            .store
            .session
            .sessions
            .iter_mut()
            .find(|s| s.id == active_id)
        {
            // Isolated 模式不允许修改工作目录
            if session.cwd_mode == crate::session::SessionCwdMode::Isolated {
                return Err(anyhow::anyhow!("隔离模式会话不允许修改工作目录"));
            }
            session.cwd = cwd;
            session.cwd_mode = crate::session::SessionCwdMode::Custom;
            session.updated_at = now_text();
        }
        self.persist_session_and_app(&active_id)
    }
}
