use std::collections::HashMap;
use std::sync::Mutex;

use tauri::Manager;

use tiangong_config::load_tiangong_config;
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
    pub state: std::sync::Arc<AsyncMutex<tiangong_core::app_state::TiangongState>>,
    pub cores: Mutex<HashMap<String, TiangongCore>>,
    pub config: CoreConfigProvider,
    embedded_server: Mutex<Option<tiangong_server::EmbeddedServerHandle>>,
    /// 进程内插件集合（issue #156 自注册架构）。
    ///
    /// 每个插件封装自己的全部能力，在 [`Self::ensure_core`] 创建 core 时传入，
    /// 由 worker_loop 在 engine 创建/重建时遍历调用 `Plugin::register`。
    plugins: Mutex<Vec<std::sync::Arc<dyn tiangong_core::core::Plugin>>>,
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

impl Default for TiangongApp {
    fn default() -> Self {
        let app_config = load_tiangong_config();
        let core_config = app_config.to_core_config();
        let config = CoreConfigProvider::new(core_config);

        let (tool_injection_tx, tool_injection_rx) = tokio::sync::mpsc::unbounded_channel();

        Self {
            state: std::sync::Arc::new(AsyncMutex::new(
                tiangong_core::app_state::TiangongState::load_or_default(),
            )),
            cores: Mutex::new(HashMap::new()),
            config,
            embedded_server: Mutex::new(None),
            plugins: Mutex::new(Vec::new()),
            tool_injection_tx,
            tool_injection_rx: Mutex::new(Some(tool_injection_rx)),
        }
    }
}

impl TiangongApp {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册进程内插件（issue #156 自注册架构）。
    ///
    /// 由 main.rs setup 阶段调用：插件在 engine 创建/重建时自行向 engine 注册
    /// 全部能力（页面获取、终端能力、工具覆盖、Prompt 段落等），替代旧的
    /// `set_page_fetcher` / `register_tool_override` 等逐字段注册。
    pub fn register_plugin(&self, plugin: std::sync::Arc<dyn tiangong_core::core::Plugin>) {
        if let Ok(mut guard) = self.plugins.lock() {
            guard.push(plugin);
        }
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
                            app_state.ensure_core(&session_id, session, stream_tx);
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
                        let sent = core.deliver(AgentInputKind::Tool(req.tool));
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
                running = core.is_running(),
                "注入工具消息 via deliver"
            );
            core.deliver(AgentInputKind::Tool(tool))
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
        let next = self
            .with_state_read(|core_state| Ok(core_state.build_core_config_from_base(&base)))
            .await?;
        self.config.replace(next);
        if let Ok(cores) = self.cores.lock() {
            for core in cores.values() {
                let _ = core.deliver(AgentInputKind::reload_config());
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
                let _ = core.deliver(AgentInputKind::reload_config());
                let _ = core.deliver(AgentInputKind::update_cwd(session.cwd.clone()));
                core.set_trust_mode(session.trust_mode);
                return (session_id.to_string(), false); // 已存在，复用
            }
            warn!(session_id, "移除已停止的 TiangongCore");
            cores.remove(session_id);
        }
        let plugins = self
            .plugins
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let core =
            TiangongCore::with_session_for_gui(self.config.clone(), session, stream_tx, plugins);
        // plugins 已随构造传入 worker_loop，在 engine 创建/重建时遍历调用
        // Plugin::register 完成能力注入（issue #156），无需在此逐字段灌入。
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
                core.deliver(AgentInputKind::message_with_id(
                    content,
                    message_id,
                    Vec::new(),
                ))
            } else {
                core.deliver(AgentInputKind::message(content))
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
            core.deliver(AgentInputKind::cancel());
        }
    }

    /// 取消指定会话中某个 Agent 的当前执行
    pub fn cancel_agent_core(&self, session_id: &str, role: String) -> bool {
        let cores = self.lock_cores();
        cores
            .get(session_id)
            .map(|core| core.deliver(AgentInputKind::cancel_agent(role)))
            .unwrap_or(false)
    }

    /// 向指定会话的 core 发送审批响应
    pub fn respond_approval_to_core(&self, session_id: &str, request_id: String, approved: bool) {
        let cores = self.lock_cores();
        if let Some(core) = cores.get(session_id) {
            core.deliver(AgentInputKind::approval(request_id, approved));
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
            .map(|core| core.deliver(AgentInputKind::compress_context()))
            .unwrap_or(false)
    }

    /// 清理上下文（重置 LLM 上下文到初始 system prompt）
    pub fn reset_context_core(&self, session_id: &str) -> bool {
        let cores = self.lock_cores();
        cores
            .get(session_id)
            .map(|core| core.deliver(AgentInputKind::reset_context()))
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
