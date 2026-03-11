use super::super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use tracing::error;

impl TiangongState {
    pub fn load_or_default() -> Self {
        let shutdown_flag = Arc::new(AtomicBool::new(false));
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
            title_generation_thread: None,
            shutdown_flag: shutdown_flag.clone(),
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

        // 启动时为未生成标题的会话批量生成标题（后台线程）
        let title_thread = state.spawn_title_generation_thread();
        state.title_generation_thread = Some(title_thread);

        state
    }

    /// 启动后台标题生成线程
    fn spawn_title_generation_thread(&mut self) -> JoinHandle<()> {
        use crate::core::session::MessageRole;

        // 收集需要生成标题的会话
        let sessions_to_update: Vec<(String, Vec<Message>)> = self
            .store
            .session
            .sessions
            .iter()
            .filter(|session| {
                // 只处理未生成标题且有 user 消息的会话
                !session.title_generated
                    && session
                        .messages
                        .iter()
                        .any(|msg| msg.role == MessageRole::User)
            })
            .map(|session| {
                // 传入整个会话的引用，而不仅仅是第一条消息
                (session.id.clone(), session.messages.clone())
            })
            .collect();

        if sessions_to_update.is_empty() {
            // 没有需要生成的会话，返回一个空线程
            return thread::spawn(|| {});
        }

        // 添加调试日志
        error!(count = sessions_to_update.len(), "启动后台标题生成线程");

        // 克隆需要在后台线程中使用的数据
        let sessions_dir_path = self.services.repository.paths().sessions_dir_path.clone();
        let model_config = self.store.provider.model_config.clone();
        let shutdown_flag = self.shutdown_flag.clone();

        // 在后台线程中生成标题
        thread::spawn(move || {
            // 创建独立的 model client 用于后台线程
            let client = SingleProviderClient::new(model_config);

            let mut success_count = 0;
            let mut fail_count = 0;

            for (index, (session_id, messages)) in sessions_to_update.iter().enumerate() {
                // 检查是否需要退出
                if shutdown_flag.load(Ordering::SeqCst) {
                    error!("后台线程收到退出信号，停止生成标题");
                    break;
                }

                error!(
                    index = index + 1,
                    total = sessions_to_update.len(),
                    session_id = %session_id,
                    message_count = messages.len(),
                    "开始处理会话"
                );

                // 尝试生成标题
                match Self::generate_title_for_session_with_client(&client, messages) {
                    Some(new_title) => {
                        error!(
                            session_id = %session_id,
                            title = %new_title,
                            "标题生成成功"
                        );

                        // 读取会话文件
                        let session_file = sessions_dir_path.join(format!("{}.json", session_id));
                        match std::fs::read_to_string(&session_file) {
                            Ok(content) => {
                                match serde_json::from_str::<Session>(&content) {
                                    Ok(mut session) => {
                                        let old_title = session.title.clone();
                                        session.title = new_title.clone();
                                        session.title_generated = true;
                                        session.updated_at = now_text();

                                        // 写回文件
                                        match serde_json::to_string_pretty(&session) {
                                            Ok(json) => {
                                                if let Err(err) =
                                                    std::fs::write(&session_file, json)
                                                {
                                                    error!(
                                                        session_id = %session_id,
                                                        path = %session_file.display(),
                                                        error = %err,
                                                        "写入会话文件失败"
                                                    );
                                                    fail_count += 1;
                                                } else {
                                                    error!(
                                                        session_id = %session_id,
                                                        old_title = %old_title,
                                                        new_title = %new_title,
                                                        "会话文件更新成功"
                                                    );
                                                    success_count += 1;
                                                }
                                            }
                                            Err(err) => {
                                                error!(
                                                    session_id = %session_id,
                                                    error = %err,
                                                    "序列化会话失败"
                                                );
                                                fail_count += 1;
                                            }
                                        }
                                    }
                                    Err(err) => {
                                        error!(
                                            session_id = %session_id,
                                            error = %err,
                                            "解析会话文件失败"
                                        );
                                        fail_count += 1;
                                    }
                                }
                            }
                            Err(err) => {
                                error!(
                                    session_id = %session_id,
                                    path = %session_file.display(),
                                    error = %err,
                                    "读取会话文件失败"
                                );
                                fail_count += 1;
                            }
                        }
                    }
                    None => {
                        error!(
                            session_id = %session_id,
                            "标题生成失败（模型返回空结果）"
                        );
                        fail_count += 1;
                    }
                }

                // 每生成一个标题后检查是否需要退出
                if shutdown_flag.load(Ordering::SeqCst) {
                    break;
                }
            }

            error!(
                success = success_count,
                fail = fail_count,
                total = sessions_to_update.len(),
                "后台标题生成线程完成"
            );
        })
    }

    /// 为单个会话生成标题（基于整个会话内容）
    #[allow(dead_code)]
    fn generate_title_for_session(&self, messages: &[Message]) -> Option<String> {
        use crate::core::session::MessageRole;

        if messages.is_empty() {
            return None;
        }

        // 构建对话摘要
        let conversation_summary: String = messages
            .iter()
            .filter(|msg| {
                // 只包含用户和助手的对话消息，排除系统消息
                matches!(msg.role, MessageRole::User | MessageRole::Assistant)
            })
            .map(|msg| {
                let role = match msg.role {
                    MessageRole::User => "用户",
                    MessageRole::Assistant => "助手",
                    _ => "",
                };
                format!("{}: {}", role, msg.content.trim())
            })
            .collect::<Vec<_>>()
            .join("\n");

        if conversation_summary.is_empty() {
            return None;
        }

        let prompt = format!(
            "根据以下对话内容，生成一个简洁的会话标题（不超过10个汉字）：\n{}",
            conversation_summary
        );

        match self.services.runtime.complete_lite(&prompt) {
            Ok(title) => {
                let title = title.trim().to_string();
                // 更严格的长度检查：标题应该在10个汉字以内
                if title.is_empty() || title.len() > 20 {
                    None
                } else {
                    Some(title)
                }
            }
            Err(_) => None,
        }
    }

    /// 生成降级标题（当模型生成失败时使用）
    fn generate_fallback_title_for_client(messages: &[Message]) -> String {
        use crate::core::session::MessageRole;
        use chrono::Local;

        // 尝试从对话中提取关键词
        let user_content: String = messages
            .iter()
            .filter(|msg| msg.role == MessageRole::User)
            .map(|msg| msg.content.trim())
            .collect::<Vec<_>>()
            .join(" ");

        // 如果用户输入很短，直接截取作为标题
        if user_content.len() <= 10 {
            user_content
        } else {
            // 用户输入较长，使用日期
            format!("对话 {}", Local::now().format("%m-%d"))
        }
    }

    /// 为单个会话生成标题（后台线程版本，使用独立的 client）
    fn generate_title_for_session_with_client(
        client: &SingleProviderClient,
        messages: &[Message],
    ) -> Option<String> {
        use crate::core::session::MessageRole;

        if messages.is_empty() {
            return None;
        }

        // 构建对话摘要
        let conversation_summary: String = messages
            .iter()
            .filter(|msg| {
                // 只包含用户和助手的对话消息，排除系统消息
                matches!(msg.role, MessageRole::User | MessageRole::Assistant)
            })
            .map(|msg| {
                let role = match msg.role {
                    MessageRole::User => "用户",
                    MessageRole::Assistant => "助手",
                    _ => "",
                };
                format!("{}: {}", role, msg.content.trim())
            })
            .collect::<Vec<_>>()
            .join("\n");

        if conversation_summary.is_empty() {
            return None;
        }

        // 改进的 prompt，更明确的指令
        let prompt = format!(
            "你是会话标题生成助手。根据以下对话内容生成一个简洁的标题。

要求：
1. 标题长度：2-10个汉字
2. 直接返回标题文本，不要任何解释、引号或额外文字
3. 如果对话内容简单（如问候），可以用关键词作为标题
4. 标题要能概括对话的主要内容

示例：
- 对话关于Python编程 -> 'Python脚本开发'
- 对话问候 -> '新对话'
- 对话故事创作 -> '故事创作'

对话内容：
{}",
            conversation_summary
        );

        match client.complete_lite(&prompt) {
            Ok(title) => {
                let title = title.trim().to_string();

                // 改进的验证逻辑（更宽松）
                if title.is_empty() {
                    error!(
                        model_response = %title,
                        "标题为空，使用降级策略"
                    );
                    Some(Self::generate_fallback_title_for_client(messages))
                } else if title.len() > 20 {
                    // 如果标题太长，截取前10个字符
                    let truncated: String = title.chars().take(10).collect();
                    error!(
                        original_title = %title,
                        truncated_title = %truncated,
                        "标题过长，已截取"
                    );
                    Some(truncated)
                } else {
                    Some(title)
                }
            }
            Err(err) => {
                // 记录详细错误
                error!(
                    error = %err,
                    prompt_preview = %prompt.chars().take(200).collect::<String>(),
                    "轻量级模型请求失败，使用降级策略"
                );
                Some(Self::generate_fallback_title_for_client(messages))
            }
        }
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
