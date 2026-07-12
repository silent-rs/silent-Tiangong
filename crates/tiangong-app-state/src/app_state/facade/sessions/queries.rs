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

    pub fn active_session_trust_mode(&self) -> tiangong_core::permission::TrustMode {
        self.active_session()
            .map(|session| session.trust_mode)
            .unwrap_or(self.store.agent.agent_config.default_trust_mode)
    }

    pub fn active_session_reasoning_effort(&self) -> String {
        self.active_session()
            .and_then(|session| session.reasoning_effort.as_deref())
            .map(str::trim)
            .filter(|effort| !effort.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| self.store.agent.agent_config.reasoning_effort.clone())
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

    pub fn has_active_turn_for(&self, session_id: &str) -> bool {
        self.store
            .runtime
            .pending_turns
            .get(session_id)
            .is_some_and(|pending| {
                pending.legacy_pending || !pending.accepted_message_ids.is_empty()
            })
    }

    pub fn pending_session_ids(&self) -> Vec<String> {
        self.store.runtime.pending_turns.keys().cloned().collect()
    }

    pub fn mark_pending_turn_for(&mut self, session_id: impl Into<String>) {
        let session_id = session_id.into();
        self.store
            .runtime
            .pending_turns
            .entry(session_id.clone())
            .or_insert_with(|| PendingTurnStub {
                session_id,
                queued_message_ids: HashSet::new(),
                accepted_message_ids: HashSet::new(),
                legacy_pending: false,
            })
            .legacy_pending = true;
    }

    pub fn mark_pending_message_for(&mut self, session_id: &str, message_id: &str) {
        let pending = self
            .store
            .runtime
            .pending_turns
            .entry(session_id.to_string())
            .or_insert_with(|| PendingTurnStub {
                session_id: session_id.to_string(),
                queued_message_ids: HashSet::new(),
                accepted_message_ids: HashSet::new(),
                legacy_pending: false,
            });
        pending.queued_message_ids.insert(message_id.to_string());
    }

    pub fn accept_pending_message_for(&mut self, session_id: &str, message_id: &str) {
        let pending = self
            .store
            .runtime
            .pending_turns
            .entry(session_id.to_string())
            .or_insert_with(|| PendingTurnStub {
                session_id: session_id.to_string(),
                queued_message_ids: HashSet::new(),
                accepted_message_ids: HashSet::new(),
                legacy_pending: false,
            });
        pending.queued_message_ids.remove(message_id);
        pending.accepted_message_ids.insert(message_id.to_string());
    }

    /// 当前 turn 的最终 User 快照已提交：清除本轮所有已接受消息，只保留仍在
    /// Core 命令队列、尚未开始的后续消息。
    pub fn complete_accepted_turn_for(&mut self, session_id: &str) {
        let should_remove = self
            .store
            .runtime
            .pending_turns
            .get_mut(session_id)
            .is_some_and(|pending| {
                pending.accepted_message_ids.clear();
                pending.legacy_pending = false;
                pending.queued_message_ids.is_empty()
            });
        if should_remove {
            self.store.runtime.pending_turns.remove(session_id);
        }
    }

    pub fn remove_pending_message_for(&mut self, session_id: &str, message_id: &str) {
        let should_remove = self
            .store
            .runtime
            .pending_turns
            .get_mut(session_id)
            .is_some_and(|pending| {
                pending.queued_message_ids.remove(message_id);
                pending.accepted_message_ids.remove(message_id);
                !pending.legacy_pending
                    && pending.queued_message_ids.is_empty()
                    && pending.accepted_message_ids.is_empty()
            });
        if should_remove {
            self.store.runtime.pending_turns.remove(session_id);
        }
    }

    pub fn clear_pending_turn_for(&mut self, session_id: &str) {
        self.store.runtime.pending_turns.remove(session_id);
    }

    pub fn update_draft(&mut self, value: String) {
        let active_id = self.store.session.active_session_id.clone();
        let draft = self
            .store
            .session
            .input_drafts
            .entry(active_id)
            .or_default();
        draft.text = value;
        draft.revision = draft.revision.saturating_add(1);
    }

    pub fn provider_label(&self) -> String {
        self.services.runtime.provider_label()
    }

    pub fn models_config(&self) -> &tiangong_llm::models_config::ModelsConfig {
        &self.store.provider.models_config
    }

    /// 根据当前应用状态构建供 TiangongCore 使用的最小配置快照
    pub fn build_core_config_from_base(
        &self,
        base: &tiangong_core::core_config::CoreConfig,
    ) -> tiangong_core::core_config::CoreConfig {
        self.build_core_config_for_session_from_base(base, self.active_session_id())
    }

    /// 按显式目标会话构建配置，异步发送期间即使活动会话切换也不会借用其他会话的
    /// trust_mode 或 reasoning_effort。
    pub fn build_core_config_for_session_from_base(
        &self,
        _base: &tiangong_core::core_config::CoreConfig,
        session_id: &str,
    ) -> tiangong_core::core_config::CoreConfig {
        let llm = tiangong_core::core_config::LlmConfig::from_models_config(self.models_config());
        // context_limit 由 config 层解析注入（core 不做配置磁盘 IO）。
        let context_limit = Self::resolve_chat_context_limit(self.models_config(), &llm.chat.model);
        let target_session = self
            .sessions()
            .iter()
            .find(|session| session.id == session_id);
        let trust_mode = target_session
            .map(|session| session.trust_mode)
            .unwrap_or(self.agent_config().default_trust_mode);
        let reasoning_effort = target_session
            .and_then(|session| session.reasoning_effort.as_deref())
            .map(str::trim)
            .filter(|effort| !effort.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| self.agent_config().reasoning_effort.clone());
        tiangong_core::core_config::CoreConfig {
            llm,
            trust_mode,
            default_trust_mode: self.agent_config().default_trust_mode,
            custom_system_prompt: self.agent_config().custom_system_prompt.clone(),
            reasoning_effort,
            context_limit,
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

    pub fn workspace_dir(&self) -> &str {
        &self.store.session.workspace_dir
    }

    pub fn update_workspace_dir(&mut self, workspace_dir: String) -> Result<()> {
        self.store.session.workspace_dir = workspace_dir;
        for session in &mut self.store.session.sessions {
            if session.cwd_mode == tiangong_core::session::SessionCwdMode::Inherit
                && !session.has_user_messages()
            {
                session.cwd = self.store.session.workspace_dir.clone();
            }
        }
        self.persist_app_only()
    }

    pub fn active_session_effective_cwd(&self) -> String {
        self.active_session()
            .map(|session| session.cwd.trim())
            .filter(|cwd| !cwd.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| self.store.session.workspace_dir.clone())
    }

    pub fn update_active_session_cwd(&mut self, cwd: String) -> Result<()> {
        let active_id = self.store.session.active_session_id.clone();
        self.update_session_cwd(&active_id, cwd)
    }

    /// 按显式会话 ID 更新工作目录，异步创建/转正期间不依赖全局活动会话。
    pub fn update_session_cwd(&mut self, session_id: &str, cwd: String) -> Result<()> {
        if let Some(session) = self
            .store
            .session
            .sessions
            .iter_mut()
            .find(|s| s.id == session_id)
        {
            // Isolated 模式不允许修改工作目录
            if session.cwd_mode == tiangong_core::session::SessionCwdMode::Isolated {
                return Err(anyhow::anyhow!("隔离模式会话不允许修改工作目录"));
            }
            // 已有对话的会话不允许切换工作目录
            if session.has_user_messages() {
                return Err(anyhow::anyhow!("已有对话的会话不允许切换工作目录"));
            }
            session.cwd = cwd;
            session.cwd_mode = tiangong_core::session::SessionCwdMode::Custom;
        } else {
            return Err(anyhow::anyhow!("会话不存在：{session_id}"));
        }
        self.persist_session_and_app(session_id)
    }
}
