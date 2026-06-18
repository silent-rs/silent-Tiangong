use std::collections::HashMap;
use std::sync::Mutex;

use tiangong_config::load_tiangong_config;
use tiangong_core::core::TiangongCore;
use tiangong_core::core_config::CoreConfigProvider;
use tokio::sync::Mutex as AsyncMutex;
use tracing::warn;

/// 天工应用状态
///
/// state: 应用管理（会话列表、配置、持久化）— Arc<tokio Mutex> 以支持嵌入式 server 共享
/// cores: 活跃的对话核心（session_id → TiangongCore）
/// config: 共享配置提供者
/// embedded_server: 嵌入式 Server 句柄（Desktop 模式下 Server 运行在 app 进程内）
pub struct TiangongApp {
    pub state: std::sync::Arc<AsyncMutex<tiangong_core::app_state::TiangongState>>,
    pub cores: Mutex<HashMap<String, TiangongCore>>,
    pub config: CoreConfigProvider,
    embedded_server: Mutex<Option<tiangong_server::EmbeddedServerHandle>>,
    /// 浏览器页面获取能力（由 plugin 在 setup 阶段注入）
    page_fetcher: Mutex<Option<std::sync::Arc<dyn tiangong_core::browser_trait::PageFetcher>>>,
    /// 工具覆盖处理器（由 plugin 在 setup 阶段注入）
    tool_overrides: Mutex<
        HashMap<String, std::sync::Arc<dyn tiangong_core::tool_override::ToolOverrideHandler>>,
    >,
    /// 终端能力（由 plugin 在 setup 阶段注入）
    terminal_provider:
        Mutex<Option<std::sync::Arc<dyn tiangong_core::terminal_trait::TerminalProvider>>>,
    /// Plugin 工具规格提供者
    tool_spec_providers:
        Mutex<Vec<std::sync::Arc<dyn tiangong_core::tool_override::ToolSpecProvider>>>,
    /// Plugin Prompt 段落提供者
    prompt_section_providers:
        Mutex<Vec<std::sync::Arc<dyn tiangong_core::tool_override::PromptSectionProvider>>>,
}

impl Default for TiangongApp {
    fn default() -> Self {
        let app_config = load_tiangong_config();
        let core_config = app_config.to_core_config();
        let config = CoreConfigProvider::new(core_config);

        Self {
            state: std::sync::Arc::new(AsyncMutex::new(
                tiangong_core::app_state::TiangongState::load_or_default(),
            )),
            cores: Mutex::new(HashMap::new()),
            config,
            embedded_server: Mutex::new(None),
            page_fetcher: Mutex::new(None),
            tool_overrides: Mutex::new(HashMap::new()),
            terminal_provider: Mutex::new(None),
            tool_spec_providers: Mutex::new(Vec::new()),
            prompt_section_providers: Mutex::new(Vec::new()),
        }
    }
}

impl TiangongApp {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注入浏览器页面获取能力（由 plugin 在 setup 阶段调用）
    pub fn set_page_fetcher(
        &self,
        fetcher: std::sync::Arc<dyn tiangong_core::browser_trait::PageFetcher>,
    ) {
        if let Ok(mut guard) = self.page_fetcher.lock() {
            *guard = Some(fetcher);
        }
    }

    /// 注入终端能力（由 plugin 在 setup 阶段调用）
    pub fn set_terminal_provider(
        &self,
        provider: std::sync::Arc<dyn tiangong_core::terminal_trait::TerminalProvider>,
    ) {
        if let Ok(mut guard) = self.terminal_provider.lock() {
            *guard = Some(provider);
        }
    }

    /// 注册 Plugin 工具规格提供者
    pub fn register_tool_spec_provider(
        &self,
        provider: std::sync::Arc<dyn tiangong_core::tool_override::ToolSpecProvider>,
    ) {
        if let Ok(mut guard) = self.tool_spec_providers.lock() {
            guard.push(provider);
        }
    }

    /// 注册 Plugin Prompt 段落提供者
    pub fn register_prompt_section_provider(
        &self,
        provider: std::sync::Arc<dyn tiangong_core::tool_override::PromptSectionProvider>,
    ) {
        if let Ok(mut guard) = self.prompt_section_providers.lock() {
            guard.push(provider);
        }
    }

    /// 注册工具覆盖处理器（由 plugin 在 setup 阶段调用）
    pub fn register_tool_override(
        &self,
        name: &str,
        handler: std::sync::Arc<dyn tiangong_core::tool_override::ToolOverrideHandler>,
    ) {
        if let Ok(mut guard) = self.tool_overrides.lock() {
            guard.insert(name.to_string(), handler);
        }
    }

    /// 向当前活跃会话注入浏览器页面内容
    pub async fn inject_browser_content(
        &self,
        title: String,
        url: String,
        text: String,
        tabs: Vec<(String, String, String)>,
        active_tab_id: Option<String>,
        feedback: Option<String>,
    ) -> bool {
        let guard = self.state.lock().await;
        let session_id = guard.active_session_id().to_string();
        drop(guard);

        let cores = self.lock_cores();
        if let Some(core) = cores.get(&session_id) {
            if core.is_running() {
                let has_feedback = feedback
                    .as_deref()
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false);
                tracing::info!(
                    session_id,
                    url = %url,
                    text_len = text.len(),
                    has_feedback,
                    "向当前会话注入浏览器内容"
                );
                return core.inject_browser_content(
                    title,
                    url,
                    text,
                    tabs,
                    active_tab_id,
                    feedback,
                );
            } else {
                tracing::debug!(
                    session_id,
                    url = %url,
                    "跳过浏览器内容注入：当前会话 core 未运行"
                );
            }
        } else {
            tracing::debug!(
                session_id,
                url = %url,
                "跳过浏览器内容注入：当前会话没有活跃 core"
            );
        }
        false
    }

    /// 注入用户终端操作到当前会话的对话链。
    ///
    /// 用户在终端提交命令时由 main.rs 的 `terminal:user_command` 事件监听器调用。
    /// 无论 Agent 是否运行都注入：运行时 Agent 在下一轮看到；空闲时记入对话历史，
    /// Agent 下次被唤醒时能看到。空闲时不触发新 turn（由 worker 主循环保证）。
    pub async fn inject_terminal_user_input(&self, command: String) -> bool {
        let guard = self.state.lock().await;
        let session_id = guard.active_session_id().to_string();
        drop(guard);

        let cores = self.lock_cores();
        if let Some(core) = cores.get(&session_id) {
            tracing::info!(
                session_id,
                command = %command,
                running = core.is_running(),
                "向当前会话注入用户终端操作"
            );
            return core.inject_terminal_user_input(command);
        }
        tracing::debug!(
            session_id,
            command = %command,
            "跳过终端用户操作注入：当前会话没有活跃 core"
        );
        false
    }

    fn lock_cores(&self) -> std::sync::MutexGuard<'_, HashMap<String, TiangongCore>> {
        match self.cores.lock() {
            Ok(guard) => guard,
            Err(err) => {
                warn!(error = %err, "cores 锁已污染，尝试恢复");
                err.into_inner()
            }
        }
    }

    fn lock_embedded_server(
        &self,
    ) -> std::sync::MutexGuard<'_, Option<tiangong_server::EmbeddedServerHandle>> {
        match self.embedded_server.lock() {
            Ok(guard) => guard,
            Err(err) => {
                warn!(error = %err, "embedded_server 锁已污染，尝试恢复");
                err.into_inner()
            }
        }
    }

    pub async fn sync_core_config_from_state(&self) -> Result<(), String> {
        let base = self.config.snapshot();
        let next = self
            .with_state_read(|core_state| Ok(core_state.build_core_config_from_base(&base)))
            .await?;
        self.config.replace(next);
        if let Ok(cores) = self.cores.lock() {
            for core in cores.values() {
                let _ = core.reload_config();
            }
        }
        Ok(())
    }

    pub async fn with_state<F, R>(&self, f: F) -> Result<R, String>
    where
        F: FnOnce(&mut tiangong_core::app_state::TiangongState) -> Result<R, anyhow::Error>,
    {
        let mut guard = self.state.lock().await;
        f(&mut guard).map_err(|e| e.to_string())
    }

    pub async fn with_state_read<F, R>(&self, f: F) -> Result<R, String>
    where
        F: FnOnce(&tiangong_core::app_state::TiangongState) -> Result<R, anyhow::Error>,
    {
        let guard = self.state.lock().await;
        f(&guard).map_err(|e| e.to_string())
    }

    /// 获取或创建会话对应的 TiangongCore
    ///
    /// 如果 core 已存在（多轮对话），直接复用。
    /// stream_tx 只在创建新 core 时使用。
    pub fn ensure_core(
        &self,
        session_id: &str,
        session: tiangong_core::session::Session,
        stream_tx: std::sync::mpsc::Sender<tiangong_types::SessionStreamEvent>,
    ) -> (String, bool) {
        let mut cores = self.lock_cores();
        if let Some(core) = cores.get(session_id) {
            if core.is_running() {
                let _ = core.reload_config();
                let _ = core.update_cwd(session.cwd.clone());
                core.set_trust_mode(session.trust_mode);
                return (session_id.to_string(), false); // 已存在，复用
            }
            warn!(session_id, "移除已停止的 TiangongCore");
            cores.remove(session_id);
        }
        let core = TiangongCore::with_session_for_gui(self.config.clone(), session, stream_tx);
        // 注入浏览器页面获取能力和工具覆盖（由 plugin setup 阶段注册）
        if let Ok(guard) = self.page_fetcher.lock() {
            if let Some(ref fetcher) = *guard {
                core.set_page_fetcher(fetcher.clone());
            }
        }
        if let Ok(guard) = self.tool_overrides.lock() {
            for (name, handler) in guard.iter() {
                core.register_tool_override(name, handler.clone());
            }
        }
        // 注入终端能力、工具规格、Prompt 段落
        if let Ok(guard) = self.terminal_provider.lock() {
            if let Some(ref provider) = *guard {
                core.set_terminal_provider(provider.clone());
            }
        }
        if let Ok(guard) = self.tool_spec_providers.lock() {
            for provider in guard.iter() {
                core.register_tool_spec_provider(provider.clone());
            }
        }
        if let Ok(guard) = self.prompt_section_providers.lock() {
            for provider in guard.iter() {
                core.register_prompt_section_provider(provider.clone());
            }
        }
        let id = core.session_id().to_string();
        cores.insert(id.clone(), core);
        (id, true) // 新创建
    }

    /// 向指定会话的 core 发送消息
    pub fn send_to_core(&self, session_id: &str, content: String) -> bool {
        self.send_to_core_with_id(session_id, content, None)
    }

    /// 向指定会话的 core 发送带固定消息 ID 的消息
    pub fn send_to_core_with_id(
        &self,
        session_id: &str,
        content: String,
        message_id: Option<String>,
    ) -> bool {
        let mut cores = self.lock_cores();
        if let Some(core) = cores.get(session_id) {
            let sent = if let Some(message_id) = message_id {
                core.send_message_with_id(content, message_id, Vec::new())
            } else {
                core.send_message(content)
            };
            if !sent {
                warn!(session_id, "TiangongCore 命令通道已关闭，移除僵尸 core");
                cores.remove(session_id);
            }
            sent
        } else {
            false
        }
    }

    /// 取回 core 的 session（消费 core，用于持久化或切换会话）
    pub fn take_core(&self, session_id: &str) -> Option<TiangongCore> {
        let mut cores = self.lock_cores();
        cores.remove(session_id)
    }

    /// 取消指定会话的执行
    pub fn cancel_core(&self, session_id: &str) {
        let cores = self.lock_cores();
        if let Some(core) = cores.get(session_id) {
            core.cancel();
        }
    }

    /// 取消指定会话中某个 Agent 的当前执行
    pub fn cancel_agent_core(&self, session_id: &str, role: String) -> bool {
        let cores = self.lock_cores();
        cores
            .get(session_id)
            .map(|core| core.cancel_agent(role))
            .unwrap_or(false)
    }

    /// 向指定会话的 core 发送审批响应
    pub fn respond_approval_to_core(&self, session_id: &str, request_id: String, approved: bool) {
        let cores = self.lock_cores();
        if let Some(core) = cores.get(session_id) {
            core.respond_approval(request_id, approved);
        }
    }

    /// 设置所有活跃 core 的信任模式（全局生效）
    pub fn set_all_cores_trust_mode(&self, mode: tiangong_core::permission::TrustMode) {
        let cores = self.lock_cores();
        for core in cores.values() {
            core.set_trust_mode(mode);
        }
    }

    /// 设置指定会话 core 的信任模式（实时生效）
    pub fn set_core_trust_mode(
        &self,
        session_id: &str,
        mode: tiangong_core::permission::TrustMode,
    ) {
        let cores = self.lock_cores();
        if let Some(core) = cores.get(session_id) {
            core.set_trust_mode(mode);
        }
    }

    /// 检查 session 是否有活跃 core
    pub fn is_session_executing(&self, session_id: &str) -> bool {
        let cores = self.lock_cores();
        cores.contains_key(session_id)
    }

    /// 手动触发上下文压缩
    pub fn compress_context_core(&self, session_id: &str) -> bool {
        let cores = self.lock_cores();
        cores
            .get(session_id)
            .map(|core| core.compress_context())
            .unwrap_or(false)
    }

    /// 清理上下文（重置 LLM 上下文到初始 system prompt）
    pub fn reset_context_core(&self, session_id: &str) -> bool {
        let cores = self.lock_cores();
        cores
            .get(session_id)
            .map(|core| core.reset_context())
            .unwrap_or(false)
    }

    /// 启动嵌入式 Server（共享 app 的 state 和 config）
    pub fn start_embedded_server(
        &self,
        host: &str,
        port: u16,
        token: Option<String>,
    ) -> Result<(), String> {
        let mut guard = self.lock_embedded_server();
        if guard.is_some() {
            return Err("Server 已在运行".to_string());
        }
        let handle = tiangong_server::run_embedded(
            host,
            port,
            token,
            self.state.clone(),
            self.config.clone(),
        )
        .map_err(|e| e.to_string())?;
        *guard = Some(handle);
        Ok(())
    }

    /// 停止嵌入式 Server
    pub fn stop_embedded_server(&self) -> Result<(), String> {
        let mut guard = self.lock_embedded_server();
        if let Some(mut handle) = guard.take() {
            handle.stop();
            Ok(())
        } else {
            Err("Server 未在运行".to_string())
        }
    }

    /// 检查嵌入式 Server 是否在运行
    pub fn is_embedded_server_running(&self) -> bool {
        let guard = self.lock_embedded_server();
        guard.is_some()
    }

    /// 创建调度器执行上下文（用于 Desktop 端独立执行定时任务）
    pub fn create_scheduler_context(
        &self,
    ) -> std::sync::Arc<dyn tiangong_scheduler::executor::SchedulerContext> {
        std::sync::Arc::new(crate::scheduler::DesktopSchedulerContext::new(
            self.state.clone(),
            self.config.clone(),
        ))
    }
}
