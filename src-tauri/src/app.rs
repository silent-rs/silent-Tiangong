use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use tauri::Manager;

use tiangong_core::agent_input::{AgentInput, AgentInputKind};
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
    pub state: std::sync::Arc<AsyncMutex<tiangong_app_state::app_state::TiangongState>>,
    pub cores: Mutex<HashMap<String, TiangongCore>>,
    pub config: CoreConfigProvider,
    /// 插件集合变化（能力新增/删除）时标记的 session，下次 ensure_core 移除旧 core 重建。
    plugin_dirty_sessions: Mutex<HashSet<String>>,
    /// Skill 管理插件句柄（dual-ownership：core 拿 clone 做 LLM 工具，
    /// app 持有此句柄做 skill 管理：remove/set_enabled/refresh/gc/doctor）。
    pub skill_plugin: std::sync::Arc<tiangong_plugin_skill::SkillPlugin>,
    /// MCP 管理插件句柄（dual-ownership：core 拿 clone 做 LLM 工具（动态 MCP 工具
    /// spec + 执行分发），app 持有此句柄做 MCP 管理：register/update/remove/
    /// set_enabled/probe/health）。
    pub mcp_plugin: std::sync::Arc<tiangong_plugin_mcp::McpPlugin>,
    embedded_server: Mutex<Option<tiangong_server::EmbeddedServerHandle>>,
    /// Tauri 应用句柄（browser/terminal 插件构造需要）。
    ///
    /// 由 setup 阶段经 [`Self::set_app_handle`] 注入（builder 链构造时尚无 handle）。
    /// 每次 [`Self::ensure_core`] 创建 Core 时，用此句柄现场构造全部插件实例。
    app_handle: std::sync::OnceLock<tauri::AppHandle>,
    /// 工具消息注入通道（插件作为生产者 push，app 消费者统一处理）。
    /// 插件通过 [`Self::tool_injection_tx`] 获取 sender，直接 push `ToolInjection`。
    /// 消费者任务由 [`Self::start_tool_injection_consumer`] 启动。
    tool_injection_tx: tokio::sync::mpsc::UnboundedSender<ToolInjection>,
    /// 消费者 receiver（Option：take 出来启动消费者任务后变 None）。
    tool_injection_rx: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<ToolInjection>>>,
}

/// 工具消息注入请求（插件 → app 消费者）。
pub struct ToolInjection {
    /// 注入到哪个 session（None = 当前活跃 session）。
    pub session_id: Option<String>,
    /// 注入的工具数据。
    pub tool: Box<dyn tiangong_core::agent_input::ToolInput>,
    /// 注入后是否需要刷新前端（emit run_snapshot）。
    pub refresh_frontend: bool,
}

impl TiangongApp {
    /// 构造应用状态。`app_handle` 由 setup 阶段经 [`Self::set_app_handle`] 注入。
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        // 初始化 config 内存单例（从磁盘加载一次，后续读内存）。
        tiangong_config::registry::init();
        let core_config = tiangong_config::registry::config().to_core_config();
        let config = CoreConfigProvider::new(core_config);

        let (tool_injection_tx, tool_injection_rx) = tokio::sync::mpsc::unbounded_channel();

        // 构造 state：load_or_default 经 RuntimeEngine::new 注入 storage_root 到 core
        //（core 运行时持久化需要）。config 加载走自己的 dir，不依赖 core cell。
        let storage_root = tiangong_app_state::app_state::storage_root();
        let state = std::sync::Arc::new(AsyncMutex::new(
            tiangong_app_state::app_state::TiangongState::load_or_default(),
        ));

        Self {
            state,
            cores: Mutex::new(HashMap::new()),
            config,
            plugin_dirty_sessions: Mutex::new(HashSet::new()),
            skill_plugin: std::sync::Arc::new(
                tiangong_plugin_skill::SkillPlugin::with_storage_root(storage_root.join("skills")),
            ),
            mcp_plugin: std::sync::Arc::new(tiangong_plugin_mcp::McpPlugin::with_storage_root(
                storage_root,
            )),
            embedded_server: Mutex::new(None),
            app_handle: std::sync::OnceLock::new(),
            tool_injection_tx,
            tool_injection_rx: Mutex::new(Some(tool_injection_rx)),
        }
    }

    /// 注入 Tauri 应用句柄（setup 阶段调用，仅一次）。
    pub fn set_app_handle(&self, handle: tauri::AppHandle) {
        let _ = self.app_handle.set(handle);
    }

    /// 获取工具消息注入 channel sender。
    ///
    /// 插件持有此 sender 后可直接投递 `ToolInjection`，无需经过 emit/listen 事件中转。
    /// 消费者任务由 [`Self::start_tool_injection_consumer`] 启动后统一处理。
    pub fn tool_injection_tx(&self) -> tokio::sync::mpsc::UnboundedSender<ToolInjection> {
        self.tool_injection_tx.clone()
    }

    /// 启动工具消息注入消费者任务（main.rs setup 阶段调用一次）。
    ///
    /// 循环接收插件 push 的 `ToolInjection`，统一处理注入到 session。
    /// 注入逻辑与 [`Self::inject_tool`] 相同，但支持指定 session_id 和前端刷新。
    pub fn start_tool_injection_consumer(&self, app_handle: tauri::AppHandle) {
        let rx = {
            let mut guard = self.tool_injection_rx.lock().unwrap();
            guard.take()
        };
        let Some(mut rx) = rx else {
            tracing::warn!("工具消息注入消费者已启动，跳过重复启动");
            return;
        };

        // 持有 Arc<state> 让消费者任务独立存活
        let state = self.state.clone();
        tauri::async_runtime::spawn(async move {
            while let Some(req) = rx.recv().await {
                let session_id = match req.session_id {
                    Some(id) => id,
                    None => {
                        let guard = state.lock().await;
                        guard.active_session_id().to_string()
                    }
                };

                let tool_name = req.tool.tool_name().to_string();

                // 通过 app_handle 获取 TiangongApp
                let app_state = app_handle.state::<TiangongApp>();

                // core 不存在 → 自动恢复（ensure_core），保证 stream_tx 可用
                let core_exists = {
                    let cores = app_state.lock_cores();
                    cores.get(&session_id).is_some()
                };
                if !core_exists {
                    // 从 state 取 session 快照
                    let session_snapshot = {
                        let guard = state.lock().await;
                        guard
                            .sessions()
                            .iter()
                            .find(|s| s.id == session_id)
                            .cloned()
                    };
                    if let Some(session) = session_snapshot {
                        use std::sync::mpsc;
                        use tiangong_types::SessionStreamEvent;
                        let (stream_tx, stream_rx) = mpsc::channel::<SessionStreamEvent>();
                        let (_sid, _is_new) =
                            app_state.ensure_core(&session_id, session, stream_tx).await;
                        // 启动 stream_consumer 同步 worker session → TiangongState session
                        let cancel_flag = {
                            let cores = app_state.lock_cores();
                            cores.get(&session_id).map(|c| c.cancel_flag())
                        };
                        if let Some(cancel_flag) = cancel_flag {
                            crate::commands::start_stream_consumer(
                                app_handle.clone(),
                                stream_rx,
                                cancel_flag,
                            );
                        }
                        tracing::info!(session_id, "消费者自动恢复 core");
                    } else {
                        tracing::warn!(session_id, "消费者无法恢复 core：session 不存在");
                        continue;
                    }
                }

                // core 存在 → deliver(Tool)，worker 通过 StreamEvent 处理注入
                let core_sent = {
                    let cores = app_state.lock_cores();
                    if let Some(core) = cores.get(&session_id) {
                        use tiangong_core::agent_input::{AgentInput, AgentInputKind};
                        let sent = core.deliver(AgentInputKind::Tool(req.tool)).is_ok();
                        drop(cores);
                        sent
                    } else {
                        false
                    }
                };

                if !core_sent {
                    tracing::warn!(session_id, tool_name, "deliver 失败（core 通道已关闭）");
                }

                tracing::debug!(session_id, tool_name, "工具消息注入完成");
            }
            tracing::info!("工具消息注入消费者任务结束");
        });
    }

    /// 同步工具消息注入（供需要同步返回值的场景，如 browser:events 的 ack 判断）。
    ///
    /// core 不存在时返回 false。大多数场景应通过 [`Self::tool_injection_tx`] push 到 channel，
    /// 消费者会自动 ensure_core 恢复 core 后注入。
    pub async fn inject_tool(&self, tool: Box<dyn tiangong_core::agent_input::ToolInput>) -> bool {
        let tool_name = tool.tool_name().to_string();
        let session_id = {
            let guard = self.state.lock().await;
            guard.active_session_id().to_string()
        };

        let cores = self.lock_cores();
        if let Some(core) = cores.get(&session_id) {
            use tiangong_core::agent_input::{AgentInput, AgentInputKind};
            tracing::info!(
                session_id,
                tool_name,
                stopped = core.is_stopped(),
                "注入工具消息 via deliver"
            );
            core.deliver(AgentInputKind::Tool(tool)).is_ok()
        } else {
            tracing::warn!(
                session_id,
                tool_name,
                "inject_tool: core 不存在，返回 false（应走 channel 消费者自动恢复）"
            );
            false
        }
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
        // old_sig 从 registry 旧值算（set_models 之前），new_sig 从 app-state 新值算。
        let old_sig =
            tiangong_config::registry::plugin_set_signature(&tiangong_config::registry::models());
        let (next, new_sig) = self
            .with_state_read(|core_state| {
                let new_models = core_state.models_config().clone();
                let new_sig = tiangong_config::registry::plugin_set_signature(&new_models);
                // 同步 app-state 的最新 models 到 config 内存单例。
                tiangong_config::registry::set_models(new_models);
                Ok((core_state.build_core_config_from_base(&base), new_sig))
            })
            .await?;
        let plugin_set_changed = old_sig != new_sig;
        self.config.replace(next);
        let cores = self.lock_cores();
        if plugin_set_changed {
            // 能力集合变化（新增/删除）：plugin 列表构造时固定，无法热更新。
            // 标记 dirty，下次 ensure_core 时移除旧 core 重建（不打断当前 turn）。
            for session_id in cores.keys().cloned().collect::<Vec<_>>() {
                self.plugin_dirty_sessions
                    .lock()
                    .map(|mut g| g.insert(session_id))
                    .ok();
            }
        } else {
            // 仅 endpoint 变化：reload_config + on_config_updated 热更新端点。
            for core in cores.values() {
                let _ = core.deliver(AgentInputKind::reload_config());
            }
        }
        Ok(())
    }

    pub async fn with_state<F, R>(&self, f: F) -> Result<R, String>
    where
        F: FnOnce(&mut tiangong_app_state::app_state::TiangongState) -> Result<R, anyhow::Error>,
    {
        let mut guard = self.state.lock().await;
        f(&mut guard).map_err(|e| e.to_string())
    }

    pub async fn with_state_read<F, R>(&self, f: F) -> Result<R, String>
    where
        F: FnOnce(&tiangong_app_state::app_state::TiangongState) -> Result<R, anyhow::Error>,
    {
        let guard = self.state.lock().await;
        f(&guard).map_err(|e| e.to_string())
    }

    /// 获取或创建会话对应的 TiangongCore
    ///
    /// 如果 core 已存在（多轮对话），直接复用。
    /// stream_tx 只在创建新 core 时使用。
    pub async fn ensure_core(
        &self,
        session_id: &str,
        session: tiangong_core::session::Session,
        stream_tx: std::sync::mpsc::Sender<tiangong_types::SessionStreamEvent>,
    ) -> (String, bool) {
        // Invariant: 无效 CWD 的会话不会加载到 Core 生命周期。调用方在加载会话前
        // 已过滤掉 cwd 为无效目录的会话，因此插件可以假设 session.cwd 要么为空
        //（普通聊天会话）要么是有效工作区目录。
        //
        // Invariant: 同一 session 的 ensure_core 调用由上层业务串行化（Tauri 命令
        // 经 session 级互斥 / 前端单消息流保证），不会并发为同一 session 创建 Core。
        // 因此这里可以在 await 初始化 memory handle 后直接创建并插入。
        // 如未来允许同 session 并发入口，需要在 await 后增加二次检查。

        // 1. 先检查是否已有 core（持有锁期间不做 async 操作）
        {
            let mut cores = self.lock_cores();
            // 插件集合变化（能力新增/删除）时移除旧 core，用最新 models 重建。
            let dirty = self
                .plugin_dirty_sessions
                .lock()
                .map(|mut g| g.remove(session_id))
                .unwrap_or(false);
            if dirty {
                if cores.remove(session_id).is_some() {
                    tracing::info!(session_id, "插件集合变化，移除旧 core 待重建");
                }
            } else if let Some(core) = cores.get(session_id) {
                if !core.is_stopped() {
                    let _ = core.deliver(AgentInputKind::reload_config());
                    let _ = core.deliver(AgentInputKind::update_cwd(session.cwd.clone()));
                    core.set_trust_mode(session.trust_mode);
                    return (session_id.to_string(), false); // 已存在，复用
                }
                warn!(session_id, "移除已停止的 TiangongCore");
                cores.remove(session_id);
            }
        }

        // 2. 初始化 Memory Handle（async，不持有 cores 锁）。
        let memory_handle = tiangong_memory::registry::init_memory_handle_for_process(
            self.config.generation(),
            tiangong_memory::ProcessType::Gui,
        )
        .await;

        // 3. 现场构造全部插件实例（per-Core 独立，隔离 per-session 状态）。
        let mut plugins: Vec<std::sync::Arc<dyn tiangong_core::core::Plugin>> = Vec::new();
        let Some(app_handle) = self.app_handle.get() else {
            panic!("TiangongApp.app_handle 未注入，set_app_handle 应在 setup 阶段调用");
        };
        if let Some(browser) = tiangong_plugin_browser::build_plugin(app_handle) {
            plugins.push(browser);
        } else {
            warn!("浏览器插件构造失败（Tauri state 未就绪），浏览器能力将缺失");
        }
        if let Some(terminal) = tiangong_plugin_terminal::build_plugin(app_handle) {
            plugins.push(terminal);
        } else {
            warn!("终端插件构造失败（Tauri state 未就绪），终端能力将缺失");
        }
        plugins.push(tiangong_plugin_fs::build_plugin());
        plugins.push(tiangong_plugin_media_archive::build_plugin());
        plugins.push(tiangong_plugin_index::build_plugin());
        // app 层判断是否注册各能力插件，经 llm 路由解析端点后构造注入。
        // models 从 config 内存单例读取（sync_core_config_from_state 时已同步）。
        use tiangong_llm::{ModelCapability, ModelEndpoint, SingleProviderClient};
        let models = tiangong_config::registry::models();
        let resolve_ep = |cap: ModelCapability| {
            models
                .resolve_for_capability(cap)
                .map(ModelEndpoint::from_resolved)
        };
        if let Some(ep) = resolve_ep(ModelCapability::ImageGeneration) {
            plugins.push(tiangong_plugin_generate_image::build_plugin(ep));
        }
        if let Some(ep) = resolve_ep(ModelCapability::VideoGeneration) {
            plugins.push(tiangong_plugin_generate_video::build_plugin(ep));
        }
        if let Some(ep) = resolve_ep(ModelCapability::Tts) {
            plugins.push(tiangong_plugin_text_to_speech::build_plugin(ep));
        }
        if let Some(ep) = resolve_ep(ModelCapability::Stt) {
            plugins.push(tiangong_plugin_speech_to_text::build_plugin(ep));
        }
        plugins.push(tiangong_plugin_memory::build_plugin(memory_handle));
        plugins.push(tiangong_plugin_scheduler::build_plugin());
        plugins.push(tiangong_plugin_task::build_plugin());
        if models.has_capability(ModelCapability::Multimodal) && !models.chat_is_multimodal() {
            if let Some(client) =
                resolve_ep(ModelCapability::Multimodal).map(SingleProviderClient::new)
            {
                plugins.push(tiangong_plugin_analyze_attachment::build_plugin(client));
            }
        }
        // Skill 插件：dual-ownership——core 拿 clone 做 LLM 工具（get_skill_detail），
        // app 侧经 self.skill_plugin 做管理（remove/set_enabled/refresh/gc/doctor）。
        plugins.push(self.skill_plugin.clone());
        // MCP 插件：dual-ownership——core 拿 clone 做 LLM 工具（动态 MCP 工具），
        // app 侧经 self.mcp_plugin 做管理（register/update/remove/set_enabled/probe）。
        plugins.push(self.mcp_plugin.clone());

        // 4. 创建 Core 并插入（重新拿锁）。
        let core = TiangongCore::builder()
            .config(self.config.clone())
            .session(session)
            .event_sender(stream_tx)
            .plugins(plugins)
            .storage(tiangong_core::core::CoreStorageLocation::new(
                tiangong_app_state::app_state::storage_root(),
            ))
            .build()
            .expect("Builder 必填字段已齐");
        let id = core.session_id().to_string();
        {
            let mut cores = self.lock_cores();
            cores.insert(id.clone(), core);
        }
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
                core.deliver(AgentInputKind::message_with_id(
                    content,
                    message_id,
                    Vec::new(),
                ))
                .is_ok()
            } else {
                core.deliver(AgentInputKind::message(content)).is_ok()
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
            let _ = core.deliver(AgentInputKind::cancel());
        }
    }

    /// 取消指定会话中某个 Agent 的当前执行
    pub fn cancel_agent_core(&self, session_id: &str, role: String) -> bool {
        let cores = self.lock_cores();
        cores
            .get(session_id)
            .map(|core| core.deliver(AgentInputKind::cancel_agent(role)).is_ok())
            .unwrap_or(false)
    }

    /// 向指定会话的 core 发送审批响应
    pub fn respond_approval_to_core(&self, session_id: &str, request_id: String, approved: bool) {
        let cores = self.lock_cores();
        if let Some(core) = cores.get(session_id) {
            let _ = core.deliver(AgentInputKind::approval(request_id, approved));
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
            .map(|core| core.deliver(AgentInputKind::compress_context()).is_ok())
            .unwrap_or(false)
    }

    /// 清理上下文（重置 LLM 上下文到初始 system prompt）
    pub fn reset_context_core(&self, session_id: &str) -> bool {
        let cores = self.lock_cores();
        cores
            .get(session_id)
            .map(|core| core.deliver(AgentInputKind::reset_context()).is_ok())
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
