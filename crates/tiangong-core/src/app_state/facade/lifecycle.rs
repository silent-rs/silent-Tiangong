use crate::models_config::ModelsConfig;

use super::super::*;

impl TiangongState {
    pub fn load_or_default() -> Self {
        let app_storage_path = default_app_storage_path();
        let skills_config_path = default_skills_config_path();
        let mcp_config_path = default_mcp_config_path();
        let mcp_capability_cache_path = default_mcp_capability_cache_path();
        let sessions_dir_path = default_sessions_dir_path();
        let default_agent_config = AgentConfig::default();

        // ModelsConfig 为主配置源
        let mut models_config = ModelsConfig::load();
        if models_config.is_empty() {
            // 从环境变量生成默认 ModelsConfig
            let env_config = ModelProviderConfig::from_env();
            if !env_config.api_auth_token.is_empty() {
                models_config = ModelsConfig::from_legacy(&env_config);
                let _ = models_config.save();
            }
        }

        // 从 models_config 生成内部 ModelProviderConfig
        let model_config = models_config.to_chat_provider_config();

        let context_limit = crate::core_config::resolve_context_limit(&model_config.api_model);
        let runtime = RuntimeEngine::new(
            SingleProviderClient::new(model_config.clone()),
            context_limit,
            default_agent_config.clone(),
        )
        .with_models_config(models_config.clone());

        let mut state = Self {
            store: AppStore {
                session: SessionState {
                    sessions: Vec::new(),
                    active_session_id: String::new(),
                    workspace_dir: default_workspace_dir(),
                    session_title_draft: DEFAULT_SESSION_TITLE.to_string(),
                    input_draft: String::new(),
                },
                provider: ProviderState {
                    models_config,
                    model_config,
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
                skill_registry: std::sync::Arc::new(crate::skill::SkillRegistry::new(
                    default_skills_storage_dir_path(),
                )),
            },
        };

        let mut loaded_from_disk = false;
        if let Ok(Some(loaded)) = state.load_from_disk() {
            state.apply_loaded_state(loaded);
            loaded_from_disk = true;
        } else if let Ok(Some(legacy_loaded)) = state.load_from_legacy_disk() {
            state.apply_loaded_state(legacy_loaded);
            let _ = state.persist_to_disk();
            let _ = state.persist_agent_configs_only();
        }

        if loaded_from_disk
            && (!state
                .services
                .repository
                .paths()
                .skills_config_path
                .exists()
                || !state.services.repository.paths().mcp_config_path.exists())
        {
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

        state.store.session.session_title_draft = state
            .active_session()
            .map(|session| session.title.clone())
            .unwrap_or_else(|| DEFAULT_SESSION_TITLE.to_string());
        let active_trust_mode = state.active_session_trust_mode();
        state.store.agent.agent_config.trust_mode = active_trust_mode;
        state
            .services
            .runtime
            .permission_gate()
            .set_trust_mode(active_trust_mode);
        state.rebuild_runtime_from_current_config();
        state.store.provider.model_list = normalize_model_list(
            state.store.provider.model_list.clone(),
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
        // EventLoop 状态恢复已移除（TiangongCore 统一管理执行状态）

        // skills 已脱离 agent_config（由 skill plugin 自治），此处仅同步 MCP 依赖锁
        let _ = state.sync_mcp_dependency_lock();
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
        self.store.session.workspace_dir = loaded.workspace_dir;
        self.store.provider.model_list = loaded.model_list;
        if let Some(agent_config) = loaded.agent_config {
            self.store.agent.agent_config = agent_config;
            // agent_config 变更后需重建 runtime，否则 runtime 持有默认空 MCP 配置
            self.rebuild_runtime_from_current_config();
        }
    }

    pub(in crate::app_state) fn rebuild_runtime_for_agent_config(&mut self) {
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
