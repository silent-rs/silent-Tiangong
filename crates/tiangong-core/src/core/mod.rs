//! TiangongCore：天工智能体核心
//!
//! 单一线程完成所有工作：接收消息 → LLM 调用 → 工具执行 → session 更新 → 推送 StreamEvent
//! CLI / GUI / Server / Connector 统一通过 TiangongCore 运行。

use std::collections::HashMap;
use std::sync::mpsc::{self, Sender, Sender as StdSender};
use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};
use tokio::sync::mpsc as tokio_mpsc;

use crate::agents::execution_mcp_agent::{McpFunctionTarget, execution_function_tools};
use crate::coordinator::TaskCoordinator;
use crate::coordinator::types::CoordinatorTask;
use crate::core_config::CoreConfigProvider;
use crate::model::{ModelClient, ModelRequest, SingleProviderClient, TokenUsage, ToolSpec};
use crate::react::message::{append_or_reuse_user_message, append_runtime_tool_message};
use crate::runtime::{RuntimeEngine, inject_enhanced_tools};
use crate::session::{Message, MessageRole, Session};
use tiangong_types::{SessionStreamEvent, StreamEvent};

use std::sync::mpsc::Receiver;

const MAX_ROUNDS: usize = 20;

// ── Memory re-exports ──
pub use crate::memory::gui_api::*;
pub(crate) use crate::memory::recall::{
    duplicate_memory_recall_tool_result, execute_memory_recall_tool, inject_memory_recall_tool,
};
pub(crate) use crate::memory::registry::{
    WorkerMemoryContext, get_or_init_memory, get_or_init_memory_async, resolve_memory_workspace_id,
};
pub use crate::memory::registry::{
    get_or_init_memory_handle, get_or_init_memory_handle_async, load_memory_config,
    memory_workspace_id_from_cwd, save_memory_config, shutdown_memory_registry_blocking,
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
        let memory_workspace_id = resolve_memory_workspace_id(&session.cwd);
        let memory_handle = get_or_init_memory(
            &config_snapshot,
            config.generation(),
            memory_workspace_id.clone(),
            process_type.clone(),
        );
        let initial_trust_mode = session.trust_mode;
        let shared_trust_mode = Arc::new(RwLock::new(initial_trust_mode));
        let session_id = session.id.clone();
        let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel::<Command>();

        let worker_trust_mode = shared_trust_mode.clone();
        let worker_process_type = process_type.clone();
        let worker = thread::spawn(move || {
            let memory = WorkerMemoryContext {
                handle: memory_handle,
                workspace_id: memory_workspace_id,
                process_type: worker_process_type,
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
    let mut engine: Option<RuntimeEngine> = None;
    let mut tools: Vec<ToolSpec> = Vec::new();
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
            memory.handle = get_or_init_memory_async(
                &cfg,
                cfg_gen,
                memory.workspace_id.clone(),
                memory.process_type.clone(),
            )
            .await;
            engine = Some(build_engine_from_config(
                &cfg,
                &stream_tx,
                shared_trust_mode.clone(),
            ));
            let e = engine.as_ref().unwrap();
            let (all_tools, new_mcp_targets) = execution_function_tools(&e.agent_config().mcp);
            let mut new_tools: Vec<ToolSpec> = all_tools
                .into_iter()
                .filter(|t| t.name != "mark_step_completed")
                .collect();
            inject_enhanced_tools(&mut new_tools, e);
            if memory.handle.is_some() {
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
                memory.workspace_id = resolve_memory_workspace_id(&session.cwd);
                let cfg = config.snapshot();
                memory.handle = get_or_init_memory_async(
                    &cfg,
                    config.generation(),
                    memory.workspace_id.clone(),
                    memory.process_type.clone(),
                )
                .await;
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
                )
                .await;

                // turn 完成后触发 Micro 反刍（fire-and-forget）
                if let Some(handle) = memory.handle.as_ref() {
                    // 显式携带 workspace_id，避免 Actor 固化到启动时工作区造成跨工作区串写
                    let mut turn_result =
                        build_memory_turn_result(&session, turn_start_idx, &content);
                    turn_result.workspace_id = memory.workspace_id.clone();
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
    if let Some(handle) = memory.handle.as_ref()
        && let Some(wid) = &memory.workspace_id
    {
        handle.run_meso_rumination(session_id.clone(), wid.clone());
        tracing::info!(session_id = %session_id, workspace_id = %wid, "Meso 反刍已触发（会话结束）");
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

/// 供 Worker 调用的独立执行函数
///
/// 通过内部 tokio runtime 桥接到 ReactEngine（async），Worker 无需关心 async 细节。
/// std::sync::mpsc::Receiver<Command> 通过桥接线程转发到 tokio channel。
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_turn_standalone(
    session: &mut Session,
    user_input: &str,
    engine: &RuntimeEngine,
    tools: &[ToolSpec],
    mcp_targets: &HashMap<String, McpFunctionTarget>,
    stream_tx: &StdSender<StreamEvent>,
    cmd_rx: Receiver<Command>,
    max_rounds: usize,
) -> TokenUsage {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("创建 execute_turn_standalone tokio runtime 失败");

    let (tokio_tx, mut tokio_rx) = tokio_mpsc::unbounded_channel::<Command>();
    let bridge_handle = std::thread::spawn(move || {
        while let Ok(cmd) = cmd_rx.recv() {
            if tokio_tx.send(cmd).is_err() {
                break;
            }
        }
    });

    let react = crate::react::engine::ReactEngine::new(
        engine.clone(),
        tools.to_vec(),
        mcp_targets.clone(),
        max_rounds,
    );
    let usage =
        rt.block_on(react.execute_turn(session, user_input, stream_tx, &mut tokio_rx, None));

    drop(tokio_rx);
    let _ = bridge_handle.join();

    usage
}

/// 执行一个完整的对话轮次（可能多轮工具调用），async 版
///
/// 首先判断是否需要多代理并行执行，如需要则拆分并行；
/// 否则走标准的 ReAct 循环。
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
) {
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
                let react = crate::react::engine::ReactEngine::new(
                    engine.clone(),
                    tools.to_vec(),
                    mcp_targets.clone(),
                    MAX_ROUNDS,
                );
                react
                    .execute_turn(session, user_input, stream_tx, cmd_rx, memory_handle)
                    .await;
            }
        }
        return;
    }

    let react = crate::react::engine::ReactEngine::new(
        engine.clone(),
        tools.to_vec(),
        mcp_targets.clone(),
        MAX_ROUNDS,
    );
    react
        .execute_turn(session, user_input, stream_tx, cmd_rx, memory_handle)
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

    // 从 LlmConfig 构建兼容的 ModelsConfig（供 PromptAssembler 等旧代码使用）
    let models_config = ModelsConfig::from_llm_config(&config.llm);
    let model_config = models_config.to_chat_provider_config();

    let agent_config = AgentConfig {
        mcp: config.mcp.clone(),
        skills: config.skills.clone(),
        trust_mode: config.trust_mode,
        default_trust_mode: config.default_trust_mode,
        custom_system_prompt: config.custom_system_prompt.clone(),
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

    engine
}

pub(crate) fn execute_attachment_analysis_tool(
    call: &crate::model::ToolCall,
    engine: &RuntimeEngine,
    session: &Session,
) -> crate::tool::ToolResult {
    let started = std::time::Instant::now();
    if !engine.has_multimodal_client() {
        return attachment_tool_result(
            false,
            "未配置多模态模型",
            String::new(),
            "multimodal model is not configured".to_string(),
            1,
            started,
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
        return attachment_tool_result(
            false,
            "未找到可解析的附件",
            String::new(),
            "no user message with attachments found".to_string(),
            1,
            started,
        );
    };

    let media = if let Some(index) = attachment_index {
        let Some(asset) = source_message.media.get(index) else {
            return attachment_tool_result(
                false,
                "附件序号不存在",
                String::new(),
                format!("attachment_index {index} out of range"),
                1,
                started,
            );
        };
        vec![asset.clone()]
    } else {
        source_message.media.clone()
    };

    if media.is_empty() {
        return attachment_tool_result(
            false,
            "未找到可解析的附件",
            String::new(),
            "selected message has no attachments".to_string(),
            1,
            started,
        );
    }

    let mut attachment_message = Message::new(
        MessageRole::User,
        format!(
            "用户原始消息：{}\n\n解析要求：{}",
            source_message.content.trim(),
            instruction
        ),
    );
    attachment_message.media = media;

    let req = ModelRequest {
        session_title: format!("{} · attachment-analysis", session.title),
        user_input: String::new(),
        context: vec![attachment_message],
        assembled_system_prompt: Some(
            "你是附件解析助手。只根据随消息提供的附件内容和解析要求回答，输出可供主模型直接使用的简洁中文结果。"
                .to_string(),
        ),
        thinking: None,
        include_media: true,
    };

    match engine.multimodal_client().complete(&req) {
        Ok(response) => attachment_tool_result(
            true,
            "附件解析完成",
            response.text,
            String::new(),
            0,
            started,
        ),
        Err(err) => attachment_tool_result(
            false,
            "附件解析失败",
            String::new(),
            err.to_string(),
            1,
            started,
        ),
    }
}

fn find_attachment_source_message<'a>(
    session: &'a Session,
    message_id: Option<&str>,
) -> Option<&'a Message> {
    if let Some(message_id) = message_id {
        return session
            .messages
            .iter()
            .find(|message| message.id == message_id && !message.media.is_empty());
    }
    session
        .messages
        .iter()
        .rev()
        .find(|message| message.role == MessageRole::User && !message.media.is_empty())
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

pub(crate) fn parse_media_assets_from_tool_result(
    tool_name: &str,
    stdout: &str,
    summary: &str,
) -> Vec<tiangong_types::MediaAsset> {
    if output_may_contain_generated_images(tool_name, stdout) {
        return parse_image_assets(stdout);
    }
    if tool_name == "generate_video" {
        parse_video_assets(stdout, summary)
    } else {
        Vec::new()
    }
}

pub(crate) fn localize_tool_result_images(tool_name: &str, result: &mut crate::tool::ToolResult) {
    if !result.ok || !output_may_contain_generated_images(tool_name, &result.stdout) {
        return;
    }
    result.stdout = archive_image_markdown_output(&result.stdout);
}

fn output_may_contain_generated_images(tool_name: &str, output: &str) -> bool {
    tool_name == "generate_image"
        || looks_like_pure_image_markdown(output)
        || (tool_name.to_ascii_lowercase().contains("image") && output.contains("]("))
}

fn looks_like_pure_image_markdown(output: &str) -> bool {
    let trimmed = output.trim();
    !trimmed.is_empty()
        && trimmed.lines().all(|line| {
            let line = line.trim();
            line.is_empty()
                || (line.starts_with("![") && line.contains("](") && line.ends_with(')'))
        })
}

fn archive_image_markdown_output(output: &str) -> String {
    output
        .lines()
        .map(|line| {
            let Some((alt, url)) = parse_markdown_image_line(line.trim()) else {
                return line.to_string();
            };
            match crate::media_archive::archive_image_reference(url, None) {
                Ok(archived) => format!("![{alt}]({})", archived.path()),
                Err(err) => {
                    tracing::warn!(url = %url, error = %err, "图片归档到本地失败，保留原始 URL");
                    line.to_string()
                }
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_markdown_image_line(line: &str) -> Option<(&str, &str)> {
    if !line.starts_with("![") || !line.ends_with(')') {
        return None;
    }
    let close_alt = line.find("](")?;
    let alt = line[2..close_alt].trim();
    let url = line[close_alt + 2..line.len() - 1].trim();
    (!url.is_empty()).then_some((alt, url))
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
