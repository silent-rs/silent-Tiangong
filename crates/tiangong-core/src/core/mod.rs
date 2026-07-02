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

/// 单次工具执行阶段（ReAct Loop 内层）的最大轮次。
///
/// 30 轮：review 类多步骤任务（连续读取多个文件做审查）在 15 轮时容易触顶，
/// 过早进入总结阶段。30 轮给复杂任务更多空间；触顶后仍走与正常完成相同的总结
/// 路径（由模型自行判断完成度），无需特殊触顶逻辑。
const MAX_TOOL_ROUNDS: usize = 30;
/// 总结阶段后重新进入工具执行阶段的最大次数。
const MAX_OUTER_ITERATIONS: u32 = 3;

// ── Memory re-exports ──
pub use crate::index::{
    WorkspaceIndexInfo, backfill_session_index, delete_workspace_index_for_gui,
    list_workspace_indexes_for_gui, rebuild_workspace_index_for_gui, session_index_exists,
    workspace_index_exists,
};
pub use crate::memory::gui_api::*;
pub(crate) use crate::memory::registry::{WorkerMemoryContext, get_or_init_memory_async};
pub use crate::memory::registry::{
    get_or_init_memory_handle_async, load_memory_config, save_memory_config,
    shutdown_memory_registry_blocking,
};

pub(crate) mod command;
pub(crate) use command::Command;
pub mod plugin;
pub use plugin::Plugin;

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
    /// 创建新对话（CLI 便捷入口）。
    ///
    /// `plugins` 为进程内自注册插件（如定时任务插件），传 `Vec::new()` 表示不启用。
    pub fn new(
        config: CoreConfigProvider,
        stream_tx: Sender<SessionStreamEvent>,
        plugins: Vec<Arc<dyn Plugin>>,
    ) -> Self {
        Self::new_for_process(
            config,
            stream_tx,
            tiangong_memory::ProcessType::Cli,
            plugins,
        )
    }

    /// 创建 CLI 入口 core。
    pub fn new_for_cli(
        config: CoreConfigProvider,
        stream_tx: Sender<SessionStreamEvent>,
        plugins: Vec<Arc<dyn Plugin>>,
    ) -> Self {
        Self::new_for_process(
            config,
            stream_tx,
            tiangong_memory::ProcessType::Cli,
            plugins,
        )
    }

    pub fn new_for_process(
        config: CoreConfigProvider,
        stream_tx: Sender<SessionStreamEvent>,
        process_type: tiangong_memory::ProcessType,
        plugins: Vec<Arc<dyn Plugin>>,
    ) -> Self {
        let session = Session::new("新对话");
        Self::with_session_for_process(config, session, stream_tx, process_type, plugins)
    }

    /// 从已有 session 创建（CLI 便捷入口）。
    pub fn with_session(
        config: CoreConfigProvider,
        session: Session,
        stream_tx: Sender<SessionStreamEvent>,
        plugins: Vec<Arc<dyn Plugin>>,
    ) -> Self {
        Self::with_session_for_process(
            config,
            session,
            stream_tx,
            tiangong_memory::ProcessType::Cli,
            plugins,
        )
    }

    pub fn with_session_for_gui(
        config: CoreConfigProvider,
        session: Session,
        stream_tx: Sender<SessionStreamEvent>,
        plugins: Vec<Arc<dyn Plugin>>,
    ) -> Self {
        Self::with_session_for_process(
            config,
            session,
            stream_tx,
            tiangong_memory::ProcessType::Gui,
            plugins,
        )
    }

    pub fn with_session_for_server(
        config: CoreConfigProvider,
        session: Session,
        stream_tx: Sender<SessionStreamEvent>,
        plugins: Vec<Arc<dyn Plugin>>,
    ) -> Self {
        Self::with_session_for_process(
            config,
            session,
            stream_tx,
            tiangong_memory::ProcessType::Server,
            plugins,
        )
    }

    /// 从已有 session 创建，并显式标记入口进程类型。
    ///
    /// `plugins` 为进程内自注册插件（[`Plugin`]），在 worker_loop 的 engine
    /// 创建/重建时遍历调用 `Plugin::register`，向 engine 注入能力。
    /// 不需要插件能力的入口传 `Vec::new()`。
    pub fn with_session_for_process(
        config: CoreConfigProvider,
        session: Session,
        stream_tx: Sender<SessionStreamEvent>,
        process_type: tiangong_memory::ProcessType,
        plugins: Vec<Arc<dyn Plugin>>,
    ) -> Self {
        let config_snapshot = config.snapshot();
        let config_generation = config.generation();
        let initial_trust_mode = session.trust_mode;
        let shared_trust_mode = Arc::new(RwLock::new(initial_trust_mode));
        let session_id = session.id.clone();
        let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel::<Command>();

        let worker_trust_mode = shared_trust_mode.clone();
        // clone 一份 cmd_tx 给 worker，用于在 register_plugin 时注入给插件
        // （作为状态反馈通道，复用同一命令通道，避免新增 channel）。
        let worker_cmd_tx = cmd_tx.clone();
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
                plugins,
                worker_cmd_tx,
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

    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel_flag.clone()
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

impl crate::agent_input::AgentInput for TiangongCore {
    fn deliver(&self, input: crate::agent_input::AgentInputKind) -> bool {
        use crate::agent_input::{AgentInputKind, ApprovalInput, CommandInput, MessageInput};

        // Cancel 副作用内化：在发送命令前先置 cancel_flag（与原 cancel() 方法时序一致）。
        // 这样外部调用 deliver(Cancel) 无需知道 cancel_flag 的存在，封装更完整。
        if matches!(&input, AgentInputKind::Command(CommandInput::Cancel)) {
            self.cancel_flag.store(true, Ordering::Release);
        }

        match input {
            AgentInputKind::Message(MessageInput::UserMessage {
                content,
                message_id,
                media,
            }) => self.send_cmd(Command::Message {
                content,
                message_id,
                media,
            }),
            AgentInputKind::Tool(tool) => self.send_cmd(Command::InjectTool {
                tool_name: tool.tool_name().to_string(),
                payload: tool.render(),
            }),
            AgentInputKind::Approval(ApprovalInput::Response {
                request_id,
                approved,
            }) => self.send_cmd(Command::Approval {
                request_id,
                approved,
            }),
            AgentInputKind::Command(cmd) => match cmd {
                CommandInput::Cancel => self.send_cmd(Command::Cancel),
                CommandInput::CancelAgent { role } => self.send_cmd(Command::CancelAgent { role }),
                CommandInput::UpdateCwd { cwd } => self.send_cmd(Command::UpdateCwd { cwd }),
                CommandInput::ReloadConfig => self.send_cmd(Command::ReloadConfig),
                CommandInput::CompressContext => self.send_cmd(Command::CompressContext),
                CommandInput::ResetContext => self.send_cmd(Command::ResetContext),
            },
        }
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
#[allow(clippy::too_many_arguments)]
fn worker_loop(
    config: CoreConfigProvider,
    session: Session,
    external_tx: StdSender<SessionStreamEvent>,
    cmd_rx: tokio_mpsc::UnboundedReceiver<Command>,
    shared_trust_mode: Arc<RwLock<crate::permission::TrustMode>>,
    memory: WorkerMemoryContext,
    plugins: Vec<Arc<dyn Plugin>>,
    cmd_tx: tokio_mpsc::UnboundedSender<Command>,
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
        plugins,
        cmd_tx,
    ))
}

/// 真正的 async 工作循环
#[allow(clippy::too_many_arguments)]
async fn worker_loop_async(
    config: CoreConfigProvider,
    mut session: Session,
    external_tx: StdSender<SessionStreamEvent>,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<Command>,
    shared_trust_mode: Arc<RwLock<crate::permission::TrustMode>>,
    mut memory: WorkerMemoryContext,
    plugins: Vec<Arc<dyn Plugin>>,
    cmd_tx: tokio_mpsc::UnboundedSender<Command>,
) -> Session {
    let session_id = session.id.clone();
    let mut last_cfg_gen = 0u64;

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
    // on_session_ready 仅在首次 engine build + 插件注册完成后触发一次
    let mut session_ready_fired = false;
    // turn 计数器：每 10 个 turn 触发一次 Meta 反刍（归档低活跃节点）

    // IndexManager 已下沉到 index 插件私有持有，core 不再创建/感知它。
    // 索引的初始扫描、增量写入、finalize 全部由 index 插件的生命周期钩子接管。

    // 内部 StreamEvent 通道 —— 转发线程负责包装 session_id
    // stream_tx 保持 std::sync::mpsc（工具执行等同步代码可直接使用）
    let (stream_tx, stream_rx) = mpsc::channel::<StreamEvent>();
    let fwd_session_id = session_id.clone();
    let fwd_tx = external_tx.clone();
    // 轮次终态记录：转发线程在透传事件的同时，捕获 Done/Error 终态，
    // 供 worker_loop_async 在 execute_turn_async 返回后读取并写入用户消息。
    // 每个轮次开始前由 worker_loop_async 清空。
    let turn_outcome: Arc<Mutex<Option<TurnOutcome>>> = Arc::new(Mutex::new(None));
    let fwd_outcome = turn_outcome.clone();
    let forward_handle = thread::spawn(move || {
        while let Ok(event) = stream_rx.recv() {
            // 捕获终态：Done → Success；Error → 按文案区分 Cancelled/Failed。
            match &event {
                StreamEvent::Done { .. } => {
                    if let Ok(mut slot) = fwd_outcome.lock() {
                        *slot = Some(TurnOutcome::success());
                    }
                }
                StreamEvent::Error { message } => {
                    if let Ok(mut slot) = fwd_outcome.lock() {
                        *slot = Some(TurnOutcome::from_error(message));
                    }
                }
                _ => {}
            }
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

    // 初始索引扫描已由 index 插件的 on_session_ready 钩子接管（见下方钩子调用）。

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

    // on_session_ready 已移至「首次 engine build + 插件注册完成」之后触发，
    // 确保插件在钩子内能读到已注入的 workspace / trust_mode / feedback。

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
            // 遍历插件自注册（issue #156）：在 worker 接收任何用户消息前完成，
            // 根治「注册竞态窗口」。
            //
            // 先注入会话工作目录（set_workspace），再收集三种能力（工具规格/覆盖/Prompt
            // 段落）注册到 engine，最后让插件注入外部能力（PageFetcher 等）。
            let e = engine.as_ref().unwrap();
            let workspace = std::path::Path::new(&session.cwd);
            let workspace = if workspace.is_dir() {
                Some(workspace)
            } else {
                None
            };
            for plugin in &plugins {
                crate::core::plugin::register_plugin(
                    e,
                    plugin.clone(),
                    workspace,
                    cmd_tx.clone(),
                    memory.handle.as_ref(),
                );
            }
            // 先收集插件工具规格 + plugin_injection，作为 MCP 工具的 reserved names，
            // 避免 MCP server 暴露同名工具（read_file/web_fetch/run_command 等）与插件冲突。
            let plugin_specs = e.collect_plugin_tool_specs();
            let injection_spec = crate::core::plugin::injection_tool_spec();
            let reserved_names: std::collections::HashSet<String> = plugin_specs
                .iter()
                .map(|s| s.name.clone())
                .chain(std::iter::once(injection_spec.name.clone()))
                .collect();

            let (all_tools, new_mcp_targets) =
                execution_function_tools(&e.agent_config().mcp, reserved_names);
            let mut new_tools: Vec<ToolSpec> = all_tools;
            // 插件事件注入通道（synthetic tool，声明给模型但不主动调用）
            new_tools.push(injection_spec);
            // 合并 plugin 注册的工具规格
            new_tools.extend(plugin_specs);
            inject_enhanced_tools(&mut new_tools, e);
            // recall_memory 工具规格改由 memory 插件通过 tool_specs() 声明，
            // 随 plugin_specs 自动汇入（且自动进入 reserved_names，MCP 同名工具会被避让）。
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

            // 首次 engine build + 插件注册完成后触发一次 on_session_ready
            //（此时 workspace / trust_mode / feedback 已注入）；后续重建只调 on_engine_rebuilt。
            if !session_ready_fired {
                session_ready_fired = true;
                for plugin in &plugins {
                    plugin.on_session_ready(&mut session);
                }
            }
            // engine 创建/重建完成：回调插件生命周期钩子
            for plugin in &plugins {
                plugin.on_engine_rebuilt(&mut session);
            }
        }

        match cmd {
            Command::UpdateCwd { cwd } => {
                let cwd_changed = cwd != session.cwd;
                session.cwd = cwd;
                apply_session_cwd(&session);
                // 同步把新的工作目录注入到所有插件（文件类插件据此感知会话 workspace）
                let workspace = std::path::Path::new(&session.cwd);
                if workspace.is_dir() {
                    for plugin in &plugins {
                        plugin.set_workspace(workspace);
                    }
                }
                let cfg = config.snapshot();
                memory.handle = get_or_init_memory_async(
                    &cfg,
                    config.generation(),
                    memory.process_type.clone(),
                )
                .await;

                // CWD 变更：回调插件生命周期钩子（index 插件在此重扫工作区索引）
                if cwd_changed {
                    for plugin in &plugins {
                        plugin.on_cwd_changed(&mut session);
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
                // 记录本轮起点；执行结束后用于计算 elapsed_ms 并写入用户消息（持久化，
                // 使历史会话同样展示执行总时长与状态）。
                let turn_started = std::time::Instant::now();
                // 清空上一轮终态记录
                if let Ok(mut slot) = turn_outcome.lock() {
                    *slot = None;
                }
                // 归档附件到本地（图片→images/，PDF/Office→files/）。
                // 必须在 append 之前归档，否则 attachment_notice 引用的是 data URL
                // 而非本地路径，agent 无法读取文件（issue #149）。
                let media = crate::media_archive::archive_input_media_assets(media);
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

                // Turn 开始前：回调插件生命周期钩子（用户消息已写入 session）
                for plugin in &plugins {
                    plugin.on_turn_started(&mut session, turn_start_idx);
                }

                // 执行对话轮次
                execute_turn_async(
                    &mut session,
                    &content,
                    engine.as_ref().unwrap(),
                    &tools,
                    &mcp_targets,
                    &stream_tx,
                    &mut cmd_rx,
                    team_context.clone(),
                )
                .await;

                // 轮次结束：将执行时长与终态写入用户消息（turn 锚点）并落盘。
                // 所有终态分支（成功/失败/取消）都会回到这里，故统一处理。
                let elapsed_ms = turn_started.elapsed().as_millis() as u64;
                let status = turn_outcome
                    .lock()
                    .ok()
                    .and_then(|slot| slot.as_ref().map(|o| o.status))
                    .unwrap_or(tiangong_types::TurnStatus::Success);
                let mut user_msg_updated = false;
                if let Some(msg) = session
                    .messages
                    .iter_mut()
                    .find(|m| m.id == user_msg_id && m.role == MessageRole::User)
                {
                    msg.set_turn_result(elapsed_ms, status);
                    user_msg_updated = true;
                }
                if user_msg_updated {
                    session.persist_to_disk();
                }

                // Turn 完成后：回调插件生命周期钩子（index 插件在此批量写入 Session 索引）
                for plugin in &plugins {
                    plugin.on_turn_finished(&mut session, turn_start_idx);
                }

                // 反刍（Micro/Meta）已下沉到 memory 插件 on_turn_finished 钩子。
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
            Command::InjectTool { tool_name, payload } => {
                crate::react::message::inject_tool_to_session(
                    &mut session,
                    &stream_tx,
                    &tool_name,
                    &payload,
                );
                let _ = stream_tx.send(StreamEvent::Done { usage: None });
            }
            Command::EmitStreamEvent(ev) => {
                let _ = stream_tx.send(ev);
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

    // 会话结束：回调插件生命周期钩子（index 插件在此 finalize Session 索引）。
    // 注意：stream 通道已关闭，钩子内不应再投递流事件。
    for plugin in &plugins {
        plugin.on_session_ended(&mut session);
    }

    session
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
            crate::react::context::rebuild_system_prompt(session, engine);
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

/// 转发线程捕获的单个对话轮次终态。
///
/// `status` 由终态事件推导：`Done` → Success；`Error` 文案含「取消/cancel/abort」
/// 时为 Cancelled，否则为 Failed。worker_loop_async 据此把 `status` 与执行时长
/// 写入用户消息，供前端（含历史会话）展示。
struct TurnOutcome {
    status: tiangong_types::TurnStatus,
}

impl TurnOutcome {
    fn success() -> Self {
        Self {
            status: tiangong_types::TurnStatus::Success,
        }
    }

    /// 根据错误文案区分用户取消与执行失败。
    fn from_error(message: &str) -> Self {
        let lower = message.to_lowercase();
        let is_cancel = lower.contains("取消")
            || lower.contains("cancel")
            || lower.contains("abort")
            || lower.contains("中断");
        Self {
            status: if is_cancel {
                tiangong_types::TurnStatus::Cancelled
            } else {
                tiangong_types::TurnStatus::Failed
            },
        }
    }
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
    team_context: Arc<Mutex<crate::agent_team::lifecycle::TeamContext>>,
) {
    let mut react = crate::react::engine::ReactEngine::new(
        engine.clone(),
        tools.to_vec(),
        mcp_targets.clone(),
        MAX_TOOL_ROUNDS,
        MAX_OUTER_ITERATIONS,
    )
    .with_shared_team(team_context, "main".to_string());
    react
        .execute_turn(session, user_input, stream_tx, cmd_rx)
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
