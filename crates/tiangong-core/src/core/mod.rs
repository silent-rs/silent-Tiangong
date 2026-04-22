//! TiangongCore：天工智能体核心
//!
//! 单一线程完成所有工作：接收消息 → LLM 调用 → 工具执行 → session 更新 → 推送 StreamEvent
//! CLI / GUI / Server / Connector 统一通过 TiangongCore 运行。

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::thread::{self, JoinHandle};

use crate::agents::execution_mcp_agent::{McpFunctionTarget, execution_function_tools};
use crate::app_state::formatting::{format_llm_output_message, format_tool_trace_message};
use crate::coordinator::TaskCoordinator;
use crate::coordinator::types::CoordinatorTask;
use crate::core_config::{CoreConfig, CoreConfigProvider};
use crate::model::{FunctionToolSpec, ModelClient, ModelRequest, SingleProviderClient, TokenUsage};
use crate::observe::{audit_permission_with_context, audit_tool_execution};
use crate::prompt::PromptAssembler;
use crate::runtime::{LlmOutputRecord, RuntimeEngine, inject_enhanced_tools, use_stream_mode};
use crate::session::{Message, MessageRole, Session, now_text};
use crate::stream_throttle::ThrottledStreamSink;
use tiangong_types::{SessionStreamEvent, StreamEvent};

const MAX_ROUNDS: usize = 20;

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
    Message(String),
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
    /// 用户命令发送端
    cmd_tx: Option<Sender<Command>>,
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
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();

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

    fn send_cmd(&self, cmd: Command) {
        if let Some(ref tx) = self.cmd_tx {
            let _ = tx.send(cmd);
        }
    }

    pub fn send_message(&self, content: String) {
        self.send_cmd(Command::Message(content));
    }

    pub fn cancel(&self) {
        self.send_cmd(Command::Cancel);
    }

    pub fn respond_approval(&self, request_id: String, approved: bool) {
        self.send_cmd(Command::Approval {
            request_id,
            approved,
        });
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// 设置信任模式（实时生效，当前对话下一次工具调用立即感知）
    pub fn set_trust_mode(&self, mode: crate::permission::TrustMode) {
        if let Ok(mut guard) = self.shared_trust_mode.write() {
            *guard = mode;
        }
    }

    /// 关闭并获取最终 session
    pub fn into_session(mut self) -> Session {
        self.send_cmd(Command::Shutdown);
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
    mut session: Session,
    external_tx: Sender<SessionStreamEvent>,
    cmd_rx: Receiver<Command>,
    shared_trust_mode: Arc<RwLock<crate::permission::TrustMode>>,
    mut memory_handle: Option<tiangong_memory::MemoryHandle>,
    memory_workspace_id: Option<String>,
) -> Session {
    let session_id = session.id.clone();
    let mut last_cfg_gen = 0u64;
    let mut engine: Option<RuntimeEngine> = None;
    let mut tools: Vec<FunctionToolSpec> = Vec::new();
    let mut mcp_targets: HashMap<String, McpFunctionTarget> = HashMap::new();
    // 跨 turn 持久的回忆上下文，新 recall_memory 执行时替换
    let mut memory_context: Option<String> = None;
    // turn 计数器：每 10 个 turn 触发一次 Meta 反刍（归档低活跃节点）
    let mut turn_count: u32 = 0;

    // 内部 StreamEvent 通道 —— 转发线程负责包装 session_id
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

    // 设置工作目录
    if !session.cwd.is_empty() {
        let p = std::path::PathBuf::from(&session.cwd);
        if p.is_dir() {
            crate::tool::set_session_cwd(Some(p));
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

    while let Ok(cmd) = cmd_rx.recv() {
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
            Command::Message(content) => {
                let turn_start_idx = session.messages.len();
                // 记录用户消息
                session.append_message(MessageRole::User, content.clone());
                // 通知消费端：用户消息已记录（携带 session 中的 message_id）
                let user_msg_id = session
                    .messages
                    .last()
                    .map(|m| m.id.clone())
                    .unwrap_or_default();
                let _ = stream_tx.send(StreamEvent::UserMessage {
                    message_id: user_msg_id,
                    content: content.clone(),
                });

                // 执行对话轮次
                execute_turn(
                    &mut session,
                    &content,
                    engine.as_ref().unwrap(),
                    &tools,
                    &mcp_targets,
                    &stream_tx,
                    &cmd_rx,
                    memory_handle.as_ref(),
                    &mut memory_context,
                );

                // turn 完成后触发 Micro 反刍（fire-and-forget）
                if let Some(handle) = memory_handle.as_ref() {
                    // 显式携带 workspace_id，避免 Actor 固化到启动时工作区造成跨工作区串写
                    let mut turn_result =
                        build_memory_turn_result(&session, turn_start_idx, &content);
                    turn_result.workspace_id = memory_workspace_id.clone();
                    handle.run_micro_rumination_blocking(turn_result);

                    // 每 10 个 turn 触发一次 Meta 反刍（归档低活跃节点）
                    turn_count += 1;
                    if turn_count % 10 == 0 {
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
                    session.append_message(
                        MessageRole::System,
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
    let _ = forward_handle.join();

    // 会话结束 → 触发 Meso 反刍（提炼 Entity/Decision，更新 Workspace Injection）
    // fire-and-forget：handle 仍可使用（Memory Actor 在 registry 中持续运行）
    if let Some(handle) = memory_handle.as_ref() {
        if let Some(wid) = &memory_workspace_id {
            handle.run_meso_rumination(session_id.clone(), wid.clone());
            tracing::info!(session_id = %session_id, workspace_id = %wid, "Meso 反刍已触发（会话结束）");
        }
    }

    session
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
    stream_tx: &Sender<StreamEvent>,
    cmd_rx: &Receiver<Command>,
    max_rounds: usize,
) -> TokenUsage {
    let mut memory_context: Option<String> = None;
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
        &mut memory_context,
    )
}

/// 执行一个完整的对话轮次（可能多轮工具调用）
///
/// 首先判断是否需要多代理并行执行，如需要则拆分并行；
/// 否则走标准的 ReAct 循环。
/// 每轮之间检查 cmd_rx：新消息注入上下文，cancel 立即生效。
#[allow(clippy::too_many_arguments)]
fn execute_turn(
    session: &mut Session,
    user_input: &str,
    engine: &RuntimeEngine,
    tools: &[FunctionToolSpec],
    mcp_targets: &HashMap<String, McpFunctionTarget>,
    stream_tx: &Sender<StreamEvent>,
    cmd_rx: &Receiver<Command>,
    memory_handle: Option<&tiangong_memory::MemoryHandle>,
    memory_context: &mut Option<String>,
) {
    // 判断是否需要多代理并行执行
    let coordinator = TaskCoordinator::new(engine.clone());
    let task = CoordinatorTask {
        id: scru128::new().to_string(),
        objective: user_input.to_string(),
        user_input: user_input.to_string(),
        context: Vec::new(),
    };

    if coordinator.should_split(&task) {
        tracing::info!("任务需要拆分，启动多代理并行执行");
        match coordinator.coordinate(task, session, stream_tx) {
            Ok(result) => {
                // 记录合并结果到 session
                session.append_message(MessageRole::Assistant, result.final_response);
                let _ = stream_tx.send(StreamEvent::Done {
                    usage: Some(tiangong_types::TokenUsage {
                        prompt_tokens: result.total_usage.prompt_tokens,
                        completion_tokens: result.total_usage.completion_tokens,
                        total_tokens: result.total_usage.total_tokens,
                    }),
                });
            }
            Err(err) => {
                tracing::warn!("多代理并行执行失败，回退单代理: {err}");
                // 回退到单代理执行
                execute_turn_inner(
                    session,
                    user_input,
                    engine,
                    tools,
                    mcp_targets,
                    stream_tx,
                    cmd_rx,
                    MAX_ROUNDS,
                    memory_handle,
                    memory_context,
                );
            }
        }
        return;
    }

    execute_turn_inner(
        session,
        user_input,
        engine,
        tools,
        mcp_targets,
        stream_tx,
        cmd_rx,
        MAX_ROUNDS,
        memory_handle,
        memory_context,
    );
}

/// 内部执行：标准 ReAct 循环
#[allow(clippy::too_many_arguments)]
fn execute_turn_inner(
    session: &mut Session,
    _user_input: &str,
    engine: &RuntimeEngine,
    tools: &[FunctionToolSpec],
    mcp_targets: &HashMap<String, McpFunctionTarget>,
    stream_tx: &Sender<StreamEvent>,
    cmd_rx: &Receiver<Command>,
    max_rounds: usize,
    memory_handle: Option<&tiangong_memory::MemoryHandle>,
    memory_context: &mut Option<String>,
) -> TokenUsage {
    let mut loop_context: Vec<Message> = Vec::new();
    let mut round = 0;
    let mut accumulated_usage = TokenUsage::default();
    let mut pending_media_assets: Vec<tiangong_types::MediaAsset> = Vec::new();

    loop {
        if round >= max_rounds {
            // 超限：强制最终回复
            force_final_response(session, &loop_context, engine, stream_tx);
            break;
        }

        // 构建 prompt
        // 用户输入统一通过 session.messages 传递，不使用独立的 user_input 通道。
        // 工具调用后的续写提示只在本轮 prompt 中临时注入，避免重复累积到 loop_context。
        let mut prompt_loop_context = loop_context.clone();
        if round > 0 {
            prompt_loop_context.push(Message {
                id: scru128::new().to_string(),
                role: MessageRole::System,
                content: "内部调度提示：根据上面的工具执行结果继续完成用户原始目标。若目标尚未实际完成，继续调用必要工具；只有确认目标已完成时，才给出最终回复。".to_string(),
                reasoning_content: String::new(),
                worker_id: None,
                media: Vec::new(),
                created_at: now_text(),
            });
        }

        let assembler = PromptAssembler::new(engine.context_limit);
        let assembled = assembler.assemble(
            session,
            "",
            tools.to_vec(),
            engine.models_config(),
            engine.agent_config(),
            &prompt_loop_context,
        );

        let mut system_prompt = assembled.final_system_prompt();
        // 将 recall_memory 检索到的历史上下文追加到 system prompt
        if let Some(ctx) = memory_context.as_deref() {
            system_prompt.push_str(
                "\n\n---\n## 历史上下文（回忆系统注入）\n\
以下内容来自 recall_memory 工具的检索结果，仅供当前回复参考，\
请勿重复回忆，除非用户有新的回忆需求：\n",
            );
            system_prompt.push_str(ctx);
        }
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
        let sink = ThrottledStreamSink::new(pending_msg_id.clone(), stream_tx.clone());
        let response_result =
            engine
                .client()
                .complete_with_functions_stream(&req, tools, &mut |delta| {
                    sink.push_chunk(delta);
                });
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
            session.append_message(MessageRole::System, format_llm_output_message(&output));

            // 最终回复落盘（确保崩溃时不丢失）
            session.persist_to_disk();

            let _ = stream_tx.send(StreamEvent::Done {
                usage: Some(tiangong_types::TokenUsage {
                    prompt_tokens: accumulated_usage.prompt_tokens,
                    completion_tokens: accumulated_usage.completion_tokens,
                    total_tokens: accumulated_usage.total_tokens,
                }),
            });
            return accumulated_usage;
        }

        // 工具调用
        let tool_names: Vec<String> = response.tool_calls.iter().map(|c| c.name.clone()).collect();

        // 记录 LLM 输出到 session
        let output = LlmOutputRecord {
            stage: format!("react-round-{round}"),
            content: response.text.clone(),
            reasoning_content: response.reasoning_content.clone(),
            tool_calls: tool_names.clone(),
            usage: response.usage.clone(),
        };
        session.append_message_with_reasoning(
            MessageRole::System,
            format_llm_output_message(&output),
            response.reasoning_content.clone(),
        );

        // 推送工具调用事件
        let _ = stream_tx.send(StreamEvent::ToolCalls {
            names: tool_names.clone(),
            usage: Some(tiangong_types::TokenUsage {
                prompt_tokens: response.usage.prompt_tokens,
                completion_tokens: response.usage.completion_tokens,
                total_tokens: response.usage.total_tokens,
            }),
        });

        // 记录 assistant 意图到 loop_context
        let assistant_text = if response.text.is_empty() {
            format!("[调用工具: {}]", tool_names.join(", "))
        } else {
            response.text.clone()
        };
        loop_context.push(Message {
            id: scru128::new().to_string(),
            role: MessageRole::Assistant,
            content: assistant_text,
            reasoning_content: response.reasoning_content.clone(),
            worker_id: None,
            media: Vec::new(),
            created_at: now_text(),
        });

        // 执行工具
        for call in &response.tool_calls {
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
                        ok: false,
                        output: format!("权限拒绝：{reason}"),
                    });
                    loop_context.push(Message {
                        id: scru128::new().to_string(),
                        role: MessageRole::User,
                        content: format!("工具 {} 执行失败：权限拒绝 - {reason}", call.name),
                        reasoning_content: String::new(),
                        worker_id: None,
                        media: Vec::new(),
                        created_at: now_text(),
                    });
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
                            Ok(Command::Message(content)) => {
                                session.append_message(MessageRole::User, content.clone());
                                loop_context.push(Message {
                                    id: scru128::new().to_string(),
                                    role: MessageRole::User,
                                    content,
                                    reasoning_content: String::new(),
                                    worker_id: None,
                                    media: Vec::new(),
                                    created_at: now_text(),
                                });
                            }
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
                        session.append_message(
                            MessageRole::System,
                            format!("工具 {} 被用户拒绝执行", call.name),
                        );
                        session.persist_to_disk();

                        let _ = stream_tx.send(StreamEvent::ToolResult {
                            name: call.name.clone(),
                            ok: false,
                            output: "用户拒绝执行".to_string(),
                        });
                        // 拒绝后结束本轮，避免 LLM 再次调用同工具形成死循环
                        let _ = stream_tx.send(StreamEvent::Done {
                            usage: Some(tiangong_types::TokenUsage {
                                prompt_tokens: accumulated_usage.prompt_tokens,
                                completion_tokens: accumulated_usage.completion_tokens,
                                total_tokens: accumulated_usage.total_tokens,
                            }),
                        });
                        return accumulated_usage;
                    }
                }
            }

            let _ = stream_tx.send(StreamEvent::ToolStart {
                name: call.name.clone(),
                args_summary: args_summary.clone(),
            });

            let (result, memory_tool_usage) = if call.name == "recall_memory" {
                execute_memory_recall_tool(call, memory_handle, session)
            } else {
                (
                    engine.execute_tool_call(call, mcp_targets, &engine.agent_config().mcp),
                    tiangong_types::TokenUsage::default(),
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
                ok: result.ok,
                output: result.stdout.clone(),
            });

            // 记录到 session
            session.append_message(MessageRole::System, format_tool_trace_message(&result));

            // 记录到 loop_context（完整内容，截断由上下文压缩器处理）
            // 媒体生成工具使用摘要反馈（避免 base64 数据污染上下文）
            let is_media_tool = matches!(
                call.name.as_str(),
                "generate_image" | "generate_video" | "text_to_speech" | "speech_to_text"
            );
            if result.ok {
                pending_media_assets.extend(parse_media_assets_from_tool_result(
                    &call.name,
                    &result.stdout,
                    &result.summary,
                ));
            }
            let feedback = if is_media_tool && result.ok {
                format!(
                    "工具 {} 执行成功：{}。媒体内容已生成并交付给用户，不要再次调用该工具。请直接给出文本回复。",
                    call.name, result.summary
                )
            } else {
                format!(
                    "工具 {} 执行{}：{}",
                    call.name,
                    if result.ok { "成功" } else { "失败" },
                    if result.stdout.is_empty() {
                        result.summary.clone()
                    } else {
                        result.stdout.clone()
                    }
                )
            };
            // recall_memory 的检索内容注入 system prompt，loop_context 只记录简短通知
            if call.name == "recall_memory" && result.ok {
                if !result.stdout.trim().is_empty() {
                    *memory_context = Some(result.stdout.clone());
                }
                let notice = if result.stdout.trim().is_empty() {
                    "recall_memory 执行完成：未找到增量历史记忆。".to_string()
                } else {
                    "recall_memory 执行完成：历史上下文已注入 system prompt，请直接参考使用，无需再次调用此工具。".to_string()
                };
                loop_context.push(Message {
                    id: scru128::new().to_string(),
                    role: MessageRole::System,
                    content: notice,
                    reasoning_content: String::new(),
                    worker_id: None,
                    media: Vec::new(),
                    created_at: now_text(),
                });
            } else {
                loop_context.push(Message {
                    id: scru128::new().to_string(),
                    role: MessageRole::User,
                    content: feedback,
                    reasoning_content: String::new(),
                    worker_id: None,
                    media: Vec::new(),
                    created_at: now_text(),
                });
            }
        }

        // 工具调用完成后增量持久化（防止崩溃丢失中间数据）
        session.persist_to_disk();

        // 每轮之间检查用户命令（非阻塞）
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Command::Cancel => {
                    let _ = stream_tx.send(StreamEvent::Error {
                        message: "已取消".into(),
                    });
                    return accumulated_usage;
                }
                Command::Message(content) => {
                    // 用户追加消息：注入 loop_context，下一轮 LLM 会看到
                    session.append_message(MessageRole::User, content.clone());
                    loop_context.push(Message {
                        id: scru128::new().to_string(),
                        role: MessageRole::User,
                        content,
                        reasoning_content: String::new(),
                        worker_id: None,
                        media: Vec::new(),
                        created_at: now_text(),
                    });
                }
                Command::Shutdown => return accumulated_usage,
                _ => {}
            }
        }

        // 继续下一轮
    }

    accumulated_usage
}

/// 超限时强制最终回复
fn force_final_response(
    session: &mut Session,
    loop_context: &[Message],
    engine: &RuntimeEngine,
    stream_tx: &Sender<StreamEvent>,
) {
    // 将强制回复提示作为 System 消息注入上下文
    let mut final_context = loop_context.to_vec();
    final_context.push(Message {
        id: scru128::new().to_string(),
        role: MessageRole::System,
        content: "请基于以上所有工具执行结果，直接给出最终回复。".to_string(),
        reasoning_content: String::new(),
        worker_id: None,
        media: Vec::new(),
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
        usage: Some(tiangong_types::TokenUsage {
            prompt_tokens: resp.usage.prompt_tokens,
            completion_tokens: resp.usage.completion_tokens,
            total_tokens: resp.usage.total_tokens,
        }),
    });
}

/// 从 CoreConfig 快照构建 RuntimeEngine
///
/// `stream_tx` 用于在 LLM 请求重试时发送 `StreamEvent::Retry` 通知。
/// `shared_trust_mode` 是 TiangongCore 持有的独立信任模式，RuntimeEngine 共享此引用。
fn build_engine_from_config(
    config: &crate::core_config::CoreConfig,
    stream_tx: &Sender<StreamEvent>,
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
) -> (crate::tool::ToolResult, tiangong_types::TokenUsage) {
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
        );
    }

    let stdout = if response.content.trim().is_empty() {
        "没有发现当前上下文之外的增量记忆。".to_string()
    } else {
        response.content
    };

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
                MessageRole::System => {
                    if !message.content.starts_with("工具执行")
                        && !message.content.starts_with("LLM 输出")
                    {
                        return None;
                    }
                    "system"
                }
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
        if message.role != MessageRole::System {
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

        if message.role != MessageRole::System {
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
