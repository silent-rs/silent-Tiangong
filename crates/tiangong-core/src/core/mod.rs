//! TiangongCore：天工智能体核心
//!
//! 单一线程完成所有工作：接收消息 → LLM 调用 → 工具执行 → session 更新 → 推送 StreamEvent
//! CLI / GUI / Server / Connector 统一通过 TiangongCore 运行。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender, Sender as StdSender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use tokio::sync::mpsc as tokio_mpsc;

use crate::agents::execution_mcp_agent::{McpFunctionTarget, execution_function_tools};
use crate::core_config::CoreConfigProvider;
use crate::model::{ModelClient, ModelRequest, SingleProviderClient, ToolSpec};
use crate::react::message::{append_or_reuse_user_message, append_runtime_tool_message};
use crate::runtime::{RuntimeEngine, inject_enhanced_tools};
use crate::session::{Message, MessageRole, Session};
use tiangong_types::{SessionStreamEvent, StreamEvent};

const MAX_ROUNDS: usize = 20;

// ── Memory re-exports ──
pub use crate::index::{
    WorkspaceIndexInfo, backfill_session_index, delete_workspace_index_for_gui,
    list_workspace_indexes_for_gui, rebuild_workspace_index_for_gui, session_index_exists,
    workspace_index_exists,
};
pub use crate::memory::gui_api::*;
pub(crate) use crate::memory::recall::{
    duplicate_memory_recall_tool_result, execute_memory_recall_tool, inject_memory_recall_tool,
};
pub(crate) use crate::memory::registry::{WorkerMemoryContext, get_or_init_memory_async};
pub use crate::memory::registry::{
    get_or_init_memory_handle_async, load_memory_config, save_memory_config,
    shutdown_memory_registry_blocking,
};

pub(crate) mod command;
pub(crate) use command::Command;

/// 天工智能体核心
pub struct TiangongCore {
    /// 用户命令发送端（tokio unbounded，send 不需要 await）
    cmd_tx: Option<tokio_mpsc::UnboundedSender<Command>>,
    /// 工作线程
    worker: Option<JoinHandle<Session>>,
    /// 会话 ID
    session_id: String,
    /// 独立的信任模式（共享引用，实时生效）
    shared_trust_mode: Arc<RwLock<crate::permission::TrustMode>>,
    /// 取消标志（stream consumer 检查此标志跳过 Delta/Reasoning 事件）
    cancel_flag: Arc<AtomicBool>,
}

impl TiangongCore {
    /// 创建新对话
    pub fn new(config: CoreConfigProvider, stream_tx: Sender<SessionStreamEvent>) -> Self {
        Self::new_for_process(config, stream_tx, tiangong_memory::ProcessType::Cli)
    }

    pub fn new_for_cli(config: CoreConfigProvider, stream_tx: Sender<SessionStreamEvent>) -> Self {
        Self::new_for_process(config, stream_tx, tiangong_memory::ProcessType::Cli)
    }

    pub fn new_for_process(
        config: CoreConfigProvider,
        stream_tx: Sender<SessionStreamEvent>,
        process_type: tiangong_memory::ProcessType,
    ) -> Self {
        let session = Session::new("新对话");
        Self::with_session_for_process(config, session, stream_tx, process_type)
    }

    /// 从已有 session 创建
    pub fn with_session(
        config: CoreConfigProvider,
        session: Session,
        stream_tx: Sender<SessionStreamEvent>,
    ) -> Self {
        Self::with_session_for_process(
            config,
            session,
            stream_tx,
            tiangong_memory::ProcessType::Cli,
        )
    }

    pub fn with_session_for_gui(
        config: CoreConfigProvider,
        session: Session,
        stream_tx: Sender<SessionStreamEvent>,
    ) -> Self {
        Self::with_session_for_process(
            config,
            session,
            stream_tx,
            tiangong_memory::ProcessType::Gui,
        )
    }

    pub fn with_session_for_server(
        config: CoreConfigProvider,
        session: Session,
        stream_tx: Sender<SessionStreamEvent>,
    ) -> Self {
        Self::with_session_for_process(
            config,
            session,
            stream_tx,
            tiangong_memory::ProcessType::Server,
        )
    }

    /// 从已有 session 创建，并显式标记入口进程类型。
    pub fn with_session_for_process(
        config: CoreConfigProvider,
        session: Session,
        stream_tx: Sender<SessionStreamEvent>,
        process_type: tiangong_memory::ProcessType,
    ) -> Self {
        let config_snapshot = config.snapshot();
        let config_generation = config.generation();
        let initial_trust_mode = session.trust_mode;
        let shared_trust_mode = Arc::new(RwLock::new(initial_trust_mode));
        let session_id = session.id.clone();
        let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel::<Command>();

        let worker_trust_mode = shared_trust_mode.clone();
        let worker = thread::spawn(move || {
            let memory = WorkerMemoryContext {
                handle: None,
                process_type,
                initial_config_snapshot: Some(config_snapshot),
                initial_config_generation: config_generation,
            };
            worker_loop(
                config,
                session,
                stream_tx,
                cmd_rx,
                worker_trust_mode,
                memory,
            )
        });

        Self {
            cmd_tx: Some(cmd_tx),
            worker: Some(worker),
            session_id,
            shared_trust_mode,
            cancel_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    fn send_cmd(&self, cmd: Command) -> bool {
        let Some(ref tx) = self.cmd_tx else {
            return false;
        };
        tx.send(cmd).is_ok()
    }

    pub fn send_message(&self, content: String) -> bool {
        self.send_cmd(Command::Message {
            content,
            message_id: None,
            media: Vec::new(),
        })
    }

    pub fn send_message_with_id(
        &self,
        content: String,
        message_id: String,
        media: Vec<tiangong_types::MediaAsset>,
    ) -> bool {
        self.send_cmd(Command::Message {
            content,
            message_id: Some(message_id),
            media,
        })
    }

    pub fn update_cwd(&self, cwd: String) -> bool {
        self.send_cmd(Command::UpdateCwd { cwd })
    }

    pub fn reload_config(&self) -> bool {
        self.send_cmd(Command::ReloadConfig)
    }

    pub fn cancel(&self) -> bool {
        self.cancel_flag.store(true, Ordering::Release);
        self.send_cmd(Command::Cancel)
    }

    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel_flag.clone()
    }

    pub fn cancel_agent(&self, role: String) -> bool {
        self.send_cmd(Command::CancelAgent { role })
    }

    pub fn compress_context(&self) -> bool {
        self.send_cmd(Command::CompressContext)
    }

    pub fn reset_context(&self) -> bool {
        self.send_cmd(Command::ResetContext)
    }

    pub fn respond_approval(&self, request_id: String, approved: bool) -> bool {
        self.send_cmd(Command::Approval {
            request_id,
            approved,
        })
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn is_running(&self) -> bool {
        self.worker
            .as_ref()
            .map(|worker| !worker.is_finished())
            .unwrap_or(false)
    }

    /// 设置信任模式（实时生效，当前对话下一次工具调用立即感知）
    pub fn set_trust_mode(&self, mode: crate::permission::TrustMode) {
        if let Ok(mut guard) = self.shared_trust_mode.write() {
            *guard = mode;
        }
    }

    /// 注入页面获取能力（GUI 模式下由 Tauri Plugin 提供）
    pub fn set_page_fetcher(&self, fetcher: std::sync::Arc<dyn crate::browser_trait::PageFetcher>) {
        let _ = self.send_cmd(Command::SetPageFetcher { fetcher });
    }

    /// 注册工具覆盖处理器
    pub fn register_tool_override(
        &self,
        name: &str,
        handler: std::sync::Arc<dyn crate::tool_override::ToolOverrideHandler>,
    ) {
        let _ = self.send_cmd(Command::RegisterToolOverride {
            name: name.to_string(),
            handler,
        });
    }

    /// 注入浏览器页面内容到当前会话（不触发 LLM 调用）
    pub fn inject_browser_content(
        &self,
        title: String,
        url: String,
        text: String,
        tabs: Vec<(String, String, String)>,
        active_tab_id: Option<String>,
    ) -> bool {
        self.send_cmd(Command::InjectBrowserContent {
            title,
            url,
            text,
            tabs,
            active_tab_id,
        })
    }

    /// 关闭并获取最终 session
    pub fn into_session(mut self) -> Session {
        let _ = self.send_cmd(Command::Shutdown);
        self.cmd_tx = None;
        if let Some(w) = self.worker.take() {
            match w.join() {
                Ok(session) => return session,
                Err(_) => tracing::warn!("TiangongCore worker panic"),
            }
        }
        Session::new("recovered")
    }
}

impl Drop for TiangongCore {
    fn drop(&mut self) {
        if let Some(ref tx) = self.cmd_tx {
            let _ = tx.send(Command::Shutdown);
        }
        self.cmd_tx = None;
    }
}

// ==================== 工作线程 ====================

/// 工作线程：接收用户命令，执行 LLM + 工具，推送 StreamEvent
fn worker_loop(
    config: CoreConfigProvider,
    session: Session,
    external_tx: StdSender<SessionStreamEvent>,
    cmd_rx: tokio_mpsc::UnboundedReceiver<Command>,
    shared_trust_mode: Arc<RwLock<crate::permission::TrustMode>>,
    memory: WorkerMemoryContext,
) -> Session {
    // 在专用的 tokio runtime 上运行 async 工作循环，
    // 使 execute_turn_inner 可以用 select! + stream_function_calls 实现真正取消。
    //
    // 注意：execute_turn_inner_async 内部使用了 tokio::task::block_in_place，
    // 该 API 仅在 multi-thread runtime 可用，因此这里使用 1 worker 的 multi-thread runtime。
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("创建 TiangongCore tokio runtime 失败");
    rt.block_on(worker_loop_async(
        config,
        session,
        external_tx,
        cmd_rx,
        shared_trust_mode,
        memory,
    ))
}

/// 真正的 async 工作循环
async fn worker_loop_async(
    config: CoreConfigProvider,
    mut session: Session,
    external_tx: StdSender<SessionStreamEvent>,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<Command>,
    shared_trust_mode: Arc<RwLock<crate::permission::TrustMode>>,
    mut memory: WorkerMemoryContext,
) -> Session {
    let session_id = session.id.clone();
    let mut last_cfg_gen = 0u64;
    let mut saved_page_fetcher: Option<std::sync::Arc<dyn crate::browser_trait::PageFetcher>> =
        None;
    let mut saved_tool_overrides: std::collections::HashMap<
        String,
        std::sync::Arc<dyn crate::tool_override::ToolOverrideHandler>,
    > = std::collections::HashMap::new();

    // 在 Worker 的 tokio runtime 中异步初始化 Memory Handle
    if let Some(ref cfg) = memory.initial_config_snapshot {
        memory.handle = get_or_init_memory_async(
            cfg,
            memory.initial_config_generation,
            memory.process_type.clone(),
        )
        .await;
    }
    let mut engine: Option<RuntimeEngine> = None;
    let mut tools: Vec<ToolSpec> = Vec::new();
    let mut mcp_targets: HashMap<String, McpFunctionTarget> = HashMap::new();
    let team_context = Arc::new(Mutex::new(crate::agent_team::lifecycle::TeamContext::new()));
    let mut team_restored = false;
    // turn 计数器：每 10 个 turn 触发一次 Meta 反刍（归档低活跃节点）
    let mut turn_count: u32 = 0;

    // IndexManager：Workspace 文件索引 + Session 对话索引
    let index_manager = crate::index::IndexManager::new()
        .map(std::sync::Arc::new)
        .map_err(|e| {
            tracing::warn!("IndexManager 初始化失败: {e}");
            e
        })
        .ok();

    // 内部 StreamEvent 通道 —— 转发线程负责包装 session_id
    // stream_tx 保持 std::sync::mpsc（工具执行等同步代码可直接使用）
    let (stream_tx, stream_rx) = mpsc::channel::<StreamEvent>();
    let fwd_session_id = session_id.clone();
    let fwd_tx = external_tx.clone();
    let forward_handle = thread::spawn(move || {
        while let Ok(event) = stream_rx.recv() {
            if fwd_tx
                .send(SessionStreamEvent {
                    session_id: fwd_session_id.clone(),
                    event,
                })
                .is_err()
            {
                break;
            }
        }
    });

    apply_session_cwd(&session);

    // 初始索引：仅在索引不存在时扫描
    if let Some(ref im) = index_manager {
        let root = std::path::PathBuf::from(&session.cwd);
        if root.is_dir() && !crate::index::workspace_index_exists(&root) {
            match im.full_scan(&root) {
                Ok(count) => tracing::info!(count, "Workspace 初始索引扫描完成"),
                Err(e) => tracing::warn!("Workspace 初始索引扫描失败: {e}"),
            }
        }
    }

    // 恢复未完成的审批请求（崩溃恢复场景）
    let pending = crate::approval_store::get_pending(&session_id);
    if !pending.is_empty() {
        tracing::info!(count = pending.len(), "恢复未完成的审批请求");
        for approval in &pending {
            let _ = stream_tx.send(StreamEvent::ApprovalNeeded {
                request_id: approval.request_id.clone(),
                tool_name: approval.tool_name.clone(),
                args_summary: approval.tool_args_summary.clone(),
            });
        }
    }

    while let Some(cmd) = cmd_rx.recv().await {
        // 配置变更检测：仅在 generation 变化时重建 engine 和工具列表
        let cfg_gen = config.generation();
        if engine.is_none() || cfg_gen != last_cfg_gen {
            let cfg = config.snapshot();
            memory.handle =
                get_or_init_memory_async(&cfg, cfg_gen, memory.process_type.clone()).await;
            engine = Some(build_engine_from_config(
                &cfg,
                &stream_tx,
                shared_trust_mode.clone(),
            ));
            // 恢复 page_fetcher 和 tool_overrides 到新建的引擎
            if let Some(ref fetcher) = saved_page_fetcher {
                engine.as_ref().unwrap().set_page_fetcher(fetcher.clone());
            }
            for (name, handler) in &saved_tool_overrides {
                engine
                    .as_ref()
                    .unwrap()
                    .register_tool_override(name, handler.clone());
            }
            let e = engine.as_ref().unwrap();
            let (all_tools, new_mcp_targets) = execution_function_tools(&e.agent_config().mcp);
            let mut new_tools: Vec<ToolSpec> = all_tools
                .into_iter()
                .filter(|t| t.name != "mark_step_completed")
                .collect();
            inject_enhanced_tools(&mut new_tools, e);
            if index_manager.is_some() {
                crate::index::inject_index_search_tool(&mut new_tools);
            }
            if memory.handle.is_some() {
                inject_memory_recall_tool(&mut new_tools);
            }
            tools = new_tools;
            mcp_targets = new_mcp_targets;
            if !team_restored {
                if let Ok(mut team) = team_context.lock() {
                    let restored =
                        crate::agent_team::lifecycle::restore_agents_from_session_history(
                            &mut team, &session, &tools,
                        );
                    if restored > 0 {
                        tracing::info!(count = restored, "已从会话历史恢复 Agent 团队");
                    }
                }
                team_restored = true;
            }
            last_cfg_gen = cfg_gen;
        }

        match cmd {
            Command::UpdateCwd { cwd } => {
                let cwd_changed = cwd != session.cwd;
                session.cwd = cwd;
                apply_session_cwd(&session);
                let cfg = config.snapshot();
                memory.handle = get_or_init_memory_async(
                    &cfg,
                    config.generation(),
                    memory.process_type.clone(),
                )
                .await;

                // 索引：仅当 CWD 实际变化时扫描
                if cwd_changed && let Some(ref im) = index_manager {
                    let root = std::path::PathBuf::from(&session.cwd);
                    if root.is_dir() {
                        match im.full_scan(&root) {
                            Ok(count) => {
                                tracing::info!(count, "Workspace 索引扫描完成");
                            }
                            Err(e) => {
                                tracing::warn!("Workspace 索引扫描失败: {e}");
                            }
                        }
                    }
                }

                continue;
            }
            Command::ReloadConfig => {
                continue;
            }
            Command::Message {
                content,
                message_id,
                media,
            } => {
                let turn_start_idx = session.messages.len();
                // 记录用户消息
                let user_msg_id =
                    append_or_reuse_user_message(&mut session, &content, message_id, media);
                // 通知消费端：用户消息已记录（携带 session 中的 message_id）
                let _ = stream_tx.send(StreamEvent::UserMessage {
                    message_id: user_msg_id.clone(),
                    content: content.clone(),
                    media: session
                        .messages
                        .iter()
                        .find(|message| message.id == user_msg_id)
                        .map(|message| message.media.clone())
                        .unwrap_or_default(),
                });

                // 执行对话轮次
                execute_turn_async(
                    &mut session,
                    &content,
                    engine.as_ref().unwrap(),
                    &tools,
                    &mcp_targets,
                    &stream_tx,
                    &mut cmd_rx,
                    memory.handle.as_ref(),
                    index_manager.clone(),
                    team_context.clone(),
                )
                .await;

                // Turn 完成后 → Session 索引
                if let Some(ref im) = index_manager {
                    index_turn_messages(im, &session, turn_start_idx);
                }

                // turn 完成后触发增强版 Micro 反刍
                if let Some(handle) = memory.handle.as_ref() {
                    let enhanced_result = build_enhanced_memory_turn_result(
                        &session,
                        turn_start_idx,
                        &content,
                        vec![],
                    );
                    tokio::task::block_in_place(|| {
                        handle.run_enhanced_micro_rumination_blocking(enhanced_result);
                    });

                    // 每 10 个 turn 触发一次 Meta 反刍（归档低活跃节点）
                    turn_count += 1;
                    if turn_count.is_multiple_of(10) {
                        handle.run_meta_rumination();
                        tracing::debug!(turn_count, "Meta 反刍已触发（定期归档）");
                    }
                }
            }
            Command::Cancel => {
                // Cancel 信号通过 cmd_rx 传递到 engine 内部处理；
                // engine 在每个 block_in_place 前后检查取消信号。
            }
            Command::CancelAgent { .. } => {
                // 仅在多 Agent 执行等待期间由 ReactEngine 转发处理。
            }
            Command::Approval {
                request_id,
                approved,
            } => {
                // 恢复场景的审批响应：清除 pending 状态
                let pending = crate::approval_store::get_pending(&session_id);
                let had_pending = pending.iter().any(|a| a.request_id == request_id);
                crate::approval_store::remove_pending(&session_id, &request_id);

                if had_pending {
                    let action = if approved { "已允许" } else { "已拒绝" };
                    append_runtime_tool_message(
                        &mut session,
                        "approval_response",
                        format!("审批响应：{action}（会话已恢复，请重新发送消息继续）"),
                    );
                    session.persist_to_disk();
                    let _ = stream_tx.send(StreamEvent::Done { usage: None });
                }
            }
            Command::Shutdown => break,
            Command::SetPageFetcher { fetcher } => {
                saved_page_fetcher = Some(fetcher.clone());
                if let Some(eng) = engine.as_ref() {
                    eng.set_page_fetcher(fetcher);
                }
                continue;
            }
            Command::RegisterToolOverride { name, handler } => {
                saved_tool_overrides.insert(name.clone(), handler.clone());
                if let Some(eng) = engine.as_ref() {
                    eng.register_tool_override(&name, handler);
                }
                continue;
            }
            Command::InjectBrowserContent {
                title,
                url,
                text,
                tabs,
                active_tab_id,
            } => {
                let turn_start_idx = session.messages.len();
                crate::react::message::inject_browser_content_to_session(
                    &mut session,
                    &stream_tx,
                    &crate::react::message::BrowserContent {
                        title: &title,
                        url: &url,
                        text: &text,
                        tabs: &tabs,
                        active_tab_id: active_tab_id.as_deref(),
                    },
                    false,
                );
                let browser_input = format!("[浏览器页面更新] {url}");
                execute_turn_async(
                    &mut session,
                    &browser_input,
                    engine.as_ref().unwrap(),
                    &tools,
                    &mcp_targets,
                    &stream_tx,
                    &mut cmd_rx,
                    memory.handle.as_ref(),
                    index_manager.clone(),
                    team_context.clone(),
                )
                .await;
                if let Some(ref im) = index_manager {
                    index_turn_messages(im, &session, turn_start_idx);
                }
                if let Some(handle) = memory.handle.as_ref() {
                    let enhanced_result = build_enhanced_memory_turn_result(
                        &session,
                        turn_start_idx,
                        &browser_input,
                        vec![],
                    );
                    tokio::task::block_in_place(|| {
                        handle.run_enhanced_micro_rumination_blocking(enhanced_result);
                    });
                    turn_count += 1;
                    if turn_count.is_multiple_of(10) {
                        handle.run_meta_rumination();
                    }
                }
            }
            Command::CompressContext => {
                compress_context_for_session(&mut session, engine.as_ref().unwrap(), &stream_tx);
                continue;
            }
            Command::ResetContext => {
                reset_context_for_session(&mut session, &stream_tx, engine.as_ref().unwrap());
                continue;
            }
        }
    }

    // 关闭内部通道，等待转发线程结束
    drop(stream_tx);
    tokio::task::spawn_blocking(|| ()).await.ok(); // yield，让 forward_handle 有机会 drain
    let _ = forward_handle.join();

    // 会话结束 → 触发 Meso 反刍（提炼 Entity/Decision，更新 Workspace Injection）
    // fire-and-forget：handle 仍可使用（Memory Actor 在 registry 中持续运行）
    if let Some(handle) = memory.handle.as_ref() {
        handle.run_meso_rumination(session_id.clone(), "__global__".to_string());
        tracing::info!(session_id = %session_id, "Meso 反刍已触发（会话结束）");
    }

    // 会话结束 → finalize Session 索引
    if let Some(ref im) = index_manager
        && let Err(e) = im.finalize_session_index(&session_id)
    {
        tracing::warn!("Session 索引 finalize 失败: {e}");
    }

    session
}

fn index_turn_messages(
    index_manager: &crate::index::IndexManager,
    session: &Session,
    turn_start_idx: usize,
) {
    let turns: Vec<crate::index::TurnData> = session.messages[turn_start_idx..]
        .iter()
        .filter_map(|msg| {
            let role = match msg.role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::Tool => "tool",
                MessageRole::System => return None,
            };
            Some(crate::index::TurnData {
                turn_id: msg.id.clone(),
                workspace_id: session.cwd.clone(),
                role: role.to_string(),
                content: msg.text_content(),
                topics: Vec::new(),
                entity_names: Vec::new(),
            })
        })
        .collect();
    if let Err(e) = index_manager.index_turn_batch(&session.id, &turns) {
        tracing::warn!("Session 索引批量写入失败: {e}");
    }
}

pub(crate) fn apply_session_cwd(session: &Session) {
    let cwd = session.cwd.trim();
    if cwd.is_empty() {
        crate::tool::set_session_cwd(None);
        return;
    }

    let path = std::path::PathBuf::from(cwd);
    if path.is_dir() {
        crate::tool::set_session_cwd(Some(path));
    }
}

pub(crate) fn compress_context_for_session(
    session: &mut Session,
    engine: &RuntimeEngine,
    stream_tx: &StdSender<StreamEvent>,
) {
    let _ = stream_tx.send(StreamEvent::ContextCompressing {
        summary_up_to: session.summary_up_to,
        total_messages: session.messages.len(),
    });
    let organizer = crate::context::organizer::ContextOrganizer::new(engine.context_limit)
        .with_keep_recent_turns(6);
    match organizer.force_update_summary_with_usage(session, engine.client()) {
        Ok(update) => {
            let remaining = session.messages.len().saturating_sub(session.summary_up_to);
            session.current_tokens = 0;
            session.active_agent_current_tokens = 0;
            session.agent_current_tokens.clear();
            crate::react::context::emit_token_usage(
                stream_tx,
                &update.usage,
                Some(0),
                engine.context_limit,
                "manual_context_compress",
                None,
            );
            let _ = stream_tx.send(StreamEvent::ContextCompressed {
                action: if update.compressed {
                    tiangong_types::stream::ContextCompressAction::Compress
                } else {
                    tiangong_types::stream::ContextCompressAction::Noop
                },
                summary_up_to: session.summary_up_to,
                remaining_messages: remaining,
            });
            session.persist_to_disk();
        }
        Err(err) => {
            let _ = stream_tx.send(StreamEvent::Error {
                message: format!("上下文压缩失败：{err}"),
            });
        }
    }
}

pub(crate) fn reset_context_for_session(
    session: &mut Session,
    stream_tx: &StdSender<StreamEvent>,
    engine: &RuntimeEngine,
) {
    let total = session.messages.len();
    session.summary_up_to = total;
    crate::context::compressor::mark_compact_boundary(&mut session.messages, total);
    session.context_summary = None;
    session.current_tokens = 0;
    session.active_agent_current_tokens = 0;
    session.agent_current_tokens.clear();
    // 清空后重建 system prompt
    crate::react::context::rebuild_system_prompt(session, engine);
    let _ = stream_tx.send(StreamEvent::ContextCompressed {
        action: tiangong_types::stream::ContextCompressAction::Clear,
        summary_up_to: total,
        remaining_messages: 0,
    });
    session.persist_to_disk();
}

/// 执行一个完整的对话轮次（可能多轮工具调用），async 版
#[allow(clippy::too_many_arguments)]
async fn execute_turn_async(
    session: &mut Session,
    user_input: &str,
    engine: &RuntimeEngine,
    tools: &[ToolSpec],
    mcp_targets: &HashMap<String, McpFunctionTarget>,
    stream_tx: &StdSender<StreamEvent>,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
    memory_handle: Option<&tiangong_memory::MemoryHandle>,
    index_manager: Option<std::sync::Arc<crate::index::IndexManager>>,
    team_context: Arc<Mutex<crate::agent_team::lifecycle::TeamContext>>,
) {
    let mut react = crate::react::engine::ReactEngine::new(
        engine.clone(),
        tools.to_vec(),
        mcp_targets.clone(),
        MAX_ROUNDS,
    )
    .with_shared_team(team_context, "main".to_string());
    react
        .execute_turn(
            session,
            user_input,
            stream_tx,
            cmd_rx,
            memory_handle,
            index_manager,
        )
        .await;
}

/// 从 CoreConfig 快照构建 RuntimeEngine
///
/// `stream_tx` 用于在 LLM 请求重试时发送 `StreamEvent::Retry` 通知。
/// `shared_trust_mode` 是 TiangongCore 持有的独立信任模式，RuntimeEngine 共享此引用。
fn build_engine_from_config(
    config: &crate::core_config::CoreConfig,
    stream_tx: &StdSender<StreamEvent>,
    shared_trust_mode: Arc<RwLock<crate::permission::TrustMode>>,
) -> RuntimeEngine {
    use crate::agent_config::AgentConfig;
    use crate::model::OnRetryCallback;
    use crate::models_config::ModelsConfig;

    let models_config = ModelsConfig::from_llm_config(&config.llm);
    let model_config = models_config.to_chat_provider_config();
    let chat_is_multimodal = models_config.chat_is_multimodal();

    let agent_config = AgentConfig {
        mcp: config.mcp.clone(),
        skills: config.skills.clone(),
        trust_mode: config.trust_mode,
        default_trust_mode: config.default_trust_mode,
        custom_system_prompt: config.custom_system_prompt.clone(),
        reasoning_effort: config.reasoning_effort.clone(),
    };

    // 构造重试回调：发送 StreamEvent::Retry 通知前端
    let retry_tx = stream_tx.clone();
    let on_retry: OnRetryCallback =
        Arc::new(move |attempt, max_attempts, _delay_ms, error_text| {
            let _ = retry_tx.send(StreamEvent::Retry {
                message: error_text.to_string(),
                attempt,
                max_attempts,
            });
        });

    let context_limit = crate::core_config::resolve_context_limit(&config.llm.chat.model);
    let mut engine = RuntimeEngine::with_shared_trust_mode(
        SingleProviderClient::new(model_config).with_on_retry(on_retry.clone()),
        context_limit,
        agent_config,
        shared_trust_mode,
    )
    .with_models_config(models_config)
    .with_core_config(config.clone());

    // 如果配置了独立的 lite 端点，构建 lite client
    if let Some(ref lite_endpoint) = config.llm.lite {
        let lite_config = crate::model::ModelProviderConfig {
            api_auth_token: lite_endpoint.api_key.clone(),
            api_base_url: lite_endpoint.base_url.clone(),
            api_timeout_ms: lite_endpoint.timeout_ms.to_string(),
            api_protocol: lite_endpoint.protocol,
            api_model: lite_endpoint.model.clone(),
            api_lite_model: lite_endpoint.model.clone(),
        };
        engine = engine.with_lite_client(
            SingleProviderClient::new(lite_config).with_on_retry(on_retry.clone()),
        );
    }
    if let Some(ref multimodal_endpoint) = config.llm.multimodal {
        let multimodal_config = crate::model::ModelProviderConfig {
            api_auth_token: multimodal_endpoint.api_key.clone(),
            api_base_url: multimodal_endpoint.base_url.clone(),
            api_timeout_ms: multimodal_endpoint.timeout_ms.to_string(),
            api_protocol: multimodal_endpoint.protocol,
            api_model: multimodal_endpoint.model.clone(),
            api_lite_model: String::new(),
        };
        engine = engine.with_multimodal_client(
            SingleProviderClient::new(multimodal_config).with_on_retry(on_retry),
        );
    }

    // 当 chat 模型自带 multimodal 能力但没有独立 multimodal 端点时，
    // 用 chat client 充当 multimodal_client（用于 ensure_multimodal_enabled 等检查）
    let needs_fallback_multimodal = !engine.has_multimodal_client() && chat_is_multimodal;
    if needs_fallback_multimodal {
        let chat_client = engine.client().clone();
        engine = engine.with_multimodal_client(chat_client);
    }

    engine
}

pub(crate) fn execute_attachment_analysis_tool(
    call: &crate::model::ToolCall,
    engine: &RuntimeEngine,
    session: &Session,
) -> (crate::tool::ToolResult, crate::model::TokenUsage) {
    let started = std::time::Instant::now();
    if !engine.has_multimodal_client() {
        return (
            attachment_tool_result(
                false,
                "未配置多模态模型",
                String::new(),
                "multimodal model is not configured".to_string(),
                1,
                started,
            ),
            crate::model::TokenUsage::default(),
        );
    }

    let instruction = call
        .arguments
        .get("instruction")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("请解析附件内容，并提取与用户问题有关的信息。");
    let message_id = call
        .arguments
        .get("message_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let attachment_index = call
        .arguments
        .get("attachment_index")
        .and_then(serde_json::Value::as_u64)
        .map(|value| value as usize);

    let Some(source_message) = find_attachment_source_message(session, message_id) else {
        return (
            attachment_tool_result(
                false,
                "未找到可解析的附件",
                String::new(),
                "no user message with attachments found".to_string(),
                1,
                started,
            ),
            crate::model::TokenUsage::default(),
        );
    };

    let media = if let Some(index) = attachment_index {
        let all_media = collect_message_media(source_message);
        let Some(asset) = all_media.get(index) else {
            return (
                attachment_tool_result(
                    false,
                    "附件序号不存在",
                    String::new(),
                    format!("attachment_index {index} out of range"),
                    1,
                    started,
                ),
                crate::model::TokenUsage::default(),
            );
        };
        vec![asset.clone()]
    } else {
        collect_message_media(source_message)
    };

    if media.is_empty() {
        return (
            attachment_tool_result(
                false,
                "未找到可解析的附件",
                String::new(),
                "selected message has no attachments".to_string(),
                1,
                started,
            ),
            crate::model::TokenUsage::default(),
        );
    }

    let mut attachment_context = vec![Message::new(
        MessageRole::User,
        "你是附件解析助手。只根据随消息提供的附件内容和解析要求回答，输出可供主模型直接使用的简洁中文结果。".to_string(),
    )];
    let attachment_message = Message::new(
        MessageRole::Assistant,
        "好的，我将作为附件解析助手，根据附件内容和解析要求进行分析。".to_string(),
    );
    attachment_context.push(attachment_message);
    let mut user_message = Message::new(
        MessageRole::User,
        format!(
            "用户原始消息：{}\n\n解析要求：{}",
            source_message.text_content().trim(),
            instruction
        ),
    );
    for asset in media {
        user_message
            .content
            .push(crate::session::ContentBlock::Media {
                kind: asset.kind,
                url: asset.url.clone(),
                mime_type: asset.mime_type.clone(),
                title: asset.title.clone(),
            });
    }
    attachment_context.push(user_message);

    let req = ModelRequest {
        session_title: format!("{} · attachment-analysis", session.title),
        user_input: String::new(),
        context: attachment_context,
        thinking: None,
        reasoning_effort: None,
        thinking_disabled: false,
        include_media: true,
    };

    match engine.multimodal_client().complete(&req) {
        Ok(response) => (
            attachment_tool_result(
                true,
                "附件解析完成",
                response.text,
                String::new(),
                0,
                started,
            ),
            response.usage,
        ),
        Err(err) => (
            attachment_tool_result(
                false,
                "附件解析失败",
                String::new(),
                err.to_string(),
                1,
                started,
            ),
            crate::model::TokenUsage::default(),
        ),
    }
}

fn find_attachment_source_message<'a>(
    session: &'a Session,
    message_id: Option<&str>,
) -> Option<&'a Message> {
    let has_media = |msg: &Message| -> bool {
        !msg.media.is_empty()
            || msg
                .content
                .iter()
                .any(|b| matches!(b, tiangong_types::message::ContentBlock::Media { .. }))
    };
    if let Some(message_id) = message_id {
        return session
            .messages
            .iter()
            .find(|message| message.id == message_id && has_media(message));
    }
    session
        .messages
        .iter()
        .rev()
        .find(|message| message.role == MessageRole::User && has_media(message))
}

fn collect_message_media(message: &Message) -> Vec<tiangong_types::MediaAsset> {
    let mut assets = Vec::new();
    for block in &message.content {
        if let tiangong_types::message::ContentBlock::Media {
            kind,
            url,
            mime_type,
            title,
        } = block
        {
            assets.push(tiangong_types::MediaAsset {
                kind: *kind,
                url: url.clone(),
                mime_type: mime_type.clone(),
                title: title.clone(),
                capability: None,
            });
        }
    }
    assets.extend(message.media.clone());
    assets
}

fn attachment_tool_result(
    ok: bool,
    summary: impl Into<String>,
    stdout: String,
    stderr: String,
    exit_code: i32,
    started: std::time::Instant,
) -> crate::tool::ToolResult {
    let summary = summary.into();
    crate::tool::ToolResult {
        ok,
        summary: summary.clone(),
        stdout,
        stderr,
        exit_code,
        execution: Some(crate::tool::ToolExecutionRecord {
            tool_name: "analyze_attachment".to_string(),
            args: Vec::new(),
            duration_ms: started.elapsed().as_millis() as u64,
            ok,
            exit_code,
            summary,
        }),
    }
}

// ── Turn 记忆记录函数（已迁移至 memory::turn_result） ──
pub(crate) use crate::memory::turn_result::build_enhanced_memory_turn_result;
