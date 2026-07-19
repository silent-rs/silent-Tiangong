use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use tauri::Manager;

use tiangong_core::agent_input::{AgentInput, AgentInputKind};
use tiangong_core::core::TiangongCore;
use tiangong_core::core_config::CoreConfigProvider;
use tokio::sync::Mutex as AsyncMutex;
use tracing::warn;

type RemoteTurnResult = Result<tiangong_types::OutgoingMessage, String>;
type RemoteTurnWaiter = tokio::sync::oneshot::Sender<RemoteTurnResult>;

/// 天工应用状态
///
/// state: 应用管理（会话列表、配置、持久化）— Arc<tokio Mutex> 以支持嵌入式 server 共享
/// cores: 活跃的对话核心（session_id → TiangongCore）
/// config: 共享配置提供者
/// embedded_server: 嵌入式 Server 句柄（Desktop 模式下 Server 运行在 app 进程内）
pub struct TiangongApp {
    pub state: std::sync::Arc<AsyncMutex<tiangong_app_state::app_state::TiangongState>>,
    /// 子 Agent 的过程消息仅供桌面端视图展示，不能进入父 Session 权威状态。
    ///
    /// 按父 Session 保存；构建 RunSnapshot 时合并到消息副本。这样 Core 重建、
    /// 编辑重发和 Session 持久化永远不会读到这些临时 worker 消息。
    agent_worker_views: Mutex<HashMap<String, Vec<tiangong_types::Message>>>,
    /// 覆盖单个会话从附件准备到 Core 持久化确认的完整串行区间。
    /// 不同会话使用不同锁，可以并行发送；同一会话不会并发创建 Core 或抢占草稿。
    session_send_locks: Mutex<HashMap<String, std::sync::Arc<AsyncMutex<()>>>>,
    /// 草稿归档/持久化串行锁。与发送锁分离，使用户等待发送期间的新输入能立即写入
    /// 新 revision，而不会等旧发送结束后才落盘。
    draft_update_locks: Mutex<HashMap<String, std::sync::Arc<AsyncMutex<()>>>>,
    /// 临时草稿 ID 转正后的运行时重定向。迟到的旧 ID 写入会跟随到
    /// 真实会话，避免迁移后又复活一份孤立草稿。
    draft_redirects: Mutex<HashMap<String, String>>,
    /// 已成功投递的最新草稿 revision，用于防止双击或迟到请求重复发送。
    delivered_draft_revisions: Mutex<HashMap<String, u64>>,
    /// 前端冻结发送快照后、真正进入 send_message 前的附件租约。
    /// 该租约让草稿清理不会删掉已被本次发送冻结的归档路径。
    draft_send_claims: Mutex<HashMap<String, DraftSendClaim>>,
    /// 当前进程已明确丢弃/删除的草稿 ID，阻止早已发出的迟到写入复活。
    discarded_drafts: Mutex<HashSet<String>>,
    /// 活动会话变更代数，用于草稿转正后的条件激活。
    active_session_epoch: AtomicU64,
    pub config: CoreConfigProvider,
    scheduler_context: std::sync::Arc<crate::scheduler::DesktopSchedulerContext>,
    /// Desktop 定时消息消费者。调度上下文只持 sender；receiver 由 setup 取出后，
    /// 把所有定时投递收敛到本应用的统一 Core 映射。
    scheduled_message_rx: Mutex<
        Option<tokio::sync::mpsc::UnboundedReceiver<crate::scheduler::ScheduledMessageRequest>>,
    >,
    /// Skill 管理插件句柄（dual-ownership：core 拿 clone 做 LLM 工具，
    /// app 持有此句柄做 skill 管理：remove/set_enabled/refresh/gc/doctor）。
    pub skill_plugin: std::sync::Arc<tiangong_plugin_skill::SkillPlugin>,
    /// MCP 管理插件句柄（dual-ownership：core 拿 clone 做 LLM 工具（动态 MCP 工具
    /// spec + 执行分发），app 持有此句柄做 MCP 管理：register/update/remove/
    /// set_enabled/probe/health）。
    pub mcp_plugin: std::sync::Arc<tiangong_plugin_mcp::McpPlugin>,
    /// 内嵌 HTTP 的等待者按稳定用户消息 ID 绑定。终态由唯一的桌面流消费者完成，
    /// 不能按“当前最后一轮”唤醒，否则同会话排队消息会串答。
    remote_turn_waiters: Mutex<HashMap<(String, String), Vec<RemoteTurnWaiter>>>,
    /// 内嵌 HTTP 等待型轮次的会话所有权。watch 同时提供无丢唤醒的释放通知，
    /// 让同会话远端请求串行到终态，调度消息也不会插入正在等待的远端轮次。
    remote_turn_states: Mutex<HashMap<String, tokio::sync::watch::Sender<Option<String>>>>,
    embedded_server: Mutex<Option<tiangong_server::EmbeddedServerHandle>>,
    /// Tauri 应用句柄（browser/terminal 插件构造需要）。
    ///
    /// 由 setup 阶段经 [`Self::set_app_handle`] 注入（builder 链构造时尚无 handle）。
    /// 每次 `ensure_core` 创建 Core 时，经 [`crate::core_factory::DesktopCoreFactory`]
    /// 用此句柄现场构造全部插件实例。`Arc` 包裹以便与 factory 共享同一 cell。
    app_handle: std::sync::Arc<std::sync::OnceLock<tauri::AppHandle>>,
    /// 桌面端 Core 构造依赖（issue #245）：持有 app_handle/skill/mcp/config/
    /// storage_root,提供 `build_plugins()` 供 `ensure_core` 前构造插件集合。
    pub desktop_factory: std::sync::Arc<crate::core_factory::DesktopCoreFactory>,
    /// CoreManager 句柄（与 `TiangongState.core_manager` 共享同一注册表实例）。
    ///
    /// 真相源归 `TiangongState.core_manager`(根字段);这里持有 clone 仅供
    /// `TiangongApp` 同步方法(has_live_core / deliver_* / cancel_core 等)访问,
    /// 避免每个同步调用都 lock AsyncMutex<TiangongState>。
    pub core_manager: tiangong_core_manager::CoreManager,
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

#[derive(Debug, Clone)]
struct DraftSendClaim {
    revision: u64,
    attachment_paths: Vec<String>,
}

pub(crate) struct EnsuredCore {
    pub(crate) session_id: String,
    pub(crate) is_new: bool,
}

fn merge_agent_output_messages(
    view: &mut Vec<tiangong_types::Message>,
    agent_id: &str,
    agent_role: &str,
    agent_label: &str,
    messages: &[tiangong_types::Message],
) {
    let worker_id = format!("agent:{agent_role}:{agent_id}");
    let header_id = format!("agent:{agent_id}:header");
    if !view.iter().any(|message| {
        message.id == header_id && message.worker_id.as_deref() == Some(worker_id.as_str())
    }) {
        let mut header = tiangong_core::session::Message::new(
            tiangong_core::session::MessageRole::System,
            format!("🔧 Worker: {agent_label} (@{agent_role})"),
        );
        header.id = header_id;
        header.worker_id = Some(worker_id.clone());
        header.model_excluded = true;
        view.push(header);
    }

    for message in messages {
        let role = match message.role {
            tiangong_core::session::MessageRole::Assistant => {
                tiangong_core::session::MessageRole::Assistant
            }
            tiangong_core::session::MessageRole::System
            | tiangong_core::session::MessageRole::Tool => {
                tiangong_core::session::MessageRole::System
            }
            tiangong_core::session::MessageRole::User => tiangong_core::session::MessageRole::User,
        };

        if let Some(existing) = view.iter_mut().find(|item| {
            item.id == message.id && item.worker_id.as_deref() == Some(worker_id.as_str())
        }) {
            if role == tiangong_core::session::MessageRole::Assistant {
                existing.content.extend(message.content.iter().cloned());
                existing
                    .reasoning_content
                    .push_str(&message.reasoning_content);
                existing.phase = message.phase;
            }
            continue;
        }

        let mut worker_message = message.clone();
        worker_message.role = role;
        worker_message.worker_id = Some(worker_id.clone());
        // 即使未来误把缓存副本传入 Core，也不得进入父 Agent 上下文。
        worker_message.model_excluded = true;
        view.push(worker_message);
    }
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
        let (scheduled_message_tx, scheduled_message_rx) = tokio::sync::mpsc::unbounded_channel();

        //（core 运行时持久化需要）。config 加载走自己的 dir，不依赖 core cell。
        let storage_root = tiangong_app_state::app_state::storage_root();
        let state = std::sync::Arc::new(AsyncMutex::new(
            tiangong_app_state::app_state::TiangongState::load_or_default(),
        ));
        let scheduler_context = std::sync::Arc::new(
            crate::scheduler::DesktopSchedulerContext::new(state.clone(), scheduled_message_tx),
        );

        let app_handle = std::sync::Arc::new(std::sync::OnceLock::new());
        let skill_plugin = std::sync::Arc::new(
            tiangong_plugin_skill::SkillPlugin::with_storage_root(storage_root.join("skills")),
        );
        let mcp_plugin = std::sync::Arc::new(tiangong_plugin_mcp::McpPlugin::with_storage_root(
            storage_root.clone(),
        ));
        let desktop_factory = std::sync::Arc::new(crate::core_factory::DesktopCoreFactory {
            app_handle: app_handle.clone(),
            skill_plugin: skill_plugin.clone(),
            mcp_plugin: mcp_plugin.clone(),
            config: config.clone(),
            storage_root: storage_root.clone(),
        });
        // 注入 CoreManager 到 TiangongState 根字段（issue #245）。
        // 同时 clone 一份给 TiangongApp 供同步方法访问(共享同一 Arc 注册表)。
        // 用 try_lock 避免在 async 上下文(如 tokio 测试)中调用 blocking_lock 导致 panic;
        // new() 在构造期单线程调用,try_lock 必然成功。
        let core_manager = {
            let guard = state
                .try_lock()
                .expect("CoreManager 注入期 state 锁不应被占用");
            guard
                .install_core_manager(config.clone(), storage_root.clone())
                .clone()
        };

        Self {
            state,
            agent_worker_views: Mutex::new(HashMap::new()),
            session_send_locks: Mutex::new(HashMap::new()),
            draft_update_locks: Mutex::new(HashMap::new()),
            draft_redirects: Mutex::new(HashMap::new()),
            delivered_draft_revisions: Mutex::new(HashMap::new()),
            draft_send_claims: Mutex::new(HashMap::new()),
            discarded_drafts: Mutex::new(HashSet::new()),
            active_session_epoch: AtomicU64::new(0),
            config,
            scheduler_context,
            scheduled_message_rx: Mutex::new(Some(scheduled_message_rx)),
            skill_plugin,
            mcp_plugin,
            remote_turn_waiters: Mutex::new(HashMap::new()),
            remote_turn_states: Mutex::new(HashMap::new()),
            embedded_server: Mutex::new(None),
            app_handle,
            desktop_factory,
            core_manager,
            tool_injection_tx,
            tool_injection_rx: Mutex::new(Some(tool_injection_rx)),
        }
    }

    pub(crate) fn merge_agent_output_view(
        &self,
        session_id: &str,
        agent_id: &str,
        agent_role: &str,
        agent_label: &str,
        messages: &[tiangong_types::Message],
    ) {
        let mut views = self
            .agent_worker_views
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let view = views.entry(session_id.to_string()).or_default();
        merge_agent_output_messages(view, agent_id, agent_role, agent_label, messages);
    }

    pub(crate) fn agent_worker_view_messages(
        &self,
        session_id: &str,
    ) -> Vec<tiangong_types::Message> {
        self.agent_worker_views
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(session_id)
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn clear_agent_worker_view(&self, session_id: &str) {
        self.agent_worker_views
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(session_id);
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

    pub(crate) fn register_remote_turn_waiter(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> tokio::sync::oneshot::Receiver<RemoteTurnResult> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.remote_turn_waiters
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .entry((session_id.to_string(), message_id.to_string()))
            .or_default()
            .push(tx);
        rx
    }

    pub(crate) fn complete_remote_turn_waiters(
        &self,
        session_id: &str,
        message_id: &str,
        result: RemoteTurnResult,
    ) {
        let waiters = self
            .remote_turn_waiters
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&(session_id.to_string(), message_id.to_string()))
            .unwrap_or_default();
        for waiter in waiters {
            let _ = waiter.send(result.clone());
        }
    }

    pub(crate) fn fail_remote_session_waiters(&self, session_id: &str, message: &str) {
        let mut waiters = self
            .remote_turn_waiters
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let keys = waiters
            .keys()
            .filter(|(sid, _)| sid == session_id)
            .cloned()
            .collect::<Vec<_>>();
        let mut removed = Vec::new();
        for key in keys {
            removed.extend(waiters.remove(&key).unwrap_or_default());
        }
        drop(waiters);
        for waiter in removed {
            let _ = waiter.send(Err(message.to_string()));
        }
    }

    fn remote_turn_state_sender(
        &self,
        session_id: &str,
    ) -> tokio::sync::watch::Sender<Option<String>> {
        self.remote_turn_states
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .entry(session_id.to_string())
            .or_insert_with(|| tokio::sync::watch::channel(None).0)
            .clone()
    }

    pub(crate) fn remote_turn_owner(&self, session_id: &str) -> Option<String> {
        self.remote_turn_state_sender(session_id).borrow().clone()
    }

    pub(crate) fn begin_remote_turn(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> Result<(), String> {
        let sender = self.remote_turn_state_sender(session_id);
        if let Some(owner) = sender.borrow().as_ref() {
            return Err(format!("会话已有远程轮次正在执行：{owner}"));
        }
        sender.send_replace(Some(message_id.to_string()));
        Ok(())
    }

    pub(crate) fn finish_remote_turn(&self, session_id: &str, message_id: &str) {
        let sender = self.remote_turn_state_sender(session_id);
        let owns_turn = sender.borrow().as_deref() == Some(message_id);
        if owns_turn {
            sender.send_replace(None);
        }
    }

    pub(crate) fn remote_turn_allows_message(&self, session_id: &str, message_id: &str) -> bool {
        self.remote_turn_owner(session_id)
            .is_none_or(|owner| owner == message_id)
    }

    pub(crate) async fn wait_for_remote_turn_release(&self, session_id: &str) {
        let mut receiver = self.remote_turn_state_sender(session_id).subscribe();
        loop {
            let active = receiver.borrow().is_some();
            if !active {
                return;
            }
            if receiver.changed().await.is_err() {
                return;
            }
        }
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
                // 与发送、编辑和删除共享同一会话边界，覆盖快照读取、ensure、消费者
                // 绑定和最终 deliver，禁止在 take→删除/重建空窗中复活孤立 Core。
                let session_lock = app_state.session_send_lock(&session_id);
                let _send_guard = session_lock.lock_owned().await;
                // 会话存在性用 metadata 判定；ensure_core 需要的完整 session
                // 从磁盘 load（issue #245：真相源归磁盘）。
                let session_exists = state
                    .lock()
                    .await
                    .session_metadata()
                    .iter()
                    .any(|m| m.id == session_id);
                if !session_exists {
                    tracing::warn!(session_id, "消费者无法恢复 core：session 不存在");
                    continue;
                }
                let Ok(session_snapshot) = app_state.core_manager.load_session(&session_id) else {
                    tracing::warn!(session_id, "消费者无法恢复 core：session 磁盘加载失败");
                    continue;
                };
                use std::sync::mpsc;
                let (stream_tx, stream_rx) = mpsc::channel::<tiangong_types::StreamEvent>();
                let ensured = app_state
                    .ensure_core(&session_id, session_snapshot, stream_tx)
                    .await;
                if ensured.is_new {
                    crate::commands::start_stream_consumer(
                        app_handle.clone(),
                        ensured.session_id.clone(),
                        stream_rx,
                    );
                    tracing::info!(session_id, "消费者自动恢复 core");
                }

                let core_sent = app_state
                    .deliver_to_core_if_live(&ensured.session_id, AgentInputKind::Tool(req.tool));

                if !core_sent {
                    tracing::warn!(session_id, tool_name, "deliver 失败（core 通道已关闭）");
                }

                tracing::debug!(session_id, tool_name, "工具消息注入完成");
            }
            tracing::info!("工具消息注入消费者任务结束");
        });
    }

    /// 启动 Desktop 定时消息消费者（main.rs setup 阶段调用一次）。
    ///
    /// 定时消息与前端消息共用 `session_send_lock`、`cores` 和流消费者；这里只等
    /// Core 确认消息稳定持久化，模型轮次继续在既有 Core 中异步执行。
    pub fn start_scheduled_message_consumer(&self, app_handle: tauri::AppHandle) {
        let rx = {
            let mut guard = self
                .scheduled_message_rx
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            guard.take()
        };
        let Some(mut rx) = rx else {
            tracing::warn!("Desktop 定时消息消费者已启动，跳过重复启动");
            return;
        };

        tauri::async_runtime::spawn(async move {
            while let Some(request) = rx.recv().await {
                let request_app = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                    let crate::scheduler::ScheduledMessageRequest {
                        session_id,
                        content,
                        stable_enqueue_ack,
                    } = request;
                    let app_state = request_app.state::<TiangongApp>();
                    let result = app_state
                        .enqueue_scheduled_message(request_app.clone(), session_id, content)
                        .await;
                    let _ = stable_enqueue_ack.send(result);
                });
            }
            tracing::info!("Desktop 定时消息消费者任务结束");
        });
    }

    async fn enqueue_scheduled_message(
        &self,
        app_handle: tauri::AppHandle,
        session_id: String,
        content: String,
    ) -> Result<(), String> {
        use std::sync::mpsc;
        use tiangong_types::ContentBlock;

        if session_id.trim().is_empty() {
            return Err("定时消息目标会话 ID 不能为空".to_string());
        }

        let _send_guard = loop {
            self.wait_for_remote_turn_release(&session_id).await;
            let session_lock = self.session_send_lock(&session_id);
            let guard = session_lock.lock_owned().await;
            if self.remote_turn_owner(&session_id).is_none() {
                break guard;
            }
            drop(guard);
        };
        self.sync_core_config_from_state().await?;
        let message_id = scru128::new().to_string();
        let prepared = vec![ContentBlock::text(content)];
        let stable_prepared = tiangong_types::stable_content_blocks(&prepared);
        let session_snapshot = self
            .with_state(|core_state| {
                core_state.append_prepared_user_message_for_turn(
                    &session_id,
                    &message_id,
                    stable_prepared,
                )
            })
            .await?;

        let (stream_tx, stream_rx) = mpsc::channel::<tiangong_types::StreamEvent>();
        let ensured = self
            .ensure_core(&session_id, session_snapshot, stream_tx)
            .await;
        let sid = ensured.session_id.clone();
        if let Err(error) = self.deliver_prepared_if_live(&sid, message_id.clone(), prepared) {
            let rollback = self
                .rollback_failed_scheduled_message(&session_id, &message_id)
                .await;
            return Err(match rollback {
                Ok(()) => format!("定时消息投递失败：{error}"),
                Err(rollback_error) => {
                    format!("定时消息投递失败：{error}；回滚也失败：{rollback_error}")
                }
            });
        }

        if ensured.is_new {
            crate::commands::start_stream_consumer(app_handle, sid, stream_rx);
        }
        Ok(())
    }

    /// 关闭本次投递绑定的 Core，等待其停止后再回滚宿主消息，防止旧 worker
    /// 的迟到持久化重新写回失败消息。
    async fn rollback_failed_scheduled_message(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> Result<(), String> {
        if let Some(core) = self.take_core(session_id) {
            let sid = session_id.to_string();
            match tokio::task::spawn_blocking(move || core.shutdown_join()).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::warn!(session_id = %sid, %error, "定时消息回滚前关闭 Core 失败");
                }
                Err(error) => {
                    tracing::warn!(session_id = %sid, %error, "定时消息回滚前等待 Core 失败");
                }
            }
        }

        self.with_state(|core_state| {
            core_state.reload_session_from_disk(session_id)?;
            if !core_state
                .session_metadata()
                .iter()
                .any(|m| m.id == session_id)
            {
                return Err(anyhow::anyhow!("定时消息目标会话已不存在：{session_id}"));
            }
            core_state.remove_message_for_failed_turn(session_id, message_id);
            core_state.remove_pending_message_for(session_id, message_id);
            core_state.persist_session_and_app(session_id)
        })
        .await
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
        let session_lock = self.session_send_lock(&session_id);
        let _send_guard = session_lock.lock_owned().await;
        if self.has_live_core(&session_id) {
            tracing::info!(session_id, tool_name, "注入工具消息 via deliver");
            self.deliver_to_core_if_live(&session_id, AgentInputKind::Tool(tool))
        } else {
            tracing::warn!(
                session_id,
                tool_name,
                "inject_tool: core 不存在，返回 false（应走 channel 消费者自动恢复）"
            );
            false
        }
    }

    pub fn session_send_lock(&self, session_id: &str) -> std::sync::Arc<AsyncMutex<()>> {
        let mut locks = match self.session_send_locks.lock() {
            Ok(guard) => guard,
            Err(err) => {
                warn!(error = %err, "session_send_locks 锁已污染，尝试恢复");
                err.into_inner()
            }
        };
        locks
            .entry(session_id.to_string())
            .or_insert_with(|| std::sync::Arc::new(AsyncMutex::new(())))
            .clone()
    }

    pub fn active_session_epoch(&self) -> u64 {
        self.active_session_epoch.load(Ordering::Acquire)
    }

    pub fn mark_active_session_changed(&self) -> u64 {
        self.active_session_epoch.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn remove_session_send_lock(&self, session_id: &str) {
        // 不从锁表删除 Arc：若旧 guard/等待者仍存活，新请求创建另一把锁
        // 会直接破坏互斥。这些小锁保留到进程结束。
        let mut delivered = match self.delivered_draft_revisions.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        delivered.remove(session_id);
        drop(delivered);
        let mut claims = match self.draft_send_claims.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        claims.remove(session_id);
        drop(claims);
        let mut redirects = match self.draft_redirects.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        let discarded_aliases = redirects
            .iter()
            .filter(|(from, to)| from.as_str() == session_id || to.as_str() == session_id)
            .map(|(from, _)| from.clone())
            .collect::<Vec<_>>();
        redirects.retain(|from, to| from != session_id && to != session_id);
        drop(redirects);
        let mut discarded = match self.discarded_drafts.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        discarded.insert(session_id.to_string());
        discarded.extend(discarded_aliases);
    }

    pub fn resolve_draft_session_id(&self, session_id: &str) -> String {
        let redirects = match self.draft_redirects.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        let mut resolved = session_id.to_string();
        let mut seen = HashSet::new();
        while seen.insert(resolved.clone()) {
            let Some(next) = redirects.get(&resolved) else {
                break;
            };
            resolved = next.clone();
        }
        resolved
    }

    pub fn redirect_input_draft(&self, from_session_id: &str, to_session_id: &str) {
        if from_session_id == to_session_id {
            return;
        }
        let resolved_target = self.resolve_draft_session_id(to_session_id);
        let mut redirects = match self.draft_redirects.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        redirects.insert(from_session_id.to_string(), resolved_target.clone());
        for target in redirects.values_mut() {
            if target == from_session_id {
                *target = resolved_target.clone();
            }
        }
        drop(redirects);
        let mut delivered = match self.delivered_draft_revisions.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        if let Some(from_revision) = delivered.remove(from_session_id) {
            delivered
                .entry(resolved_target.clone())
                .and_modify(|current| *current = (*current).max(from_revision))
                .or_insert(from_revision);
        }
        drop(delivered);
        let mut claims = match self.draft_send_claims.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        if let Some(from_claim) = claims.remove(from_session_id) {
            let should_replace = claims
                .get(&resolved_target)
                .map(|target| target.revision <= from_claim.revision)
                .unwrap_or(true);
            if should_replace {
                claims.insert(resolved_target, from_claim);
            }
        }
        drop(claims);
        let mut discarded = match self.discarded_drafts.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        discarded.remove(from_session_id);
        discarded.remove(to_session_id);
    }

    pub fn mark_draft_discarded(&self, session_id: &str) {
        let mut discarded = match self.discarded_drafts.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        discarded.insert(session_id.to_string());
    }

    pub fn draft_was_discarded(&self, session_id: &str) -> bool {
        let discarded = match self.discarded_drafts.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        discarded.contains(session_id)
    }

    pub fn draft_revision_was_delivered(&self, session_id: &str, revision: u64) -> bool {
        let delivered = match self.delivered_draft_revisions.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        delivered
            .get(session_id)
            .is_some_and(|current| revision <= *current)
    }

    pub fn mark_draft_revision_delivered(&self, session_id: &str, revision: u64) {
        let mut delivered = match self.delivered_draft_revisions.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        delivered
            .entry(session_id.to_string())
            .and_modify(|current| *current = (*current).max(revision))
            .or_insert(revision);
    }

    /// 冻结一版已归档草稿用于发送。返回被更新 revision 替换的旧租约路径。
    pub fn register_draft_send_claim(
        &self,
        session_id: &str,
        revision: u64,
        attachment_paths: Vec<String>,
    ) -> Result<Vec<String>, String> {
        let mut claims = match self.draft_send_claims.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        if let Some(existing) = claims.get(session_id) {
            if existing.revision > revision {
                return Err("该草稿已有更新版本正在准备发送".to_string());
            }
            if existing.revision == revision {
                if existing.attachment_paths == attachment_paths {
                    return Ok(Vec::new());
                }
                return Err("同一草稿版本的附件快照不一致".to_string());
            }
        }
        let replaced = claims
            .insert(
                session_id.to_string(),
                DraftSendClaim {
                    revision,
                    attachment_paths,
                },
            )
            .map(|claim| claim.attachment_paths)
            .unwrap_or_default();
        Ok(replaced)
    }

    pub fn has_draft_send_claim(&self, session_id: &str, revision: u64) -> bool {
        let claims = match self.draft_send_claims.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        claims
            .get(session_id)
            .is_some_and(|claim| claim.revision == revision)
    }

    pub fn release_draft_send_claim(&self, session_id: &str, revision: u64) -> Vec<String> {
        let mut claims = match self.draft_send_claims.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        if claims
            .get(session_id)
            .is_some_and(|claim| claim.revision == revision)
        {
            return claims
                .remove(session_id)
                .map(|claim| claim.attachment_paths)
                .unwrap_or_default();
        }
        Vec::new()
    }

    pub fn release_any_draft_send_claim(&self, session_id: &str) -> Vec<String> {
        let mut claims = match self.draft_send_claims.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        claims
            .remove(session_id)
            .map(|claim| claim.attachment_paths)
            .unwrap_or_default()
    }

    pub fn claimed_draft_attachment_paths(&self) -> HashSet<String> {
        let claims = match self.draft_send_claims.lock() {
            Ok(guard) => guard,
            Err(err) => err.into_inner(),
        };
        claims
            .values()
            .flat_map(|claim| claim.attachment_paths.iter().cloned())
            .collect()
    }

    pub fn draft_update_lock(&self, session_id: &str) -> std::sync::Arc<AsyncMutex<()>> {
        let mut locks = match self.draft_update_locks.lock() {
            Ok(guard) => guard,
            Err(err) => {
                warn!(error = %err, "draft_update_locks 锁已污染，尝试恢复");
                err.into_inner()
            }
        };
        locks
            .entry(session_id.to_string())
            .or_insert_with(|| std::sync::Arc::new(AsyncMutex::new(())))
            .clone()
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
        let (template, session_configs, new_sig) = self
            .with_state_read(|core_state| {
                let new_models = core_state.models_config().clone();
                let new_sig = tiangong_config::registry::plugin_set_signature(&new_models);
                // 同步 app-state 的最新 models 到 config 内存单例。
                tiangong_config::registry::set_models(new_models);
                let template = core_state.build_core_config_for_session_from_base(&base, "");
                let session_configs = core_state
                    .session_metadata()
                    .iter()
                    .map(|metadata| {
                        (
                            metadata.id.clone(),
                            core_state.build_core_config_for_session_from_base(&base, &metadata.id),
                        )
                    })
                    .collect::<HashMap<_, _>>();
                Ok((template, session_configs, new_sig))
            })
            .await?;
        let plugin_set_changed = old_sig != new_sig;
        // 这份 provider 只作全局模板和新 Core 构造辅助，不承载任一会话的
        // trust/reasoning 覆盖。已存在 Core 使用各自独立的 provider。
        // 能力集合变化时，存活 Core 的插件列表无法热更（构造时固定）。
        // 存活 Core 继续用旧插件直到自然停止；下次 ensure_core 新建时用最新插件。
        // 仅 endpoint/trust/reasoning 变化时，按 session 热更存活 Core 的配置。
        if !plugin_set_changed {
            self.core_manager.sync_config(template, &session_configs);
        } else {
            self.config.replace(template);
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

    /// 获取或创建会话对应的 TiangongCore。
    ///
    /// 已存在的 Core 直接复用（热更配置/trust）；不存在时调用 `create_core` 新建。
    ///
    /// Core 是会话级资源，空闲只表示当前没有 turn task，并不表示实例已经停止。
    /// 关闭和移除是删除会话、失败回滚等显式流程的职责。
    /// 确保会话 Core 存在（issue #245：转走 core_manager）。
    ///
    /// 构建 per-session 配置 → 构造桌面插件集合 → 调
    /// `core_manager.ensure_core`。命中既有 Core 则刷新配置；否则新建。
    pub(crate) async fn ensure_core(
        &self,
        session_id: &str,
        session: tiangong_core::session::Session,
        stream_tx: std::sync::mpsc::Sender<tiangong_types::StreamEvent>,
    ) -> EnsuredCore {
        let base = self.config.snapshot();
        let session_config = {
            let core_state = self.state.lock().await;
            core_state.build_core_config_for_session_from_base(&base, session_id)
        };
        // 桌面插件集合由 DesktopCoreFactory 构造（host 专属）。
        let plugins = self.desktop_factory.build_plugins().await;
        let ensured = self
            .core_manager
            .ensure_core(session_id, session_config, stream_tx, plugins)
            .await
            .expect("ensure_core 不应失败");
        // session 参数的 trust_mode 仅供既有 Core 刷新(ensure_core 内已用 config.trust_mode)。
        let _ = session;
        EnsuredCore {
            session_id: ensured.session_id,
            is_new: ensured.is_new,
        }
    }

    /// 取回 core 的 session（消费 core，用于持久化或切换会话）
    pub fn take_core(&self, session_id: &str) -> Option<TiangongCore> {
        self.core_manager.take_core(session_id)
    }

    /// 关闭并等待指定会话的 Core 结束（issue #245：转走 core_manager）。
    pub async fn retire_core(&self, session_id: &str, cancel: bool) {
        self.core_manager.retire_core(session_id, cancel).await;
    }

    /// 指定会话是否持有可投递的 Core 实例。
    pub fn has_live_core(&self, session_id: &str) -> bool {
        self.core_manager.has_live_core(session_id)
    }

    /// 仅当指定会话存在 Core 时投递输入。
    pub fn deliver_to_core_if_live(&self, session_id: &str, input: AgentInputKind) -> bool {
        self.core_manager.deliver_to_core_if_live(session_id, input)
    }

    /// 向 Core 投递已准备好的用户消息（fire-and-forget，不等持久化确认）。
    pub fn deliver_prepared_if_live(
        &self,
        session_id: &str,
        message_id: String,
        prepared: Vec<tiangong_types::ContentBlock>,
    ) -> Result<(), String> {
        if !self.remote_turn_allows_message(session_id, &message_id) {
            return Err("会话正在处理远端请求，拒绝插入其他用户消息".to_string());
        }
        if !self.core_manager.has_live_core(session_id) {
            return Err("会话 Core 不存在".to_string());
        }
        self.core_manager
            .deliver_to_core_if_live(
                session_id,
                AgentInputKind::prepared_with_id(message_id, prepared),
            )
            .then_some(())
            .ok_or_else(|| "会话 Core 投递失败".to_string())
    }

    /// 取消指定会话的执行
    pub fn cancel_core(&self, session_id: &str) -> bool {
        self.core_manager.cancel_core(session_id)
    }

    /// 取消指定会话中某个 Agent 的当前执行
    pub fn cancel_agent_core(&self, session_id: &str, role: String) -> bool {
        tiangong_plugin_agent_team::cancel_agent(session_id, &role)
    }

    /// 向指定会话的 core 发送审批响应
    pub fn respond_approval_to_core(&self, session_id: &str, request_id: String, approved: bool) {
        self.core_manager
            .respond_approval_to_core(session_id, request_id, approved);
    }

    /// 设置指定会话 core 的信任模式（实时生效）
    pub fn set_core_trust_mode(
        &self,
        session_id: &str,
        mode: tiangong_core::permission::TrustMode,
    ) {
        self.core_manager.set_core_trust_mode(session_id, mode);
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
        let app_handle = self
            .app_handle
            .get()
            .cloned()
            .ok_or_else(|| "Desktop 应用尚未完成初始化".to_string())?;
        let event_bus = std::sync::Arc::new(tiangong_server::remote::event::EventBus::default());
        let core_backend: std::sync::Arc<dyn tiangong_server::remote::backend::ServerCoreBackend> =
            crate::embedded_server::spawn_desktop_server_core_bridge(app_handle, event_bus.clone());
        let handle = tiangong_server::run_embedded(
            host,
            port,
            token,
            tiangong_server::EmbeddedServerDependencies {
                state: self.state.clone(),
                config: self.config.clone(),
                core_backend,
                scheduler_context: self.create_scheduler_context(),
                mcp_plugin: self.mcp_plugin.clone(),
                event_bus,
            },
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

    /// 创建调度器执行上下文（定时消息通过本应用的统一 Core 路由执行）
    pub fn create_scheduler_context(
        &self,
    ) -> std::sync::Arc<dyn tiangong_scheduler::executor::SchedulerContext> {
        self.scheduler_context.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn same_session_core_creation_uses_one_serial_boundary() {
        // CoreManager.creation_lock 替代了原 CoreCreationLocks(issue #245)。
        use tiangong_core::core_config::{CoreConfig, CoreConfigProvider};
        let manager = tiangong_core_manager::CoreManager::new(
            CoreConfigProvider::new(CoreConfig::default()),
            std::path::PathBuf::from("/tmp"),
        );
        let first = manager.creation_lock("session-1");
        let second = manager.creation_lock("session-1");
        let other_session = manager.creation_lock("session-2");

        assert!(std::sync::Arc::ptr_eq(&first, &second));
        assert!(!std::sync::Arc::ptr_eq(&first, &other_session));

        let first_guard = first.lock_owned().await;
        let entered = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let entered_in_task = std::sync::Arc::clone(&entered);
        let (waiting_tx, waiting_rx) = tokio::sync::oneshot::channel();
        let mut waiter = tokio::spawn(async move {
            let _ = waiting_tx.send(());
            let _guard = second.lock_owned().await;
            entered_in_task.store(true, Ordering::Release);
        });

        waiting_rx.await.expect("后继创建应开始等待同一把锁");
        tokio::task::yield_now().await;
        assert!(!entered.load(Ordering::Acquire));
        drop(first_guard);

        tokio::time::timeout(std::time::Duration::from_secs(1), &mut waiter)
            .await
            .expect("同会话后继创建应在前一创建离开后继续")
            .expect("创建锁等待任务不应失败");
        assert!(entered.load(Ordering::Acquire));
    }

    /// 空闲 Core 仍是可复用的会话级资源，只有从映射移除后才不再可投递。
    #[tokio::test]
    async fn has_live_core_tracks_core_instance_presence() {
        use std::sync::mpsc;
        use tiangong_core::core_config::{CoreConfig, CoreConfigProvider};

        let app = TiangongApp::new();
        let session_id = "session-lifecycle-test";
        assert!(!app.has_live_core(session_id), "初始无 Core");

        let session = tiangong_core::session::Session::new(session_id);
        let (event_tx, _event_rx) = mpsc::channel();
        let storage_root = tempfile::tempdir().unwrap();
        let core = tiangong_core::core::TiangongCore::builder()
            .config(CoreConfigProvider::new(CoreConfig::default()))
            .session_id(session.id)
            .stream_tx(event_tx)
            .plugins(Vec::new())
            .storage_root(storage_root.path())
            .trust_mode(session.trust_mode)
            .build();
        assert!(core.is_stopped(), "新 Core 当前没有活跃 turn");
        // 直接经 core_manager registry 插入(issue #245:不再有 TiangongApp.lock_cores)。
        app.core_manager
            .registry()
            .insert(session_id.to_string(), core);

        assert!(app.has_live_core(session_id), "空闲 Core 仍应可投递新消息");
        let core = app.take_core(session_id).expect("应能取回 Core");
        tokio::task::spawn_blocking(move || core.shutdown_join())
            .await
            .unwrap()
            .unwrap();
        assert!(!app.has_live_core(session_id), "移除后无 Core");
    }

    #[test]
    fn agent_worker_view_is_session_scoped_and_never_model_visible() {
        let app = TiangongApp::new();
        let mut first = tiangong_core::session::Message::new(
            tiangong_core::session::MessageRole::Assistant,
            "first",
        );
        first.id = "agent:child:assistant:reply".to_string();
        let mut second = tiangong_core::session::Message::new(
            tiangong_core::session::MessageRole::Assistant,
            " second",
        );
        second.id = first.id.clone();

        app.merge_agent_output_view("parent-a", "child", "dev", "Developer", &[first]);
        app.merge_agent_output_view("parent-a", "child", "dev", "Developer", &[second]);

        let messages = app.agent_worker_view_messages("parent-a");
        assert_eq!(messages.len(), 2, "应包含稳定标题和一条合并后的回复");
        assert!(messages.iter().all(|message| message.model_excluded));
        let reply = messages
            .iter()
            .find(|message| message.id == "agent:child:assistant:reply")
            .unwrap();
        assert_eq!(reply.text_content(), "first second");
        assert!(app.agent_worker_view_messages("parent-b").is_empty());

        app.clear_agent_worker_view("parent-a");
        assert!(app.agent_worker_view_messages("parent-a").is_empty());
    }

    #[tokio::test]
    async fn remote_turn_ownership_serializes_until_exact_owner_finishes() {
        let app = std::sync::Arc::new(TiangongApp::new());
        app.begin_remote_turn("session-1", "message-1").unwrap();
        assert!(app.remote_turn_allows_message("session-1", "message-1"));
        assert!(!app.remote_turn_allows_message("session-1", "message-2"));

        let waiting_app = app.clone();
        let mut waiting = tokio::spawn(async move {
            waiting_app.wait_for_remote_turn_release("session-1").await;
        });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished(), "所有者释放前同会话请求必须等待");

        app.finish_remote_turn("session-1", "wrong-message");
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished(), "错误消息 ID 不能释放其他轮次");
        app.finish_remote_turn("session-1", "message-1");
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut waiting)
            .await
            .expect("正确所有者释放后等待方应继续")
            .expect("等待任务不应失败");
    }
}
