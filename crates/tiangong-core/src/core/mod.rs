//! TiangongCore：天工智能体核心
//!
//! 单一线程完成所有工作：接收消息 → LLM 调用 → 工具执行 → session 更新 → 推送 StreamEvent
//! CLI / GUI / Server / Connector 统一通过 TiangongCore 运行。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender, Sender as StdSender};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::thread::{self, JoinHandle};
use tokio::sync::mpsc as tokio_mpsc;

use crate::agents::execution_mcp_agent::{McpFunctionTarget, execution_function_tools};
use crate::app_state::formatting::{format_llm_output_message, format_tool_trace_message};
use crate::context::organizer::ContextOrganizer;
use crate::coordinator::TaskCoordinator;
use crate::coordinator::types::CoordinatorTask;
use crate::core_config::{CoreConfig, CoreConfigProvider};
use crate::model::{FunctionToolSpec, ModelClient, ModelRequest, SingleProviderClient, TokenUsage};
use crate::observe::{audit_permission_with_context, audit_tool_execution};
use crate::prompt::PromptAssembler;
use crate::runtime::{LlmOutputRecord, RuntimeEngine, inject_enhanced_tools, use_stream_mode};
use crate::session::{Message, MessageRole, MessageToolCall, Session, now_text};
use crate::stream_throttle::ThrottledStreamSink;
use tiangong_types::{SessionStreamEvent, StreamEvent, StreamToolCall};

// 为了让 drain_pending_commands / execute_turn_standalone 继续使用 std Receiver
use std::sync::mpsc::Receiver;

const MAX_ROUNDS: usize = 20;
const MEMORY_LOOP_FEEDBACK_MAX_CHARS: usize = 12_000;
const TOOL_RESULT_STREAM_MAX_CHARS: usize = 8_000;

type MemoryRecallToolOutput = (crate::tool::ToolResult, tiangong_types::TokenUsage, bool);

/// 进程级 Memory Handle 注册表。
///
/// 按 workspace_id 缓存 MemoryHandle，避免长生命周期 GUI/Server 进程在打开多个
/// workspace 时把后续对话错误绑定到首个 workspace 的 Memory Actor。
static MEMORY_HANDLES: OnceLock<Mutex<HashMap<String, MemoryRegistryEntry>>> = OnceLock::new();

const GLOBAL_MEMORY_WORKSPACE_KEY: &str = "__global__";

#[derive(Clone)]
struct MemoryRegistryEntry {
    handle: tiangong_memory::MemoryHandle,
    workspace_id: Option<String>,
    config_summary: MemoryConfigSummary,
    config_generation: u64,
    created_at: String,
    last_used_at: String,
    restart_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoryConfigSummary {
    model: Option<MemoryModelSummary>,
    embedding: Option<MemoryEmbeddingSummary>,
    vector_mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoryModelSummary {
    base_url: String,
    model: String,
    protocol: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoryEmbeddingSummary {
    base_url: String,
    model: String,
    protocol: String,
    dimension: usize,
}

/// 获取或初始化 workspace 级 Memory Handle。
fn get_or_init_memory(
    config: &CoreConfig,
    config_generation: u64,
    workspace_id: Option<String>,
) -> Option<tiangong_memory::MemoryHandle> {
    let key = memory_registry_key(workspace_id.as_deref());
    let options = config.to_memory_options(workspace_id.clone());
    let config_summary = memory_config_summary_from_options(&options);
    let registry = MEMORY_HANDLES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = match registry.lock() {
        Ok(guard) => guard,
        Err(err) => {
            tracing::warn!("Memory Handle registry 已污染，跳过 Memory 启动: {}", err);
            return None;
        }
    };
    if let Some(entry) = guard.get_mut(&key) {
        entry.last_used_at = now_text();
        let summary_changed = memory_config_changed(&entry.config_summary, &config_summary);
        let generation_changed = entry.config_generation != config_generation;
        if generation_changed {
            entry.config_generation = config_generation;
        }
        if summary_changed {
            if memory_config_can_update_in_place(&entry.config_summary, &config_summary) {
                match entry.handle.reconfigure_blocking(options) {
                    Ok(()) => {
                        tracing::info!(
                            workspace_id = ?entry.workspace_id,
                            created_at = %entry.created_at,
                            last_used_at = %entry.last_used_at,
                            "Memory 配置已原地热更新"
                        );
                        entry.config_summary = config_summary;
                        entry.restart_required = false;
                    }
                    Err(err) => {
                        entry.restart_required = true;
                        tracing::warn!(
                            workspace_id = ?entry.workspace_id,
                            created_at = %entry.created_at,
                            last_used_at = %entry.last_used_at,
                            "Memory 配置热更新失败，继续复用旧 handle 并标记待重启: {}", err
                        );
                    }
                }
            } else {
                entry.restart_required = true;
                tracing::warn!(
                    workspace_id = ?entry.workspace_id,
                    created_at = %entry.created_at,
                    last_used_at = %entry.last_used_at,
                    "Memory 配置变化需要重启 actor，当前继续复用旧 handle 并标记待重启"
                );
            }
        }
        return Some(entry.handle.clone());
    }

    match tiangong_memory::start_with_options(options) {
        Ok(handle) => {
            tracing::info!(workspace_id = ?workspace_id, "Memory Actor 已启动");
            let now = now_text();
            guard.insert(
                key,
                MemoryRegistryEntry {
                    handle: handle.clone(),
                    workspace_id,
                    config_summary,
                    config_generation,
                    created_at: now.clone(),
                    last_used_at: now,
                    restart_required: false,
                },
            );
            Some(handle)
        }
        Err(err) => {
            tracing::warn!(workspace_id = ?workspace_id, "Memory Actor 启动失败（非致命）: {}", err);
            None
        }
    }
}

fn memory_config_changed(running: &MemoryConfigSummary, latest: &MemoryConfigSummary) -> bool {
    running != latest
}

#[cfg(test)]
fn memory_config_summary(config: &CoreConfig) -> MemoryConfigSummary {
    let options = config.to_memory_options(None);
    memory_config_summary_from_options(&options)
}

fn memory_config_summary_from_options(
    options: &tiangong_memory::MemoryOptions,
) -> MemoryConfigSummary {
    MemoryConfigSummary {
        model: options.model.as_ref().map(|model| MemoryModelSummary {
            base_url: model.base_url.clone(),
            model: model.model.clone(),
            protocol: format!("{:?}", model.protocol),
        }),
        embedding: options
            .embedding
            .as_ref()
            .map(|embedding| MemoryEmbeddingSummary {
                base_url: embedding.base_url.clone(),
                model: embedding.model.clone(),
                protocol: format!("{:?}", embedding.protocol),
                dimension: embedding.dimension,
            }),
        vector_mode: format!("{:?}", options.vector_mode),
    }
}

fn memory_config_can_update_in_place(
    _running: &MemoryConfigSummary,
    _latest: &MemoryConfigSummary,
) -> bool {
    // 当前 Memory Actor 已支持模型端点和向量层原地重配置；workspace_id
    // 变化通过 registry key 创建新 entry，不在同一 actor 内热更新。
    true
}

fn memory_registry_key(workspace_id: Option<&str>) -> String {
    workspace_id
        .filter(|workspace_id| !workspace_id.trim().is_empty())
        .unwrap_or(GLOBAL_MEMORY_WORKSPACE_KEY)
        .to_string()
}

/// 统一关闭当前进程内所有 MemoryHandle。
///
/// `TiangongCore::drop` 和 `into_session` 不会关闭 Memory，避免多会话共享 handle
/// 时误关 Actor。应用进程退出时应显式调用这里统一清理 registry。
pub fn shutdown_memory_registry_blocking() {
    let Some(registry) = MEMORY_HANDLES.get() else {
        return;
    };
    let entries = match registry.lock() {
        Ok(mut guard) => guard
            .drain()
            .map(|(_, entry)| entry.handle)
            .collect::<Vec<_>>(),
        Err(err) => {
            tracing::warn!("Memory Handle registry 已污染，无法统一关闭: {}", err);
            return;
        }
    };
    if entries.is_empty() {
        return;
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::warn!("Memory shutdown runtime 构建失败: {}", err);
            return;
        }
    };
    runtime.block_on(async move {
        for handle in entries {
            handle.shutdown().await;
        }
    });
}

fn resolve_memory_workspace_id(session_cwd: &str) -> Option<String> {
    let trimmed = session_cwd.trim();
    if !trimmed.is_empty() {
        return Some(tiangong_memory::workspace_id_from_path(
            &std::path::PathBuf::from(trimmed),
        ));
    }
    std::env::current_dir()
        .ok()
        .map(|p| tiangong_memory::workspace_id_from_path(&p))
}

/// 用户命令
pub(crate) enum Command {
    /// 发送消息
    Message {
        content: String,
        message_id: Option<String>,
    },
    /// 更新当前会话工作目录
    UpdateCwd { cwd: String },
    /// 重新加载共享配置
    ReloadConfig,
    /// 取消当前执行
    Cancel,
    /// 审批响应
    #[allow(dead_code)]
    Approval { request_id: String, approved: bool },
    /// 关闭
    Shutdown,
}

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
}

impl TiangongCore {
    /// 创建新对话
    pub fn new(config: CoreConfigProvider, stream_tx: Sender<SessionStreamEvent>) -> Self {
        let session = Session::new("新对话");
        Self::with_session(config, session, stream_tx)
    }

    /// 从已有 session 创建
    pub fn with_session(
        config: CoreConfigProvider,
        session: Session,
        stream_tx: Sender<SessionStreamEvent>,
    ) -> Self {
        let config_snapshot = config.snapshot();
        let memory_workspace_id = resolve_memory_workspace_id(&session.cwd);
        let memory_handle = get_or_init_memory(
            &config_snapshot,
            config.generation(),
            memory_workspace_id.clone(),
        );
        let initial_trust_mode = config_snapshot.trust_mode;
        let shared_trust_mode = Arc::new(RwLock::new(initial_trust_mode));
        let session_id = session.id.clone();
        let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel::<Command>();

        let worker_trust_mode = shared_trust_mode.clone();
        let worker = thread::spawn(move || {
            worker_loop(
                config,
                session,
                stream_tx,
                cmd_rx,
                worker_trust_mode,
                memory_handle,
                memory_workspace_id,
            )
        });

        Self {
            cmd_tx: Some(cmd_tx),
            worker: Some(worker),
            session_id,
            shared_trust_mode,
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
        })
    }

    pub fn send_message_with_id(&self, content: String, message_id: String) -> bool {
        self.send_cmd(Command::Message {
            content,
            message_id: Some(message_id),
        })
    }

    pub fn update_cwd(&self, cwd: String) -> bool {
        self.send_cmd(Command::UpdateCwd { cwd })
    }

    pub fn reload_config(&self) -> bool {
        self.send_cmd(Command::ReloadConfig)
    }

    pub fn cancel(&self) -> bool {
        self.send_cmd(Command::Cancel)
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
    memory_handle: Option<tiangong_memory::MemoryHandle>,
    memory_workspace_id: Option<String>,
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
        memory_handle,
        memory_workspace_id,
    ))
}

/// 真正的 async 工作循环
async fn worker_loop_async(
    config: CoreConfigProvider,
    mut session: Session,
    external_tx: StdSender<SessionStreamEvent>,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<Command>,
    shared_trust_mode: Arc<RwLock<crate::permission::TrustMode>>,
    mut memory_handle: Option<tiangong_memory::MemoryHandle>,
    mut memory_workspace_id: Option<String>,
) -> Session {
    let session_id = session.id.clone();
    let mut last_cfg_gen = 0u64;
    let mut engine: Option<RuntimeEngine> = None;
    let mut tools: Vec<FunctionToolSpec> = Vec::new();
    let mut mcp_targets: HashMap<String, McpFunctionTarget> = HashMap::new();
    // turn 计数器：每 10 个 turn 触发一次 Meta 反刍（归档低活跃节点）
    let mut turn_count: u32 = 0;

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
            memory_handle = get_or_init_memory(&cfg, cfg_gen, memory_workspace_id.clone());
            engine = Some(build_engine_from_config(
                &cfg,
                &stream_tx,
                shared_trust_mode.clone(),
            ));
            let e = engine.as_ref().unwrap();
            let (all_tools, new_mcp_targets) = execution_function_tools(&e.agent_config().mcp);
            let mut new_tools: Vec<FunctionToolSpec> = all_tools
                .into_iter()
                .filter(|t| t.name != "mark_step_completed")
                .collect();
            inject_enhanced_tools(&mut new_tools, e);
            if memory_handle.is_some() {
                inject_memory_recall_tool(&mut new_tools);
            }
            tools = new_tools;
            mcp_targets = new_mcp_targets;
            last_cfg_gen = cfg_gen;
        }

        match cmd {
            Command::UpdateCwd { cwd } => {
                session.cwd = cwd;
                apply_session_cwd(&session);
                memory_workspace_id = resolve_memory_workspace_id(&session.cwd);
                let cfg = config.snapshot();
                memory_handle =
                    get_or_init_memory(&cfg, config.generation(), memory_workspace_id.clone());
                continue;
            }
            Command::ReloadConfig => {
                continue;
            }
            Command::Message {
                content,
                message_id,
            } => {
                let turn_start_idx = session.messages.len();
                // 记录用户消息
                let user_msg_id = append_or_reuse_user_message(&mut session, &content, message_id);
                // 通知消费端：用户消息已记录（携带 session 中的 message_id）
                let _ = stream_tx.send(StreamEvent::UserMessage {
                    message_id: user_msg_id.clone(),
                    content: content.clone(),
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
                    memory_handle.as_ref(),
                )
                .await;

                // turn 完成后触发 Micro 反刍（fire-and-forget）
                if let Some(handle) = memory_handle.as_ref() {
                    // 显式携带 workspace_id，避免 Actor 固化到启动时工作区造成跨工作区串写
                    let mut turn_result =
                        build_memory_turn_result(&session, turn_start_idx, &content);
                    turn_result.workspace_id = memory_workspace_id.clone();
                    handle.run_micro_rumination(turn_result);

                    // 每 10 个 turn 触发一次 Meta 反刍（归档低活跃节点）
                    turn_count += 1;
                    if turn_count.is_multiple_of(10) {
                        handle.run_meta_rumination();
                        tracing::debug!(turn_count, "Meta 反刍已触发（定期归档）");
                    }
                }
            }
            Command::Cancel => {
                // 当前简单处理：发送错误事件
                let _ = stream_tx.send(StreamEvent::Error {
                    message: "已取消".into(),
                });
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
        }
    }

    // 关闭内部通道，等待转发线程结束
    drop(stream_tx);
    tokio::task::spawn_blocking(|| ()).await.ok(); // yield，让 forward_handle 有机会 drain
    let _ = forward_handle.join();

    // 会话结束 → 触发 Meso 反刍（提炼 Entity/Decision，更新 Workspace Injection）
    // fire-and-forget：handle 仍可使用（Memory Actor 在 registry 中持续运行）
    if let Some(handle) = memory_handle.as_ref()
        && let Some(wid) = &memory_workspace_id
    {
        handle.run_meso_rumination(session_id.clone(), wid.clone());
        tracing::info!(session_id = %session_id, workspace_id = %wid, "Meso 反刍已触发（会话结束）");
    }

    session
}

fn apply_session_cwd(session: &Session) {
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

/// 供 Worker 调用的独立执行函数
///
/// 与 execute_turn 相同的执行逻辑，但返回累计 token 用量。
/// Worker 通过此函数获得与 TiangongCore 完全一致的执行路径。
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_turn_standalone(
    session: &mut Session,
    user_input: &str,
    engine: &RuntimeEngine,
    tools: &[FunctionToolSpec],
    mcp_targets: &HashMap<String, McpFunctionTarget>,
    stream_tx: &StdSender<StreamEvent>,
    cmd_rx: &Receiver<Command>,
    max_rounds: usize,
) -> TokenUsage {
    execute_turn_inner(
        session,
        user_input,
        engine,
        tools,
        mcp_targets,
        stream_tx,
        cmd_rx,
        max_rounds,
        None,
    )
}

/// 执行一个完整的对话轮次（可能多轮工具调用），async 版
///
/// 首先判断是否需要多代理并行执行，如需要则拆分并行；
/// 否则走标准的 ReAct 循环。
/// 每轮之间检查 cmd_rx：新消息注入上下文，cancel 立即生效。
#[allow(clippy::too_many_arguments)]
async fn execute_turn_async(
    session: &mut Session,
    user_input: &str,
    engine: &RuntimeEngine,
    tools: &[FunctionToolSpec],
    mcp_targets: &HashMap<String, McpFunctionTarget>,
    stream_tx: &StdSender<StreamEvent>,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
    memory_handle: Option<&tiangong_memory::MemoryHandle>,
) {
    // 判断是否需要多代理并行执行（同步 LLM 调用，用 block_in_place 保护）
    let should_split = {
        let coordinator = TaskCoordinator::new(engine.clone());
        let task = CoordinatorTask {
            id: scru128::new().to_string(),
            objective: user_input.to_string(),
            user_input: user_input.to_string(),
            context: Vec::new(),
        };
        tokio::task::block_in_place(|| coordinator.should_split(&task))
    };

    if should_split {
        tracing::info!("任务需要拆分，启动多代理并行执行");
        let coordinator = TaskCoordinator::new(engine.clone());
        let task = CoordinatorTask {
            id: scru128::new().to_string(),
            objective: user_input.to_string(),
            user_input: user_input.to_string(),
            context: Vec::new(),
        };
        match tokio::task::block_in_place(|| coordinator.coordinate(task, session, stream_tx)) {
            Ok(result) => {
                session.append_message(MessageRole::Assistant, result.final_response);
                let _ = stream_tx.send(StreamEvent::Done {
                    usage: Some(result.total_usage.clone()),
                });
            }
            Err(err) => {
                tracing::warn!("多代理并行执行失败，回退单代理: {err}");
                execute_turn_inner_async(
                    session,
                    user_input,
                    engine,
                    tools,
                    mcp_targets,
                    stream_tx,
                    cmd_rx,
                    MAX_ROUNDS,
                    memory_handle,
                )
                .await;
            }
        }
        return;
    }

    execute_turn_inner_async(
        session,
        user_input,
        engine,
        tools,
        mcp_targets,
        stream_tx,
        cmd_rx,
        MAX_ROUNDS,
        memory_handle,
    )
    .await;
}

/// 内部执行：标准 ReAct 循环（async 版，由 TiangongCore 主路径使用）
/// LLM 流式调用使用真正的 async stream，通过 tokio::select! 实现任意时刻取消。
#[allow(clippy::too_many_arguments)]
async fn execute_turn_inner_async(
    session: &mut Session,
    _user_input: &str,
    engine: &RuntimeEngine,
    tools: &[FunctionToolSpec],
    mcp_targets: &HashMap<String, McpFunctionTarget>,
    stream_tx: &StdSender<StreamEvent>,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
    max_rounds: usize,
    memory_handle: Option<&tiangong_memory::MemoryHandle>,
) -> TokenUsage {
    let mut loop_context: Vec<Message> = Vec::new();
    let mut round = 0;
    let mut accumulated_usage = TokenUsage::default();
    let mut pending_media_assets: Vec<tiangong_types::MediaAsset> = Vec::new();
    let mut memory_context: Option<String> = None;
    let mut memory_recall_attempted = false;

    'react_loop: loop {
        match drain_pending_commands_async(session, &mut loop_context, stream_tx, cmd_rx) {
            PendingCommandEffect::Terminate => return accumulated_usage,
            PendingCommandEffect::MessageInjected => {
                memory_context = None;
                memory_recall_attempted = false;
            }
            PendingCommandEffect::None => {}
        }

        if round >= max_rounds {
            tokio::task::block_in_place(|| {
                force_final_response(session, &loop_context, engine, stream_tx);
            });
            break;
        }

        let request_tools = tools.to_vec();

        let loop_context_with_memory =
            loop_context_with_memory(&loop_context, memory_context.as_deref());
        let assembler = PromptAssembler::new(engine.context_limit);
        let assembled = assembler.assemble(
            session,
            "",
            request_tools.clone(),
            engine.models_config(),
            engine.agent_config(),
            &loop_context_with_memory,
        );

        let system_prompt = assembled.final_system_prompt();
        let req = ModelRequest {
            session_title: session.title.clone(),
            user_input: assembled.user_input.clone(),
            context: assembled.build_messages(),
            assembled_system_prompt: Some(system_prompt),
            thinking: Some(crate::model::ModelThinkingConfig {
                budget_tokens: 4096,
            }),
        };

        let pending_msg_id = scru128::new().to_string();
        let sink = ThrottledStreamSink::new(pending_msg_id.clone(), stream_tx.clone());

        // ── 真正的 async 流式调用 + select! 取消 ──
        let (chunk_tx, mut chunk_rx) =
            tokio_mpsc::unbounded_channel::<crate::model::ModelStreamChunk>();
        let client = engine.client().clone();
        let req_clone = req.clone();
        let tools_clone = request_tools.clone();
        let llm_fut = tokio::task::spawn(async move {
            client
                .stream_function_calls_with_tool_choice(req_clone, tools_clone, None, chunk_tx)
                .await
        });

        // 同时驱动 LLM 流和命令队列，直到 LLM 完成或收到取消指令。
        // 用户新消息在流式阶段立即落盘并回显，但不打断当前生成；
        // 本次输出完成后再进入下一轮规划处理新消息。
        let mut user_message_injected_during_stream = false;
        let response_result: anyhow::Result<crate::model::ModelFunctionResponse> = loop {
            tokio::select! {
                biased;
                // 优先处理用户命令
                cmd_opt = cmd_rx.recv() => {
                    match cmd_opt {
                        Some(Command::Cancel) | Some(Command::Shutdown) | None => {
                            llm_fut.abort();
                            sink.finish();
                            let _ = stream_tx.send(StreamEvent::Error {
                                message: "已取消".into(),
                            });
                            return accumulated_usage;
                        }
                        // 用户输入到来时不打断当前生成：立即落盘并回显给前端，
                        // 当前 assistant 继续输出；输出完成后马上进入下一轮规划处理新消息。
                        Some(Command::Message { content, message_id }) => {
                            append_user_message_to_loop_context(
                                session,
                                &mut loop_context,
                                stream_tx,
                                content,
                                message_id,
                            );
                            user_message_injected_during_stream = true;
                        }
                        Some(Command::UpdateCwd { cwd }) => {
                            session.cwd = cwd;
                            apply_session_cwd(session);
                        }
                        Some(Command::ReloadConfig) => {}
                        // Approval 在非等待阶段无语义，忽略
                        Some(Command::Approval { .. }) => {}
                    }
                }
                // 处理 LLM chunk
                chunk_opt = chunk_rx.recv() => {
                    match chunk_opt {
                        Some(chunk) => sink.push_chunk(&chunk),
                        None => {
                            // chunk_tx 已关闭，LLM future 即将完成，等待结果
                            let response_result = match llm_fut.await {
                                Ok(r) => r,
                                Err(e) if e.is_cancelled() => {
                                    sink.finish();
                                    let _ = stream_tx.send(StreamEvent::Error {
                                        message: "已取消".into(),
                                    });
                                    return accumulated_usage;
                                }
                                Err(e) => Err(anyhow::anyhow!(e.to_string())),
                            };
                            break response_result;
                        }
                    }
                }
            }
        };
        sink.finish();

        let response = match response_result {
            Ok(r) => r,
            Err(err) => {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: err.to_string(),
                });
                return accumulated_usage;
            }
        };

        accumulated_usage.accumulate(&response.usage);
        round += 1;

        if response.tool_calls.is_empty() {
            if is_synthetic_tool_call_placeholder(&response.text) {
                continue;
            }

            session.append_message_with_id_and_media(
                pending_msg_id,
                MessageRole::Assistant,
                response.text.clone(),
                response.reasoning_content.clone(),
                std::mem::take(&mut pending_media_assets),
            );
            let output = LlmOutputRecord {
                stage: format!("react-round-{round}"),
                content: String::new(),
                reasoning_content: String::new(),
                tool_calls: Vec::new(),
                usage: response.usage.clone(),
            };
            append_runtime_tool_message(session, "llm_output", format_llm_output_message(&output));
            session.persist_to_disk();
            maybe_update_context_summary(session, engine, response.usage.prompt_tokens);

            if user_message_injected_during_stream {
                // 新输入到来后，旧的 recall_memory 注入上下文可能不再相关，避免污染下一轮 prompt
                memory_context = None;
                memory_recall_attempted = false;
                continue 'react_loop;
            }

            let _ = stream_tx.send(StreamEvent::Done {
                usage: Some(accumulated_usage.clone()),
            });
            return accumulated_usage;
        }

        // 工具调用
        let executable_calls = response.tool_calls.iter().collect::<Vec<_>>();
        if executable_calls.is_empty() {
            let _ = stream_tx.send(StreamEvent::Error {
                message: "模型没有返回可执行工具调用，任务已停止".to_string(),
            });
            return accumulated_usage;
        }
        let tool_names: Vec<String> = executable_calls.iter().map(|c| c.name.clone()).collect();
        let output = LlmOutputRecord {
            stage: format!("react-round-{round}"),
            content: response.text.clone(),
            reasoning_content: response.reasoning_content.clone(),
            tool_calls: tool_names.clone(),
            usage: response.usage.clone(),
        };
        append_runtime_tool_message_with_reasoning(
            session,
            "llm_output",
            format_llm_output_message(&output),
            response.reasoning_content.clone(),
        );
        let _ = stream_tx.send(StreamEvent::ToolCalls {
            message_id: pending_msg_id.clone(),
            names: tool_names.clone(),
            calls: executable_calls
                .iter()
                .map(|call| StreamToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                })
                .collect(),
            usage: Some(response.usage.clone()),
        });
        append_assistant_tool_call_message(
            session,
            pending_msg_id.clone(),
            &response.text,
            &response.reasoning_content,
            &executable_calls,
        );

        // 执行工具
        for call in executable_calls {
            match drain_pending_commands_async(session, &mut loop_context, stream_tx, cmd_rx) {
                PendingCommandEffect::Terminate => return accumulated_usage,
                PendingCommandEffect::MessageInjected => {
                    memory_context = None;
                    memory_recall_attempted = false;
                    session.persist_to_disk();
                    continue 'react_loop;
                }
                PendingCommandEffect::None => {}
            }

            let args_summary = format_call_args_summary(call);
            let (target_scope, target_summary) = infer_audit_target(call);
            let normalized_target = normalize_permission_target(
                session,
                target_scope.as_deref(),
                target_summary.as_deref(),
            );

            use crate::permission::PermissionDecision;
            let decision = evaluate_tool_permission(
                engine,
                &call.name,
                target_scope.as_deref(),
                normalized_target.as_deref(),
            );
            let trust_mode = format!("{:?}", engine.permission_gate().trust_mode());
            match decision {
                PermissionDecision::Approved => {
                    audit_permission_with_context(
                        &session.id,
                        &call.name,
                        "approved",
                        &trust_mode,
                        (!args_summary.is_empty()).then_some(args_summary.as_str()),
                        target_scope.as_deref(),
                        normalized_target.as_deref().or(target_summary.as_deref()),
                    );
                }
                PermissionDecision::Denied { reason } => {
                    audit_permission_with_context(
                        &session.id,
                        &call.name,
                        "denied",
                        &trust_mode,
                        (!args_summary.is_empty()).then_some(args_summary.as_str()),
                        target_scope.as_deref(),
                        normalized_target.as_deref().or(target_summary.as_deref()),
                    );
                    let _ = stream_tx.send(StreamEvent::ToolResult {
                        name: call.name.clone(),
                        tool_call_id: Some(call.id.clone()),
                        ok: false,
                        output: format!("权限拒绝：{reason}"),
                        full_output: None,
                    });
                    append_tool_result_message(
                        session,
                        &call.id,
                        &call.name,
                        format!("权限拒绝：{reason}"),
                        true,
                    );
                    continue;
                }
                PermissionDecision::NeedsApproval { request_id } => {
                    audit_permission_with_context(
                        &session.id,
                        &call.name,
                        "needs_approval",
                        &trust_mode,
                        (!args_summary.is_empty()).then_some(args_summary.as_str()),
                        target_scope.as_deref(),
                        normalized_target.as_deref().or(target_summary.as_deref()),
                    );
                    crate::approval_store::add_pending(
                        &session.id,
                        crate::session::PendingApproval {
                            request_id: request_id.clone(),
                            tool_name: call.name.clone(),
                            tool_args_summary: args_summary.clone(),
                            created_at: now_text(),
                        },
                    );
                    let _ = stream_tx.send(StreamEvent::ApprovalNeeded {
                        request_id: request_id.clone(),
                        tool_name: call.name.clone(),
                        args_summary: args_summary.clone(),
                    });

                    // async 等待用户审批
                    let approved = loop {
                        match cmd_rx.recv().await {
                            Some(Command::Approval {
                                request_id: rid,
                                approved,
                            }) if rid == request_id => {
                                break approved;
                            }
                            Some(Command::Cancel) | Some(Command::Shutdown) | None => {
                                let _ = stream_tx.send(StreamEvent::Error {
                                    message: "已取消".into(),
                                });
                                return accumulated_usage;
                            }
                            Some(Command::Message {
                                content,
                                message_id,
                            }) => {
                                append_user_message_to_loop_context(
                                    session,
                                    &mut loop_context,
                                    stream_tx,
                                    content,
                                    message_id,
                                );
                            }
                            Some(Command::UpdateCwd { cwd }) => {
                                session.cwd = cwd;
                                apply_session_cwd(session);
                            }
                            Some(Command::ReloadConfig) => {}
                            Some(Command::Approval { .. }) => {}
                        }
                    };

                    crate::approval_store::remove_pending(&session.id, &request_id);

                    if !approved {
                        audit_tool_execution(
                            &session.id,
                            &call.name,
                            false,
                            (!args_summary.is_empty()).then_some(args_summary.as_str()),
                            target_scope.as_deref(),
                            normalized_target.as_deref().or(target_summary.as_deref()),
                            "用户拒绝执行",
                        );
                        append_runtime_tool_message(
                            session,
                            &call.name,
                            format!("工具 {} 被用户拒绝执行", call.name),
                        );
                        append_tool_result_message(
                            session,
                            &call.id,
                            &call.name,
                            "用户拒绝执行".to_string(),
                            true,
                        );
                        session.persist_to_disk();
                        let _ = stream_tx.send(StreamEvent::ToolResult {
                            name: call.name.clone(),
                            tool_call_id: Some(call.id.clone()),
                            ok: false,
                            output: "用户拒绝执行".to_string(),
                            full_output: None,
                        });
                        let _ = stream_tx.send(StreamEvent::Done {
                            usage: Some(accumulated_usage.clone()),
                        });
                        return accumulated_usage;
                    }
                }
            }

            let _ = stream_tx.send(StreamEvent::ToolStart {
                name: call.name.clone(),
                args_summary: args_summary.clone(),
            });

            let (result, memory_tool_usage, allow_memory_context) =
                tokio::task::block_in_place(|| {
                    if call.name == "recall_memory" {
                        if memory_recall_attempted {
                            duplicate_memory_recall_tool_result()
                        } else {
                            memory_recall_attempted = true;
                            execute_memory_recall_tool(call, memory_handle, session)
                        }
                    } else {
                        (
                            engine.execute_tool_call(call, mcp_targets, &engine.agent_config().mcp),
                            tiangong_types::TokenUsage::default(),
                            false,
                        )
                    }
                });
            accumulated_usage.accumulate(&memory_tool_usage);

            audit_tool_execution(
                &session.id,
                &call.name,
                result.ok,
                (!args_summary.is_empty()).then_some(args_summary.as_str()),
                target_scope.as_deref(),
                normalized_target.as_deref().or(target_summary.as_deref()),
                &result.summary,
            );
            let _ = stream_tx.send(StreamEvent::ToolResult {
                name: call.name.clone(),
                tool_call_id: Some(call.id.clone()),
                ok: result.ok,
                output: tool_result_stream_output(&result),
                full_output: Some(tool_result_full_output(&result)),
            });
            append_tool_result_message(
                session,
                &call.id,
                &call.name,
                tool_result_provider_text(&call.name, &result, allow_memory_context),
                !result.ok,
            );
            append_runtime_tool_message(session, &call.name, format_tool_trace_message(&result));

            if result.ok {
                pending_media_assets.extend(parse_media_assets_from_tool_result(
                    &call.name,
                    &result.stdout,
                    &result.summary,
                ));
            }
            if call.name == "recall_memory"
                && result.ok
                && allow_memory_context
                && !result.stdout.trim().is_empty()
            {
                memory_context = Some(result.stdout.clone());
            }
            maybe_update_context_summary(session, engine, response.usage.prompt_tokens);

            match drain_pending_commands_async(session, &mut loop_context, stream_tx, cmd_rx) {
                PendingCommandEffect::Terminate => return accumulated_usage,
                PendingCommandEffect::MessageInjected => {
                    memory_context = None;
                    memory_recall_attempted = false;
                    session.persist_to_disk();
                    continue 'react_loop;
                }
                PendingCommandEffect::None => {}
            }
        }

        session.persist_to_disk();
    }

    accumulated_usage
}

/// 内部执行：标准 ReAct 循环（同步版，由 execute_turn_standalone / Worker 使用）
#[allow(clippy::too_many_arguments)]
fn execute_turn_inner(
    session: &mut Session,
    _user_input: &str,
    engine: &RuntimeEngine,
    tools: &[FunctionToolSpec],
    mcp_targets: &HashMap<String, McpFunctionTarget>,
    stream_tx: &StdSender<StreamEvent>,
    cmd_rx: &Receiver<Command>,
    max_rounds: usize,
    memory_handle: Option<&tiangong_memory::MemoryHandle>,
) -> TokenUsage {
    let mut loop_context: Vec<Message> = Vec::new();
    let mut round = 0;
    let mut accumulated_usage = TokenUsage::default();
    let mut pending_media_assets: Vec<tiangong_types::MediaAsset> = Vec::new();
    let mut memory_context: Option<String> = None;
    let mut memory_recall_attempted = false;

    'react_loop: loop {
        match drain_pending_commands(session, &mut loop_context, stream_tx, cmd_rx) {
            PendingCommandEffect::Terminate => return accumulated_usage,
            PendingCommandEffect::MessageInjected => {
                memory_context = None;
                memory_recall_attempted = false;
            }
            PendingCommandEffect::None => {}
        }

        if round >= max_rounds {
            // 超限：强制最终回复
            force_final_response(session, &loop_context, engine, stream_tx);
            break;
        }

        // 构建 prompt。用户输入和工具结果统一通过 session/loop messages 传递；
        // 工具执行后的下一轮允许模型直接最终回复，避免为了满足 forced tool choice
        // 而重复调用无关工具。
        let request_tools = tools.to_vec();

        let loop_context_with_memory =
            loop_context_with_memory(&loop_context, memory_context.as_deref());
        let assembler = PromptAssembler::new(engine.context_limit);
        let assembled = assembler.assemble(
            session,
            "",
            request_tools.clone(),
            engine.models_config(),
            engine.agent_config(),
            &loop_context_with_memory,
        );

        let system_prompt = assembled.final_system_prompt();
        let req = ModelRequest {
            session_title: session.title.clone(),
            user_input: assembled.user_input.clone(),
            context: assembled.build_messages(),
            assembled_system_prompt: Some(system_prompt),
            thinking: Some(crate::model::ModelThinkingConfig {
                budget_tokens: 4096,
            }),
        };

        // 预生成本轮 assistant 消息 ID（Delta/Reasoning 事件先于消息创建）
        let pending_msg_id = scru128::new().to_string();

        // LLM 流式调用
        // stream_cancel：流期间收到 Cancel/Shutdown 时设为 true，后续 chunk 不再入 sink
        // cmds_during_stream：流期间截获的非终止命令（如 Message），LLM 完成后统一处理
        let sink = ThrottledStreamSink::new(pending_msg_id.clone(), stream_tx.clone());
        let stream_cancel = Arc::new(AtomicBool::new(false));
        let stream_cancel_c = stream_cancel.clone();
        let cmds_during_stream: Arc<Mutex<Vec<Command>>> = Arc::new(Mutex::new(Vec::new()));
        let cmds_during_stream_c = cmds_during_stream.clone();
        let response_result = engine
            .client()
            .complete_with_functions_stream_with_tool_choice(
                &req,
                &request_tools,
                None,
                &mut |delta| {
                    // 每个 chunk 回调时顺带排空命令队列，实现任意时刻响应
                    while let Ok(cmd) = cmd_rx.try_recv() {
                        match cmd {
                            Command::Cancel => {
                                stream_cancel_c.store(true, Ordering::Release);
                            }
                            Command::Shutdown => {
                                stream_cancel_c.store(true, Ordering::Release);
                            }
                            other => {
                                if let Ok(mut q) = cmds_during_stream_c.lock() {
                                    q.push(other);
                                }
                            }
                        }
                    }
                    if !stream_cancel_c.load(Ordering::Acquire) {
                        sink.push_chunk(delta);
                    }
                },
            );
        sink.finish();
        // Cancel/Shutdown 在流期间被触发
        if stream_cancel.load(Ordering::Acquire) {
            let _ = stream_tx.send(StreamEvent::Error {
                message: "已取消".into(),
            });
            return accumulated_usage;
        }
        // 处理流期间截获的用户消息，有新消息则立即重新规划
        {
            let had_messages = if let Ok(mut q) = cmds_during_stream.lock() {
                let mut has = false;
                for cmd in q.drain(..) {
                    if let Command::Message {
                        content,
                        message_id,
                    } = cmd
                    {
                        has = true;
                        append_user_message_to_loop_context(
                            session,
                            &mut loop_context,
                            stream_tx,
                            content,
                            message_id,
                        );
                    }
                }
                has
            } else {
                false
            };
            if had_messages {
                memory_context = None;
                memory_recall_attempted = false;
                continue 'react_loop;
            }
        }
        let response = match response_result {
            Ok(r) => r,
            Err(err) => {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: err.to_string(),
                });
                return accumulated_usage;
            }
        };

        accumulated_usage.accumulate(&response.usage);
        round += 1;

        if response.tool_calls.is_empty() {
            if is_synthetic_tool_call_placeholder(&response.text) {
                continue;
            }

            // 最终回复：使用预生成的 ID 记录到 session
            session.append_message_with_id_and_media(
                pending_msg_id,
                MessageRole::Assistant,
                response.text.clone(),
                response.reasoning_content.clone(),
                std::mem::take(&mut pending_media_assets),
            );
            // 记录 LLM 输出
            let output = LlmOutputRecord {
                stage: format!("react-round-{round}"),
                content: String::new(),
                reasoning_content: String::new(),
                tool_calls: Vec::new(),
                usage: response.usage.clone(),
            };
            append_runtime_tool_message(session, "llm_output", format_llm_output_message(&output));

            // 最终回复落盘（确保崩溃时不丢失）
            session.persist_to_disk();
            maybe_update_context_summary(session, engine, response.usage.prompt_tokens);

            let _ = stream_tx.send(StreamEvent::Done {
                usage: Some(accumulated_usage.clone()),
            });
            return accumulated_usage;
        }

        // 工具调用
        let executable_calls = response.tool_calls.iter().collect::<Vec<_>>();
        if executable_calls.is_empty() {
            let _ = stream_tx.send(StreamEvent::Error {
                message: "模型没有返回可执行工具调用，任务已停止".to_string(),
            });
            return accumulated_usage;
        }
        let tool_names: Vec<String> = executable_calls.iter().map(|c| c.name.clone()).collect();

        // 记录 LLM 输出到 session
        let output = LlmOutputRecord {
            stage: format!("react-round-{round}"),
            content: response.text.clone(),
            reasoning_content: response.reasoning_content.clone(),
            tool_calls: tool_names.clone(),
            usage: response.usage.clone(),
        };
        append_runtime_tool_message_with_reasoning(
            session,
            "llm_output",
            format_llm_output_message(&output),
            response.reasoning_content.clone(),
        );

        // 推送工具调用事件
        let _ = stream_tx.send(StreamEvent::ToolCalls {
            message_id: pending_msg_id.clone(),
            names: tool_names.clone(),
            calls: executable_calls
                .iter()
                .map(|call| StreamToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                })
                .collect(),
            usage: Some(response.usage.clone()),
        });

        append_assistant_tool_call_message(
            session,
            pending_msg_id.clone(),
            &response.text,
            &response.reasoning_content,
            &executable_calls,
        );

        // 执行工具
        for call in executable_calls {
            match drain_pending_commands(session, &mut loop_context, stream_tx, cmd_rx) {
                PendingCommandEffect::Terminate => return accumulated_usage,
                PendingCommandEffect::MessageInjected => {
                    memory_context = None;
                    memory_recall_attempted = false;
                    session.persist_to_disk();
                    continue 'react_loop;
                }
                PendingCommandEffect::None => {}
            }

            let args_summary = format_call_args_summary(call);
            let (target_scope, target_summary) = infer_audit_target(call);
            let normalized_target = normalize_permission_target(
                session,
                target_scope.as_deref(),
                target_summary.as_deref(),
            );

            // 权限检查（在执行前完成，trust_mode 通过 shared Arc 实时生效）
            use crate::permission::PermissionDecision;
            let decision = evaluate_tool_permission(
                engine,
                &call.name,
                target_scope.as_deref(),
                normalized_target.as_deref(),
            );
            let trust_mode = format!("{:?}", engine.permission_gate().trust_mode());
            match decision {
                PermissionDecision::Approved => {
                    audit_permission_with_context(
                        &session.id,
                        &call.name,
                        "approved",
                        &trust_mode,
                        (!args_summary.is_empty()).then_some(args_summary.as_str()),
                        target_scope.as_deref(),
                        normalized_target.as_deref().or(target_summary.as_deref()),
                    );
                }
                PermissionDecision::Denied { reason } => {
                    audit_permission_with_context(
                        &session.id,
                        &call.name,
                        "denied",
                        &trust_mode,
                        (!args_summary.is_empty()).then_some(args_summary.as_str()),
                        target_scope.as_deref(),
                        normalized_target.as_deref().or(target_summary.as_deref()),
                    );
                    let _ = stream_tx.send(StreamEvent::ToolResult {
                        name: call.name.clone(),
                        tool_call_id: Some(call.id.clone()),
                        ok: false,
                        output: format!("权限拒绝：{reason}"),
                        full_output: None,
                    });
                    append_tool_result_message(
                        session,
                        &call.id,
                        &call.name,
                        format!("权限拒绝：{reason}"),
                        true,
                    );
                    continue;
                }
                PermissionDecision::NeedsApproval { request_id } => {
                    audit_permission_with_context(
                        &session.id,
                        &call.name,
                        "needs_approval",
                        &trust_mode,
                        (!args_summary.is_empty()).then_some(args_summary.as_str()),
                        target_scope.as_deref(),
                        normalized_target.as_deref().or(target_summary.as_deref()),
                    );
                    // 记录待审批状态（独立存储，崩溃恢复时可重新展示）
                    crate::approval_store::add_pending(
                        &session.id,
                        crate::session::PendingApproval {
                            request_id: request_id.clone(),
                            tool_name: call.name.clone(),
                            tool_args_summary: args_summary.clone(),
                            created_at: now_text(),
                        },
                    );

                    // 发送审批请求
                    let _ = stream_tx.send(StreamEvent::ApprovalNeeded {
                        request_id: request_id.clone(),
                        tool_name: call.name.clone(),
                        args_summary: args_summary.clone(),
                    });

                    // 阻塞等待用户审批
                    let approved = loop {
                        match cmd_rx.recv() {
                            Ok(Command::Approval {
                                request_id: rid,
                                approved,
                            }) if rid == request_id => {
                                break approved;
                            }
                            Ok(Command::Cancel) => {
                                let _ = stream_tx.send(StreamEvent::Error {
                                    message: "已取消".into(),
                                });
                                return accumulated_usage;
                            }
                            Ok(Command::Message {
                                content,
                                message_id,
                            }) => {
                                append_user_message_to_loop_context(
                                    session,
                                    &mut loop_context,
                                    stream_tx,
                                    content,
                                    message_id,
                                );
                            }
                            Ok(Command::UpdateCwd { cwd }) => {
                                session.cwd = cwd;
                                apply_session_cwd(session);
                            }
                            Ok(Command::ReloadConfig) => {}
                            Ok(Command::Shutdown) => return accumulated_usage,
                            _ => {}
                        }
                    };

                    // 审批完成，清除 pending 状态
                    crate::approval_store::remove_pending(&session.id, &request_id);

                    if !approved {
                        audit_tool_execution(
                            &session.id,
                            &call.name,
                            false,
                            (!args_summary.is_empty()).then_some(args_summary.as_str()),
                            target_scope.as_deref(),
                            normalized_target.as_deref().or(target_summary.as_deref()),
                            "用户拒绝执行",
                        );
                        append_runtime_tool_message(
                            session,
                            &call.name,
                            format!("工具 {} 被用户拒绝执行", call.name),
                        );
                        append_tool_result_message(
                            session,
                            &call.id,
                            &call.name,
                            "用户拒绝执行".to_string(),
                            true,
                        );
                        session.persist_to_disk();

                        let _ = stream_tx.send(StreamEvent::ToolResult {
                            name: call.name.clone(),
                            tool_call_id: Some(call.id.clone()),
                            ok: false,
                            output: "用户拒绝执行".to_string(),
                            full_output: None,
                        });
                        // 拒绝后结束本轮，避免 LLM 再次调用同工具形成死循环
                        let _ = stream_tx.send(StreamEvent::Done {
                            usage: Some(accumulated_usage.clone()),
                        });
                        return accumulated_usage;
                    }
                }
            }

            let _ = stream_tx.send(StreamEvent::ToolStart {
                name: call.name.clone(),
                args_summary: args_summary.clone(),
            });

            let (result, memory_tool_usage, allow_memory_context) = if call.name == "recall_memory"
            {
                if memory_recall_attempted {
                    duplicate_memory_recall_tool_result()
                } else {
                    memory_recall_attempted = true;
                    execute_memory_recall_tool(call, memory_handle, session)
                }
            } else {
                (
                    engine.execute_tool_call(call, mcp_targets, &engine.agent_config().mcp),
                    tiangong_types::TokenUsage::default(),
                    false,
                )
            };
            // 累加 memory 阶段产生的 token 消耗
            accumulated_usage.accumulate(&memory_tool_usage);

            audit_tool_execution(
                &session.id,
                &call.name,
                result.ok,
                (!args_summary.is_empty()).then_some(args_summary.as_str()),
                target_scope.as_deref(),
                normalized_target.as_deref().or(target_summary.as_deref()),
                &result.summary,
            );

            let _ = stream_tx.send(StreamEvent::ToolResult {
                name: call.name.clone(),
                tool_call_id: Some(call.id.clone()),
                ok: result.ok,
                output: tool_result_stream_output(&result),
                full_output: Some(tool_result_full_output(&result)),
            });

            // 记录到 session
            append_tool_result_message(
                session,
                &call.id,
                &call.name,
                tool_result_provider_text(&call.name, &result, allow_memory_context),
                !result.ok,
            );
            append_runtime_tool_message(session, &call.name, format_tool_trace_message(&result));

            // 记录到 loop_context（完整内容，截断由上下文压缩器处理）
            // 媒体生成工具使用摘要反馈（避免 base64 数据污染上下文）
            if result.ok {
                pending_media_assets.extend(parse_media_assets_from_tool_result(
                    &call.name,
                    &result.stdout,
                    &result.summary,
                ));
            }
            if call.name == "recall_memory"
                && result.ok
                && allow_memory_context
                && !result.stdout.trim().is_empty()
            {
                memory_context = Some(result.stdout.clone());
            }
            maybe_update_context_summary(session, engine, response.usage.prompt_tokens);

            match drain_pending_commands(session, &mut loop_context, stream_tx, cmd_rx) {
                PendingCommandEffect::Terminate => return accumulated_usage,
                PendingCommandEffect::MessageInjected => {
                    memory_context = None;
                    memory_recall_attempted = false;
                    session.persist_to_disk();
                    continue 'react_loop;
                }
                PendingCommandEffect::None => {}
            }
        }

        // 工具调用完成后增量持久化（防止崩溃丢失中间数据）
        session.persist_to_disk();

        // 继续下一轮
    }

    accumulated_usage
}

enum PendingCommandEffect {
    None,
    MessageInjected,
    Terminate,
}

fn drain_pending_commands_async(
    session: &mut Session,
    loop_context: &mut Vec<Message>,
    stream_tx: &StdSender<StreamEvent>,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
) -> PendingCommandEffect {
    let mut injected_message = false;

    while let Ok(cmd) = cmd_rx.try_recv() {
        match cmd {
            Command::Cancel => {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: "已取消".into(),
                });
                return PendingCommandEffect::Terminate;
            }
            Command::Shutdown => return PendingCommandEffect::Terminate,
            Command::Message {
                content,
                message_id,
            } => {
                append_user_message_to_loop_context(
                    session,
                    loop_context,
                    stream_tx,
                    content,
                    message_id,
                );
                injected_message = true;
            }
            Command::UpdateCwd { cwd } => {
                session.cwd = cwd;
                apply_session_cwd(session);
            }
            Command::ReloadConfig => {}
            Command::Approval { .. } => {}
        }
    }

    if injected_message {
        PendingCommandEffect::MessageInjected
    } else {
        PendingCommandEffect::None
    }
}

fn drain_pending_commands(
    session: &mut Session,
    loop_context: &mut Vec<Message>,
    stream_tx: &StdSender<StreamEvent>,
    cmd_rx: &Receiver<Command>,
) -> PendingCommandEffect {
    let mut injected_message = false;

    while let Ok(cmd) = cmd_rx.try_recv() {
        match cmd {
            Command::Cancel => {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: "已取消".into(),
                });
                return PendingCommandEffect::Terminate;
            }
            Command::Shutdown => return PendingCommandEffect::Terminate,
            Command::Message {
                content,
                message_id,
            } => {
                append_user_message_to_loop_context(
                    session,
                    loop_context,
                    stream_tx,
                    content,
                    message_id,
                );
                injected_message = true;
            }
            Command::UpdateCwd { cwd } => {
                session.cwd = cwd;
                apply_session_cwd(session);
            }
            Command::ReloadConfig => {}
            Command::Approval { .. } => {}
        }
    }

    if injected_message {
        PendingCommandEffect::MessageInjected
    } else {
        PendingCommandEffect::None
    }
}

fn append_user_message_to_loop_context(
    session: &mut Session,
    loop_context: &mut Vec<Message>,
    stream_tx: &StdSender<StreamEvent>,
    content: String,
    message_id: Option<String>,
) {
    let loop_message_id = append_or_reuse_user_message(session, &content, message_id);
    let _ = stream_tx.send(StreamEvent::UserMessage {
        message_id: loop_message_id.clone(),
        content: content.clone(),
    });
    loop_context.push(Message {
        id: loop_message_id,
        role: MessageRole::User,
        content,
        reasoning_content: String::new(),
        worker_id: None,
        media: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: None,
        tool_result_is_error: false,
        compact: false,
        created_at: now_text(),
    });
}

fn append_or_reuse_user_message(
    session: &mut Session,
    content: &str,
    message_id: Option<String>,
) -> String {
    if let Some(message_id) = message_id {
        if !session.messages.iter().any(|msg| msg.id == message_id) {
            session.append_message_with_id(
                message_id.clone(),
                MessageRole::User,
                content.to_string(),
                String::new(),
            );
        }
        return message_id;
    }

    session.append_message(MessageRole::User, content.to_string());
    session
        .messages
        .last()
        .map(|m| m.id.clone())
        .unwrap_or_default()
}

fn is_synthetic_tool_call_placeholder(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with("[调用工具:") && trimmed.ends_with(']')
}

fn append_assistant_tool_call_message(
    session: &mut Session,
    message_id: String,
    text: &str,
    reasoning_content: &str,
    calls: &[&crate::model::ModelFunctionCall],
) {
    let tool_calls = calls
        .iter()
        .map(|call| MessageToolCall {
            id: call.id.clone(),
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        })
        .collect::<Vec<_>>();
    if tool_calls.is_empty() {
        return;
    }

    let mut message = Message::with_reasoning(
        MessageRole::Assistant,
        text.trim().to_string(),
        reasoning_content.trim().to_string(),
    );
    message.id = message_id;
    message.tool_calls = tool_calls;
    session.messages.push(message);
    session.updated_at = now_text();
}

fn append_tool_result_message(
    session: &mut Session,
    tool_call_id: &str,
    tool_name: &str,
    text: String,
    is_error: bool,
) {
    let mut message = Message::new(MessageRole::Tool, text);
    message.tool_call_id = Some(tool_call_id.to_string());
    message.tool_name = Some(tool_name.to_string());
    message.tool_result_is_error = is_error;
    session.messages.push(message);
    session.updated_at = now_text();
}

fn append_runtime_tool_message(session: &mut Session, tool_name: &str, content: String) {
    let mut message = Message::new(MessageRole::Tool, content);
    message.tool_name = Some(tool_name.to_string());
    session.messages.push(message);
    session.updated_at = now_text();
}

fn append_runtime_tool_message_with_reasoning(
    session: &mut Session,
    tool_name: &str,
    content: String,
    reasoning_content: String,
) {
    let mut message = Message::with_reasoning(MessageRole::Tool, content, reasoning_content);
    message.tool_name = Some(tool_name.to_string());
    session.messages.push(message);
    session.updated_at = now_text();
}

fn tool_result_provider_text(
    tool_name: &str,
    result: &crate::tool::ToolResult,
    allow_memory_context: bool,
) -> String {
    if tool_name == "recall_memory" {
        build_memory_recall_feedback(&result.stdout, allow_memory_context)
    } else if is_media_tool_name(tool_name) && result.ok {
        format!(
            "工具 {tool_name} 执行成功：{}。媒体内容已生成并交付给用户，不要再次调用该工具。",
            result.summary
        )
    } else {
        tool_result_full_output(result)
    }
}

fn is_media_tool_name(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "generate_image" | "generate_video" | "text_to_speech" | "speech_to_text"
    )
}

fn maybe_update_context_summary(
    session: &mut Session,
    engine: &RuntimeEngine,
    actual_prompt_tokens: usize,
) {
    let organizer = ContextOrganizer::new(engine.context_limit)
        .with_threshold(0.95)
        .with_keep_recent_turns(6);
    match organizer.maybe_update_summary(session, engine.client(), actual_prompt_tokens) {
        Ok(true) => {
            session.persist_to_disk();
            tracing::info!(
                session_id = %session.id,
                prompt_tokens = actual_prompt_tokens,
                threshold_tokens = organizer.token_threshold(),
                summary_up_to = session.summary_up_to,
                "上下文达到压缩阈值，已更新早期对话摘要"
            );
        }
        Ok(false) => {}
        Err(err) => {
            tracing::warn!(
                session_id = %session.id,
                error = %err,
                "上下文压缩失败，继续使用原始上下文"
            );
        }
    }
}

fn loop_context_with_memory(
    loop_context: &[Message],
    memory_context: Option<&str>,
) -> Vec<Message> {
    let mut messages = loop_context.to_vec();
    let Some(ctx) = memory_context.map(str::trim).filter(|ctx| !ctx.is_empty()) else {
        return messages;
    };

    messages.insert(
        0,
        Message {
            id: scru128::new().to_string(),
            role: MessageRole::Tool,
            content: format!(
                "<memory-recall>\n{ctx}\n</memory-recall>\n\
请基于以上 recall_memory 检索结果继续完成用户原始目标；不要再次调用 recall_memory，除非用户提出新的历史查询。"
            ),
            reasoning_content: String::new(),
            worker_id: None,
            media: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: Some("recall_memory".to_string()),
            tool_result_is_error: false,
            compact: false,
            created_at: now_text(),
        },
    );
    messages
}

/// 超限时强制最终回复
fn force_final_response(
    session: &mut Session,
    loop_context: &[Message],
    engine: &RuntimeEngine,
    stream_tx: &StdSender<StreamEvent>,
) {
    // 将强制回复提示作为内部 tool 上下文注入，避免污染稳定 system prompt。
    let mut final_context = loop_context.to_vec();
    final_context.push(Message {
        id: scru128::new().to_string(),
        role: MessageRole::Tool,
        content:
            "<system-reminder>\n请基于以上所有工具执行结果，直接给出最终回复。\n</system-reminder>"
                .to_string(),
        reasoning_content: String::new(),
        worker_id: None,
        media: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: Some("force_final_response".to_string()),
        tool_result_is_error: false,
        compact: false,
        created_at: now_text(),
    });

    let assembler = PromptAssembler::new(engine.context_limit);
    let assembled = assembler.assemble(
        session,
        "",
        Vec::new(),
        engine.models_config(),
        engine.agent_config(),
        &final_context,
    );

    let system_prompt = assembled.final_system_prompt();
    let req = ModelRequest {
        session_title: session.title.clone(),
        user_input: assembled.user_input.clone(),
        context: assembled.build_messages(),
        assembled_system_prompt: Some(system_prompt),
        thinking: Some(crate::model::ModelThinkingConfig {
            budget_tokens: 4096,
        }),
    };

    // 预生成 message_id
    let pending_msg_id = scru128::new().to_string();

    let resp = if use_stream_mode() {
        let sink = ThrottledStreamSink::new(pending_msg_id.clone(), stream_tx.clone());
        let response_result = engine
            .client()
            .complete_stream_with_callback(&req, |delta| {
                sink.push_chunk(delta);
            });
        sink.finish();
        match response_result {
            Ok(r) => r,
            Err(err) => {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: err.to_string(),
                });
                return;
            }
        }
    } else {
        let msg_id_non_stream = pending_msg_id.clone();
        match engine.client().complete(&req) {
            Ok(r) => {
                if !r.text.is_empty() {
                    let _ = stream_tx.send(StreamEvent::Delta {
                        message_id: msg_id_non_stream,
                        content: r.text.clone(),
                    });
                }
                if !r.reasoning_content.is_empty() {
                    let _ = stream_tx.send(StreamEvent::Reasoning {
                        message_id: pending_msg_id.clone(),
                        content: r.reasoning_content.clone(),
                    });
                }
                r
            }
            Err(err) => {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: err.to_string(),
                });
                return;
            }
        }
    };

    session.append_message_with_id(
        pending_msg_id,
        MessageRole::Assistant,
        resp.text,
        String::new(),
    );
    let _ = stream_tx.send(StreamEvent::Done {
        usage: Some(resp.usage.clone()),
    });
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

    // 从 LlmConfig 构建兼容的 ModelsConfig（供 PromptAssembler 等旧代码使用）
    let models_config = ModelsConfig::from_llm_config(&config.llm);
    let model_config = models_config.to_chat_provider_config();

    let agent_config = AgentConfig {
        mcp: config.mcp.clone(),
        skills: config.skills.clone(),
        trust_mode: config.trust_mode,
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

    let mut engine = RuntimeEngine::with_shared_trust_mode(
        SingleProviderClient::new(model_config).with_on_retry(on_retry.clone()),
        config.context_limit,
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
        engine =
            engine.with_lite_client(SingleProviderClient::new(lite_config).with_on_retry(on_retry));
    }

    engine
}

fn inject_memory_recall_tool(tools: &mut Vec<FunctionToolSpec>) {
    if tools.iter().any(|tool| tool.name == "recall_memory") {
        return;
    }

    tools.push(FunctionToolSpec {
        name: "recall_memory".to_string(),
        description: "按需回忆历史上下文、跨会话结果、之前的工具输出或生成产物。用户提到刚刚、刚才、上次、之前、那个、继续、这张图、生成的图片等历史指代时，应先调用此工具。".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "要回忆的内容，结合用户当前请求改写成可检索查询"
                },
                "reason": {
                    "type": "string",
                    "description": "为什么需要回忆，简述当前任务依赖的历史语境"
                },
                "expected": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "期望找回的内容类型，如 media、file、tool_result、decision、code_context"
                },
                "limit": {
                    "type": "integer",
                    "description": "最多返回多少条记忆，默认 5，最大 10"
                }
            },
            "required": ["query"]
        }),
    });
}

fn execute_memory_recall_tool(
    call: &crate::model::ModelFunctionCall,
    memory_handle: Option<&tiangong_memory::MemoryHandle>,
    session: &Session,
) -> MemoryRecallToolOutput {
    let started = std::time::Instant::now();
    let Some(handle) = memory_handle else {
        return (
            memory_recall_tool_result(
                false,
                "记忆系统未启用",
                String::new(),
                "memory disabled".to_string(),
                1,
                Vec::new(),
                started,
            ),
            tiangong_types::TokenUsage::default(),
            false,
        );
    };

    let query = call
        .arguments
        .get("query")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| latest_user_message(session));
    if query.is_empty() {
        return (
            memory_recall_tool_result(
                false,
                "缺少回忆查询",
                String::new(),
                "recall_memory.query is empty".to_string(),
                1,
                Vec::new(),
                started,
            ),
            tiangong_types::TokenUsage::default(),
            false,
        );
    }

    let reason = call
        .arguments
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim();
    let expected = call
        .arguments
        .get("expected")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let limit = call
        .arguments
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(5)
        .clamp(1, 10) as usize;

    let response = handle.recall_context_blocking(tiangong_memory::MemoryRecallRequest {
        query: query.to_string(),
        reason: (!reason.is_empty()).then(|| reason.to_string()),
        expected,
        context: build_memory_recall_context(session),
        limit,
    });

    let memory_usage = tiangong_types::TokenUsage::from(response.usage.clone());

    if response.hits.is_empty() {
        return (
            memory_recall_tool_result(
                true,
                "未找到相关记忆",
                if response.content.trim().is_empty() {
                    format!("未找到与「{query}」相关的历史记忆。")
                } else {
                    response.content
                },
                String::new(),
                0,
                vec![query.to_string()],
                started,
            ),
            memory_usage,
            false,
        );
    }

    let stdout = if response.content.trim().is_empty() {
        "没有发现当前上下文之外的增量记忆。".to_string()
    } else {
        response.content
    };
    let allow_memory_context = !stdout
        .trim()
        .starts_with("没有发现当前上下文之外的增量记忆");

    (
        memory_recall_tool_result(
            true,
            format!("命中 {} 条相关记忆并完成整理", response.hits.len()),
            stdout,
            String::new(),
            0,
            vec![query.to_string()],
            started,
        ),
        memory_usage,
        allow_memory_context,
    )
}

fn memory_recall_tool_result(
    ok: bool,
    summary: impl Into<String>,
    stdout: String,
    stderr: String,
    exit_code: i32,
    args: Vec<String>,
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
            tool_name: "recall_memory".to_string(),
            args,
            duration_ms: started.elapsed().as_millis() as u64,
            ok,
            exit_code,
            summary,
        }),
    }
}

fn duplicate_memory_recall_tool_result() -> MemoryRecallToolOutput {
    let started = std::time::Instant::now();
    (
        memory_recall_tool_result(
            true,
            "本轮已完成回忆，跳过重复调用",
            "recall_memory 本轮已经执行过，回忆结果已经注入当前上下文。请直接基于已有回忆结果完成用户原始目标，不要再次调用 recall_memory。"
                .to_string(),
            String::new(),
            0,
            vec!["duplicate-recall".to_string()],
            started,
        ),
        tiangong_types::TokenUsage::default(),
        false,
    )
}

fn build_memory_recall_feedback(stdout: &str, allow_memory_context: bool) -> String {
    let header = if allow_memory_context && !stdout.trim().is_empty() {
        "recall_memory 已完成。以下是可直接使用的回忆结果，请基于这些内容继续完成用户原始目标；不要再次调用 recall_memory，除非用户提出新的历史查询。"
    } else if stdout.trim().is_empty() {
        "recall_memory 已完成，但没有可用的增量历史记忆。请基于当前上下文继续完成用户原始目标；不要再次调用 recall_memory。"
    } else {
        "recall_memory 已完成，结果如下。请基于当前上下文继续完成用户原始目标；不要再次调用 recall_memory。"
    };
    let body = truncate_chars_with_notice(
        stdout.trim(),
        MEMORY_LOOP_FEEDBACK_MAX_CHARS,
        "\n...(已截断，完整回忆结果已记录在工具执行消息中)",
    );
    if body.trim().is_empty() {
        header.to_string()
    } else {
        format!("{header}\n\n{body}")
    }
}

fn tool_result_full_output(result: &crate::tool::ToolResult) -> String {
    if result.ok {
        return if result.stdout.trim().is_empty() {
            result.summary.clone()
        } else {
            result.stdout.clone()
        };
    }

    let mut lines = Vec::new();
    if !result.summary.trim().is_empty() {
        lines.push(format!("summary: {}", result.summary));
    }
    if !result.stderr.trim().is_empty() {
        lines.push(format!("stderr:\n{}", result.stderr));
    }
    if !result.stdout.trim().is_empty() {
        lines.push(format!("stdout:\n{}", result.stdout));
    }
    if lines.is_empty() {
        "工具执行失败，但没有返回详细错误".to_string()
    } else {
        lines.join("\n")
    }
}

fn tool_result_stream_output(result: &crate::tool::ToolResult) -> String {
    let output = tool_result_full_output(result);
    truncate_chars_with_notice(
        &output,
        TOOL_RESULT_STREAM_MAX_CHARS,
        "\n...(已截断，完整工具输出已记录到会话数据)",
    )
}

fn truncate_chars_with_notice(text: &str, max_chars: usize, notice: &str) -> String {
    let mut chars = text.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}{notice}")
    } else {
        truncated
    }
}

fn latest_user_message(session: &Session) -> &str {
    session
        .messages
        .iter()
        .rev()
        .find(|message| message.role == MessageRole::User)
        .map(|message| message.content.as_str())
        .unwrap_or_default()
}

fn build_memory_recall_context(session: &Session) -> Vec<String> {
    let mut items = session
        .messages
        .iter()
        .rev()
        .filter_map(|message| {
            let role = match message.role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::System => return None,
                MessageRole::Tool => message.tool_name.as_deref().unwrap_or("tool"),
            };
            let content = compact_single_memory_text(&message.content, 900);
            (!content.is_empty()).then(|| format!("{role}: {content}"))
        })
        .take(30)
        .collect::<Vec<_>>();
    items.reverse();
    items
}

fn build_memory_turn_result(
    session: &Session,
    turn_start_idx: usize,
    user_input: &str,
) -> tiangong_memory::TurnResult {
    let messages = session.messages.get(turn_start_idx..).unwrap_or_default();
    let tool_calls = extract_turn_tool_calls(messages);
    let artifacts = extract_turn_artifacts(messages);
    let summary = build_turn_memory_summary(messages, &artifacts);
    tiangong_memory::TurnResult {
        session_id: session.id.clone(),
        turn_id: scru128::new().to_string(),
        had_tool_calls: !tool_calls.is_empty(),
        user_input: user_input.to_string(),
        summary,
        tool_calls,
        artifacts,
        workspace_id: None,
    }
}

fn extract_turn_tool_calls(messages: &[Message]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut names = Vec::new();
    for message in messages {
        if message.role != MessageRole::Tool {
            continue;
        }
        for name in parse_tool_calls_line(&message.content)
            .into_iter()
            .chain(parse_tool_trace_name(&message.content))
        {
            if name == "recall_memory" {
                continue;
            }
            if seen.insert(name.clone()) {
                names.push(name);
            }
        }
    }
    names
}

fn extract_turn_artifacts(messages: &[Message]) -> Vec<tiangong_memory::TurnArtifact> {
    let mut artifacts = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for message in messages {
        if message.role == MessageRole::Assistant {
            for media in &message.media {
                let key = format!("media:{}", media.url);
                if seen.insert(key) {
                    artifacts.push(tiangong_memory::TurnArtifact {
                        kind: tiangong_memory::TurnArtifactKind::Media,
                        tool_name: None,
                        title: media.title.clone(),
                        url: Some(media.url.clone()),
                        path: None,
                        summary: media.capability.clone(),
                    });
                }
            }
            continue;
        }

        if message.role != MessageRole::Tool {
            continue;
        }
        let tool_name = parse_tool_trace_name(&message.content);
        for artifact in
            parse_media_artifacts_from_tool_trace(&message.content, tool_name.as_deref())
        {
            let key = artifact
                .url
                .as_deref()
                .or(artifact.path.as_deref())
                .unwrap_or_default()
                .to_string();
            if !key.is_empty() && seen.insert(key) {
                artifacts.push(artifact);
            }
        }
        if let Some(path) = tool_name
            .as_deref()
            .filter(|name| *name == "write_file" || *name == "replace_in_file")
            .and_then(|_| parse_written_path(&message.content))
        {
            let key = format!("file:{path}");
            if seen.insert(key) {
                artifacts.push(tiangong_memory::TurnArtifact {
                    kind: tiangong_memory::TurnArtifactKind::File,
                    tool_name: tool_name.clone(),
                    title: Some("文件产物".to_string()),
                    url: None,
                    path: Some(path),
                    summary: parse_summary_line(&message.content),
                });
            }
        }
        if let Some(tool_name) = tool_name
            && should_record_tool_result(&tool_name)
        {
            let summary = parse_summary_line(&message.content)
                .unwrap_or_else(|| compact_single_memory_text(&message.content, 240));
            let key = format!("tool:{tool_name}:{summary}");
            if !summary.is_empty() && seen.insert(key) {
                artifacts.push(tiangong_memory::TurnArtifact {
                    kind: tiangong_memory::TurnArtifactKind::ToolResult,
                    tool_name: Some(tool_name),
                    title: Some("工具结果".to_string()),
                    url: None,
                    path: None,
                    summary: Some(summary),
                });
            }
        }
    }
    artifacts.into_iter().take(12).collect()
}

fn build_turn_memory_summary(
    messages: &[Message],
    artifacts: &[tiangong_memory::TurnArtifact],
) -> String {
    let assistant_summary = messages
        .iter()
        .rev()
        .find(|message| {
            message.role == MessageRole::Assistant && !message.content.trim().is_empty()
        })
        .map(|message| compact_single_memory_text(&message.content, 600))
        .unwrap_or_default();
    if !assistant_summary.is_empty() {
        return assistant_summary;
    }
    artifacts
        .iter()
        .filter_map(|artifact| {
            artifact
                .url
                .as_deref()
                .or(artifact.path.as_deref())
                .or(artifact.summary.as_deref())
                .map(str::to_string)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_tool_calls_line(content: &str) -> Vec<String> {
    content
        .lines()
        .find_map(|line| line.trim().strip_prefix("tool_calls:"))
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_tool_trace_name(content: &str) -> Option<String> {
    let first_line = content.lines().next()?.trim();
    let rest = first_line.strip_prefix("工具执行 [")?;
    let end = rest.find(']')?;
    Some(rest[..end].to_string()).filter(|name| !name.is_empty())
}

fn parse_summary_line(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        line.trim()
            .strip_prefix("summary:")
            .map(str::trim)
            .map(String::from)
            .filter(|item| !item.is_empty())
    })
}

fn parse_media_artifacts_from_tool_trace(
    content: &str,
    tool_name: Option<&str>,
) -> Vec<tiangong_memory::TurnArtifact> {
    let mut artifacts = Vec::new();
    let summary = parse_summary_line(content);
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("![") && trimmed.ends_with(')') {
            let Some(close_alt) = trimmed.find("](") else {
                continue;
            };
            let title = trimmed[2..close_alt].trim();
            let url = trimmed[close_alt + 2..trimmed.len() - 1].trim();
            if url.is_empty() {
                continue;
            }
            artifacts.push(tiangong_memory::TurnArtifact {
                kind: tiangong_memory::TurnArtifactKind::Media,
                tool_name: tool_name.map(String::from),
                title: (!title.is_empty()).then(|| title.to_string()),
                url: Some(url.to_string()),
                path: None,
                summary: summary.clone(),
            });
            continue;
        }

        if let Some(url) = parse_video_url_line(trimmed) {
            artifacts.push(tiangong_memory::TurnArtifact {
                kind: tiangong_memory::TurnArtifactKind::Media,
                tool_name: tool_name.map(String::from),
                title: Some("生成的视频".to_string()),
                url: Some(url),
                path: None,
                summary: summary.clone(),
            });
        }
    }
    artifacts
}

fn parse_media_assets_from_tool_result(
    tool_name: &str,
    stdout: &str,
    summary: &str,
) -> Vec<tiangong_types::MediaAsset> {
    match tool_name {
        "generate_image" => parse_image_assets(stdout),
        "generate_video" => parse_video_assets(stdout, summary),
        _ => Vec::new(),
    }
}

fn parse_image_assets(output: &str) -> Vec<tiangong_types::MediaAsset> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if !line.starts_with("![") || !line.ends_with(')') {
                return None;
            }
            let close_alt = line.find("](")?;
            let title = line[2..close_alt].trim();
            let url = line[close_alt + 2..line.len() - 1].trim();
            if url.is_empty() {
                return None;
            }
            let mime_type = url
                .strip_prefix("data:")
                .and_then(|raw| raw.split(';').next())
                .filter(|mime| mime.starts_with("image/"))
                .map(str::to_string);
            Some(tiangong_types::MediaAsset {
                kind: tiangong_types::MediaKind::Image,
                url: url.to_string(),
                mime_type,
                title: (!title.is_empty()).then(|| title.to_string()),
                capability: Some("image_generation".to_string()),
            })
        })
        .collect()
}

fn parse_video_assets(output: &str, summary: &str) -> Vec<tiangong_types::MediaAsset> {
    output
        .lines()
        .filter_map(|line| parse_video_url_line(line.trim()))
        .map(|url| tiangong_types::MediaAsset {
            kind: tiangong_types::MediaKind::Video,
            url,
            mime_type: Some("video/mp4".to_string()),
            title: Some(summary.to_string()).filter(|item| !item.trim().is_empty()),
            capability: Some("video_generation".to_string()),
        })
        .collect()
}

fn parse_video_url_line(line: &str) -> Option<String> {
    let raw = line
        .strip_prefix("Video URL:")
        .or_else(|| line.strip_prefix("video_url:"))
        .map(str::trim)?;
    let url = raw.split_whitespace().next().unwrap_or(raw);
    (url.starts_with("http://") || url.starts_with("https://")).then(|| url.to_string())
}

fn parse_written_path(content: &str) -> Option<String> {
    let command = content.lines().find_map(|line| {
        line.trim()
            .strip_prefix("命令:")
            .map(str::trim)
            .filter(|item| !item.is_empty())
    })?;
    let rest = command.strip_prefix("path=")?;
    let end = rest.find(" content=").unwrap_or(rest.len());
    Some(rest[..end].trim().to_string()).filter(|path| !path.is_empty())
}

fn should_record_tool_result(tool_name: &str) -> bool {
    !matches!(
        tool_name,
        "read_file"
            | "list_dir"
            | "tree_dir"
            | "search_code"
            | "recall_memory"
            | "get_skill_detail"
    )
}

fn compact_single_memory_text(text: &str, max_chars: usize) -> String {
    let normalized = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let mut clipped = normalized.chars().take(max_chars).collect::<String>();
    clipped.push_str("...");
    clipped
}

/// 格式化工具调用参数摘要（用于 ToolStart 和 ApprovalNeeded 事件）
fn format_call_args_summary(call: &crate::model::ModelFunctionCall) -> String {
    use serde_json::Value;

    let args = &call.arguments;
    if !args.is_object() || args.as_object().is_none_or(|m| m.is_empty()) {
        return String::new();
    }

    // run_command / run_shell 特殊处理：直接展示命令
    if call.name == "run_command" || call.name == "run_shell" {
        if let Some(cmd) = args.get("command").and_then(Value::as_str) {
            return cmd.to_string();
        }
        // shell 脚本模式
        if let Some(script) = args.get("script").and_then(Value::as_str) {
            let shell = args.get("shell").and_then(Value::as_str).unwrap_or("auto");
            return format!("[{shell}] {script}");
        }
    }

    // write_file 特殊处理
    if call.name == "write_file"
        && let Some(path) = args.get("path").and_then(Value::as_str)
    {
        let len = args
            .get("content")
            .and_then(Value::as_str)
            .map(|c| c.len())
            .unwrap_or(0);
        return format!("{path} ({len} bytes)");
    }

    if call.name == "recall_memory"
        && let Some(query) = args.get("query").and_then(Value::as_str)
    {
        return query.to_string();
    }

    // read_file / list_directory
    if (call.name == "read_file" || call.name == "list_directory")
        && let Some(path) = args.get("path").and_then(Value::as_str)
    {
        return path.to_string();
    }

    // 通用：key=value 格式，截断长值（按字符截断，避免 UTF-8 边界 panic）
    let obj = args.as_object().unwrap();
    obj.iter()
        .map(|(k, v)| {
            let val = match v {
                Value::String(s) if s.chars().count() > 80 => {
                    let truncated: String = s.chars().take(77).collect();
                    format!("{truncated}...")
                }
                Value::String(s) => s.clone(),
                other => {
                    let s = other.to_string();
                    if s.chars().count() > 80 {
                        let truncated: String = s.chars().take(77).collect();
                        format!("{truncated}...")
                    } else {
                        s
                    }
                }
            };
            format!("{k}={val}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn infer_audit_target(call: &crate::model::ModelFunctionCall) -> (Option<String>, Option<String>) {
    use serde_json::Value;

    let Some(obj) = call.arguments.as_object() else {
        return infer_tool_name_scope(call.name.as_str(), None);
    };

    let path_keys = [
        "path",
        "file_path",
        "output_path",
        "cwd",
        "directory",
        "dir",
        "target_path",
        "workspace_path",
    ];
    for key in path_keys {
        if let Some(value) = obj.get(key).and_then(Value::as_str).map(str::trim)
            && !value.is_empty()
        {
            return (Some("path".to_string()), Some(value.to_string()));
        }
    }

    let network_keys = ["url", "endpoint", "host", "domain", "base_url"];
    for key in network_keys {
        if let Some(value) = obj.get(key).and_then(Value::as_str).map(str::trim)
            && !value.is_empty()
        {
            return (Some("network".to_string()), Some(value.to_string()));
        }
    }

    if let Some(value) = obj.get("task_id").and_then(Value::as_str).map(str::trim)
        && !value.is_empty()
    {
        return (Some("task".to_string()), Some(value.to_string()));
    }
    if let Some(values) = obj.get("task_ids").and_then(Value::as_array)
        && !values.is_empty()
    {
        let joined = values
            .iter()
            .filter_map(Value::as_str)
            .take(3)
            .collect::<Vec<_>>()
            .join(",");
        if !joined.is_empty() {
            return (Some("task".to_string()), Some(joined));
        }
    }

    if let Some(value) = obj.get("command").and_then(Value::as_str).map(str::trim)
        && !value.is_empty()
    {
        return (Some("command".to_string()), Some(value.to_string()));
    }
    if let Some(value) = obj.get("script").and_then(Value::as_str).map(str::trim)
        && !value.is_empty()
    {
        return (Some("command".to_string()), Some(value.to_string()));
    }

    infer_tool_name_scope(call.name.as_str(), None)
}

fn infer_tool_name_scope(
    tool_name: &str,
    summary: Option<String>,
) -> (Option<String>, Option<String>) {
    let scope = match tool_name {
        "read_file" | "write_file" | "replace_in_file" | "list_dir" | "tree_dir" => "path",
        "generate_image" | "speech_to_text" | "text_to_speech" => "external",
        "run_command" | "run_shell" => "command",
        _ => return (None, summary),
    };
    (Some(scope.to_string()), summary)
}

fn evaluate_tool_permission(
    engine: &RuntimeEngine,
    tool_name: &str,
    target_scope: Option<&str>,
    target_summary: Option<&str>,
) -> crate::permission::PermissionDecision {
    let base_decision = engine.check_tool_permission(tool_name);
    let scoped_decision = match (target_scope, target_summary) {
        (Some("path"), Some(path)) => Some(engine.permission_gate().check_path(path)),
        (Some("network"), Some(target)) => Some(engine.permission_gate().check_network(target)),
        _ => None,
    };

    match (base_decision, scoped_decision) {
        (crate::permission::PermissionDecision::Denied { reason }, _)
        | (_, Some(crate::permission::PermissionDecision::Denied { reason })) => {
            crate::permission::PermissionDecision::Denied { reason }
        }
        (crate::permission::PermissionDecision::NeedsApproval { request_id }, _) => {
            crate::permission::PermissionDecision::NeedsApproval { request_id }
        }
        (_, Some(crate::permission::PermissionDecision::NeedsApproval { request_id })) => {
            crate::permission::PermissionDecision::NeedsApproval { request_id }
        }
        _ => crate::permission::PermissionDecision::Approved,
    }
}

fn normalize_permission_target(
    session: &Session,
    target_scope: Option<&str>,
    target_summary: Option<&str>,
) -> Option<String> {
    let target = target_summary?.trim();
    if target.is_empty() {
        return None;
    }

    match target_scope {
        Some("path") => Some(normalize_path_target(session, target)),
        Some("network") => Some(normalize_network_target(target)),
        _ => Some(target.to_string()),
    }
}

fn normalize_path_target(session: &Session, target: &str) -> String {
    let path = std::path::PathBuf::from(target);
    if path.is_absolute() {
        return path.to_string_lossy().to_string();
    }

    let base = if !session.cwd.is_empty() {
        std::path::PathBuf::from(&session.cwd)
    } else {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    };

    base.join(path).to_string_lossy().to_string()
}

fn normalize_network_target(target: &str) -> String {
    let trimmed = target.trim();
    let without_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme)
        .split('@')
        .next_back()
        .unwrap_or(without_scheme)
        .split(':')
        .next()
        .unwrap_or(without_scheme)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    static MEMORY_REGISTRY_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn memory_registry_test_lock() -> std::sync::MutexGuard<'static, ()> {
        MEMORY_REGISTRY_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("memory registry 测试锁已污染")
    }

    #[test]
    fn memory_registry_key_separates_workspace_and_global_handles() {
        assert_eq!(memory_registry_key(None), GLOBAL_MEMORY_WORKSPACE_KEY);
        assert_eq!(memory_registry_key(Some("")), GLOBAL_MEMORY_WORKSPACE_KEY);
        assert_eq!(memory_registry_key(Some("ws-a")), "ws-a");
        assert_ne!(
            memory_registry_key(Some("ws-a")),
            memory_registry_key(Some("ws-b"))
        );
    }

    #[test]
    fn resolve_memory_workspace_id_prefers_session_cwd() {
        let from_session = resolve_memory_workspace_id("/tmp/tiangong-memory-workspace-a")
            .expect("session cwd 应生成 workspace_id");
        let from_other_session = resolve_memory_workspace_id("/tmp/tiangong-memory-workspace-b")
            .expect("session cwd 应生成 workspace_id");
        assert_ne!(from_session, from_other_session);
    }

    #[test]
    fn memory_config_summary_tracks_memory_relevant_fields() {
        let config = memory_test_config(768, "embedded");
        let summary = memory_config_summary(&config);

        let model = summary.model.as_ref().expect("应包含 memory model 摘要");
        assert_eq!(model.base_url, "http://chat.example");
        assert_eq!(model.model, "chat-model");
        let embedding = summary.embedding.as_ref().expect("应包含 embedding 摘要");
        assert_eq!(embedding.base_url, "http://embed.example");
        assert_eq!(embedding.model, "embed-model");
        assert_eq!(embedding.dimension, 768);
        assert_eq!(summary.vector_mode, "Embedded");

        let changed_dimension = memory_config_summary(&memory_test_config(1024, "embedded"));
        assert!(memory_config_changed(&summary, &changed_dimension));

        let changed_vector_mode = memory_config_summary(&memory_test_config(768, "disabled"));
        assert!(memory_config_changed(&summary, &changed_vector_mode));

        let same_memory_config = memory_config_summary(&memory_test_config(768, "embedded"));
        assert!(!memory_config_changed(&summary, &same_memory_config));
        assert!(memory_config_can_update_in_place(
            &summary,
            &changed_dimension
        ));
    }

    #[test]
    fn memory_registry_reuses_workspace_handle_and_separates_workspaces() {
        let _lock = memory_registry_test_lock();
        let _env = MemoryRegistryEnvGuard::enter();
        shutdown_memory_registry_blocking();

        let config = CoreConfig::default();
        let workspace_a = format!("ws-registry-a-{}", scru128::new());
        let workspace_b = format!("ws-registry-b-{}", scru128::new());

        let handle_a = get_or_init_memory(&config, 1, Some(workspace_a.clone()))
            .expect("workspace A memory handle 应启动成功");
        let handle_a_again = get_or_init_memory(&config, 1, Some(workspace_a))
            .expect("workspace A memory handle 应可复用");
        let handle_b = get_or_init_memory(&config, 1, Some(workspace_b))
            .expect("workspace B memory handle 应启动成功");

        assert!(
            handle_a.is_same_handle(&handle_a_again),
            "同一 workspace 应复用同一个 MemoryHandle"
        );
        assert!(
            !handle_a.is_same_handle(&handle_b),
            "不同 workspace 应使用不同 MemoryHandle"
        );

        shutdown_memory_registry_blocking();
    }

    #[test]
    fn memory_registry_hot_updates_memory_config_in_place() {
        let _lock = memory_registry_test_lock();
        let _env = MemoryRegistryEnvGuard::enter();
        shutdown_memory_registry_blocking();

        let workspace = format!("ws-hot-update-{}", scru128::new());
        let initial = CoreConfig::default();
        let updated = memory_test_config(1024, "embedded");
        let expected_summary = memory_config_summary(&updated);

        let initial_handle = get_or_init_memory(&initial, 1, Some(workspace.clone()))
            .expect("初始 MemoryHandle 应启动成功");
        let updated_handle = get_or_init_memory(&updated, 2, Some(workspace.clone()))
            .expect("MemoryHandle 应支持热更新后继续可用");

        assert!(
            initial_handle.is_same_handle(&updated_handle),
            "Memory 配置热更新应原地复用同一 handle"
        );

        let registry = MEMORY_HANDLES.get().expect("registry 应已初始化");
        let guard = registry.lock().expect("registry 锁应可用");
        let entry = guard
            .get(&memory_registry_key(Some(&workspace)))
            .expect("workspace entry 应存在");
        assert_eq!(entry.config_generation, 2);
        assert!(!entry.restart_required);
        assert_eq!(entry.config_summary, expected_summary);
        drop(guard);

        shutdown_memory_registry_blocking();
    }

    #[test]
    fn memory_registry_reacts_to_core_config_provider_hot_reload() {
        let _lock = memory_registry_test_lock();
        let _env = MemoryRegistryEnvGuard::enter();
        shutdown_memory_registry_blocking();

        let workspace = format!("ws-provider-reload-{}", scru128::new());
        let provider = CoreConfigProvider::new(CoreConfig::default());
        let initial_snapshot = provider.snapshot();
        let initial_handle = get_or_init_memory(
            &initial_snapshot,
            provider.generation(),
            Some(workspace.clone()),
        )
        .expect("初始 MemoryHandle 应启动成功");

        let hot_config = memory_test_config(2048, "embedded");
        provider.update(|config| {
            config.llm = hot_config.llm.clone();
        });
        let updated_snapshot = provider.snapshot();
        let expected_summary = memory_config_summary(&updated_snapshot);
        let updated_handle = get_or_init_memory(
            &updated_snapshot,
            provider.generation(),
            Some(workspace.clone()),
        )
        .expect("配置热重载后 MemoryHandle 应继续可用");

        assert!(
            initial_handle.is_same_handle(&updated_handle),
            "CoreConfigProvider 热重载后应原地复用同一 MemoryHandle"
        );

        let registry = MEMORY_HANDLES.get().expect("registry 应已初始化");
        let guard = registry.lock().expect("registry 锁应可用");
        let entry = guard
            .get(&memory_registry_key(Some(&workspace)))
            .expect("workspace entry 应存在");
        assert_eq!(
            entry.config_generation,
            provider.generation(),
            "registry 应记录最新配置 generation"
        );
        assert_eq!(
            entry.config_summary, expected_summary,
            "registry 应记录热重载后的 Memory 配置摘要"
        );
        assert!(
            !entry.restart_required,
            "model/embedding/dimension/vector_mode 变化应通过原地热更新完成"
        );
        drop(guard);

        shutdown_memory_registry_blocking();
    }

    fn memory_test_config(dimension: usize, vector_mode: &str) -> CoreConfig {
        let mut config = CoreConfig::default();
        config.llm.chat = crate::core_config::ModelEndpoint {
            base_url: "http://chat.example".to_string(),
            api_key: "secret".to_string(),
            model: "chat-model".to_string(),
            ..Default::default()
        };
        config.llm.embedding = Some(crate::core_config::ModelEndpoint {
            base_url: "http://embed.example".to_string(),
            api_key: "secret".to_string(),
            model: "embed-model".to_string(),
            options: serde_json::json!({
                "dimension": dimension,
                "vector_mode": vector_mode,
            }),
            ..Default::default()
        });
        config
    }

    struct MemoryRegistryEnvGuard {
        prev_home: Option<std::ffi::OsString>,
        prev_userprofile: Option<std::ffi::OsString>,
        home: std::path::PathBuf,
    }

    impl MemoryRegistryEnvGuard {
        fn enter() -> Self {
            let prev_home = std::env::var_os("HOME");
            let prev_userprofile = std::env::var_os("USERPROFILE");
            let home =
                std::env::temp_dir().join(format!("tiangong-core-memory-{}", scru128::new()));
            std::fs::create_dir_all(&home).expect("创建 memory registry 测试目录失败");
            unsafe {
                std::env::set_var("HOME", &home);
                std::env::set_var("USERPROFILE", &home);
            }
            Self {
                prev_home,
                prev_userprofile,
                home,
            }
        }
    }

    impl Drop for MemoryRegistryEnvGuard {
        fn drop(&mut self) {
            shutdown_memory_registry_blocking();
            unsafe {
                match &self.prev_home {
                    Some(value) => std::env::set_var("HOME", value),
                    None => std::env::remove_var("HOME"),
                }
                match &self.prev_userprofile {
                    Some(value) => std::env::set_var("USERPROFILE", value),
                    None => std::env::remove_var("USERPROFILE"),
                }
            }
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }
}
