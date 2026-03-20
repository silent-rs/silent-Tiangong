use super::super::*;

impl TiangongState {
    pub fn load_or_default() -> Self {
        let app_storage_path = default_app_storage_path();
        let skills_config_path = default_skills_config_path();
        let mcp_config_path = default_mcp_config_path();
        let mcp_capability_cache_path = default_mcp_capability_cache_path();
        let sessions_dir_path = default_sessions_dir_path();
        let default_model_config = ModelProviderConfig::from_env();
        let default_agent_config = AgentConfig::default();
        let runtime = RuntimeEngine::new(
            SingleProviderClient::new(default_model_config.clone()),
            DEFAULT_CONTEXT_LIMIT,
            default_agent_config.clone(),
        );

        let mut state = Self {
            store: AppStore {
                session: SessionState {
                    sessions: Vec::new(),
                    active_session_id: String::new(),
                    session_title_draft: DEFAULT_SESSION_TITLE.to_string(),
                    input_draft: String::new(),
                },
                provider: ProviderState {
                    model_config: default_model_config.clone(),
                    settings_api_auth_token_draft: default_model_config.api_auth_token.clone(),
                    settings_api_base_url_draft: default_model_config.api_base_url.clone(),
                    settings_api_timeout_ms_draft: default_model_config.api_timeout_ms.clone(),
                    settings_api_model_draft: default_model_config.api_model.clone(),
                    settings_model_list: Vec::new(),
                },
                agent: AgentState {
                    agent_config: default_agent_config,
                },
                runtime: RuntimeState {
                    run: RunSnapshot::default(),
                    pending_turn: None,
                },
            },
            services: AppServices {
                skill_service: AppSkillService,
                mcp_service: AppMcpService,
                repository: AppRepository::new(AppPaths {
                    app_storage_path,
                    skills_config_path,
                    mcp_config_path,
                    mcp_capability_cache_path,
                    sessions_dir_path,
                }),
                runtime,
                turn_service: AppTurnService,
            },
        };

        if let Ok(Some(loaded)) = state.load_from_disk() {
            state.apply_loaded_state(loaded);
        } else if let Ok(Some(legacy_loaded)) = state.load_from_legacy_disk() {
            state.apply_loaded_state(legacy_loaded);
            let _ = state.persist_to_disk();
        }
        if !state
            .services
            .repository
            .paths()
            .skills_config_path
            .exists()
            || !state.services.repository.paths().mcp_config_path.exists()
        {
            let _ = state.persist_app_only();
        }

        if state.store.session.sessions.is_empty() {
            let session = Session::new(DEFAULT_SESSION_TITLE);
            state.store.session.active_session_id = session.id.clone();
            state.store.session.sessions.push(session);
            let _ = state.persist_to_disk();
        }

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

        state.store.provider.settings_api_auth_token_draft =
            state.store.provider.model_config.api_auth_token.clone();
        state.store.provider.settings_api_base_url_draft =
            state.store.provider.model_config.api_base_url.clone();
        state.store.provider.settings_api_timeout_ms_draft =
            state.store.provider.model_config.api_timeout_ms.clone();
        state.store.provider.settings_api_model_draft =
            state.store.provider.model_config.api_model.clone();
        state.store.session.session_title_draft = state
            .active_session()
            .map(|session| session.title.clone())
            .unwrap_or_else(|| DEFAULT_SESSION_TITLE.to_string());
        state.store.provider.settings_model_list = normalize_model_list(
            state.store.provider.settings_model_list.clone(),
            &state.store.provider.model_config.api_model,
        );

        let recovered_count = state.recover_interrupted_tasks();
        state.store.runtime.run.summary =
            format!("模型供应商：{}", state.services.runtime.provider_label());
        if recovered_count > 0 {
            state.store.runtime.run.status = RunStatus::Failed;
            state.store.runtime.run.summary =
                format!("已恢复 {recovered_count} 个中断任务（标记为失败）");
            state.store.runtime.run.last_result = Some("recovered_interrupted_tasks".to_string());
            state.store.runtime.run.last_error =
                Some("存在未完成任务，已在启动时恢复为失败".to_string());
            let _ = state.persist_to_disk();
        }

        if let Ok(true) = state.try_auto_resume_unfinished_plan_for_active_session() {
            state.store.runtime.run.summary =
                "检测到未完成 plan，已在启动时自动继续执行".to_string();
            state.store.runtime.run.last_result =
                Some("auto_resumed_unfinished_plan_on_startup".to_string());
            state.store.runtime.run.last_error = None;
        }
        state.store.runtime.run.updated_at = now_text();
        let _ = state.sync_skill_locks();
        let _ = load_mcp_capabilities_cache(
            &state.services.repository.paths().mcp_capability_cache_path,
        );
        configure_mcp_capability_scheduler(
            state.store.agent.agent_config.mcp.clone(),
            state
                .services
                .repository
                .paths()
                .mcp_capability_cache_path
                .clone(),
            MCP_CAPABILITY_REFRESH_INTERVAL_SECS,
        );
        refresh_mcp_capabilities_async(state.store.agent.agent_config.mcp.clone());

        state
    }

    fn apply_loaded_state(&mut self, loaded: LoadedState) {
        self.store.session.sessions = loaded.sessions;
        self.store.session.active_session_id = loaded.active_session_id;
        self.store.provider.settings_model_list = loaded.model_list;
        if let Some(agent_config) = loaded.agent_config {
            self.store.agent.agent_config = agent_config;
        }
        if let Some(model_config) = loaded.model_config {
            self.store.provider.model_config = model_config;
            self.rebuild_runtime_from_current_config();
        }
    }

    pub(in crate::core::app_state) fn rebuild_runtime_for_agent_config(&mut self) {
        self.rebuild_runtime_from_current_config();
        configure_mcp_capability_scheduler(
            self.store.agent.agent_config.mcp.clone(),
            self.services
                .repository
                .paths()
                .mcp_capability_cache_path
                .clone(),
            MCP_CAPABILITY_REFRESH_INTERVAL_SECS,
        );
        refresh_mcp_capabilities_async(self.store.agent.agent_config.mcp.clone());
    }
}
