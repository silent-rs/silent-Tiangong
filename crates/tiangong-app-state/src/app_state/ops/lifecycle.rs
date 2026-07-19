use super::super::*;

impl TiangongState {
    pub fn load_or_default() -> Self {
        let app_storage_path = default_app_storage_path();
        let sessions_dir_path = default_sessions_dir_path();
        let default_agent_config = AgentConfig::default();

        // models_config 由 config registry 管理(进程单例,启动时 init)。
        let mut state = Self {
            store: AppStore {
                session: SessionState {
                    sessions: Vec::new(),
                    metadata: Vec::new(),
                    active_session_id: String::new(),
                    workspace_dir: default_workspace_dir(),
                    session_title_draft: DEFAULT_SESSION_TITLE.to_string(),
                    input_drafts: HashMap::new(),
                },
                provider: ProviderState {
                    model_list: Vec::new(),
                },
                agent: AgentState {
                    agent_config: default_agent_config,
                },
                runtime: RuntimeState {
                    run: RunSnapshot::default(),
                    pending_turns: HashMap::new(),
                },
            },
            services: AppServices {
                repository: AppRepository::new(AppPaths {
                    app_storage_path,
                    sessions_dir_path,
                }),
            },
            core_manager: std::sync::OnceLock::new(),
        };

        let mut loaded_from_disk = false;
        if let Ok(Some(loaded)) = state.load_from_disk() {
            state.apply_loaded_state(loaded);
            loaded_from_disk = true;
        } else if let Ok(Some(legacy_loaded)) = state.load_from_legacy_disk() {
            state.apply_loaded_state(legacy_loaded);
            let _ = state.persist_to_disk();
            // 扩展能力依赖锁同步已由对应插件在管理操作时自管。
        }

        if loaded_from_disk && !state.services.repository.paths().app_storage_path.exists() {
            // 首次安装（app.json 不存在）：持久化初始 app 状态。
            // 扩展能力配置已由各自 plugin 自管，core 不再判断其文件是否存在。
            let _ = state.persist_app_only();
        }

        if state.store.session.sessions.is_empty() {
            let mut session = Session::new(DEFAULT_SESSION_TITLE);
            session.cwd = state.store.session.workspace_dir.clone();
            session.trust_mode = state.store.agent.agent_config.default_trust_mode;
            state.store.session.active_session_id = session.id.clone();
            state.store.session.sessions.push(session);
            let _ = state.persist_to_disk();
        }
        state.resync_session_metadata();

        if !state
            .store
            .session
            .sessions
            .iter()
            .any(|session| session.id == state.store.session.active_session_id)
        {
            state.store.session.active_session_id = state
                .store
                .session
                .sessions
                .first()
                .map(|session| session.id.clone())
                .unwrap_or_default();
        }
        if !state.store.session.active_session_id.is_empty() {
            state
                .store
                .session
                .input_drafts
                .entry(state.store.session.active_session_id.clone())
                .or_default();
        }

        state.store.session.session_title_draft = state
            .active_session()
            .map(|session| session.title.clone())
            .unwrap_or_else(|| DEFAULT_SESSION_TITLE.to_string());
        let active_trust_mode = state.active_session_trust_mode();
        state.store.agent.agent_config.trust_mode = active_trust_mode;
        state.store.provider.model_list = normalize_model_list(
            state.store.provider.model_list.clone(),
            &state.current_model(),
        );

        let recovered_count = state.recover_interrupted_tasks();
        state.store.runtime.run.summary = format!("模型供应商：{}", state.provider_label());
        if recovered_count > 0 {
            state.store.runtime.run.status = RunStatus::Failed;
            state.store.runtime.run.summary =
                format!("已恢复 {recovered_count} 个中断任务（标记为失败）");
            state.store.runtime.run.last_result = Some("recovered_interrupted_tasks".to_string());
            state.store.runtime.run.last_error =
                Some("存在未完成任务，已在启动时恢复为失败".to_string());
            let _ = state.persist_to_disk();
        }

        state.store.runtime.run.updated_at = now_text();
        // EventLoop 状态恢复已移除（TiangongCore 统一管理执行状态）

        // 扩展能力均已脱离 core，由各自 plugin 自治：
        // - 依赖锁 / 工具能力缓存 / 后台调度器由对应插件在管理操作 / register 时自管

        state
    }

    fn apply_loaded_state(&mut self, loaded: LoadedState) {
        self.store.session.sessions = loaded.sessions;
        self.store.session.active_session_id = loaded.active_session_id;
        self.store.session.workspace_dir = loaded.workspace_dir;
        self.store.session.input_drafts = loaded.input_drafts;
        self.store.provider.model_list = loaded.model_list;
        if let Some(agent_config) = loaded.agent_config {
            self.store.agent.agent_config = agent_config;
        }
        self.resync_session_metadata();
    }
}
