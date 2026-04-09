//! TiangongCore：天工智能体核心
//!
//! 单一线程完成所有工作：接收消息 → LLM 调用 → 工具执行 → session 更新 → 推送 StreamEvent
//! CLI / GUI / Server / Connector 统一通过 TiangongCore 运行。

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};

use crate::agents::execution_mcp_agent::{McpFunctionTarget, execution_function_tools};
use crate::app_state::formatting::{format_llm_output_message, format_tool_trace_message};
use crate::coordinator::types::CoordinatorTask;
use crate::coordinator::TaskCoordinator;
use crate::core_config::CoreConfigProvider;
use crate::model::{
    FunctionToolSpec, ModelClient, ModelRequest, ModelStreamChunk, SingleProviderClient, TokenUsage,
};
use crate::prompt::PromptAssembler;
use crate::runtime::{LlmOutputRecord, RuntimeEngine, inject_enhanced_tools, use_stream_mode};
use crate::session::{Message, MessageRole, Session, now_text};
use tiangong_types::{SessionStreamEvent, StreamEvent};

const MAX_ROUNDS: usize = 20;

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
        let initial_trust_mode = config.snapshot().trust_mode;
        let shared_trust_mode = Arc::new(RwLock::new(initial_trust_mode));
        let session_id = session.id.clone();
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();

        let worker_trust_mode = shared_trust_mode.clone();
        let worker =
            thread::spawn(move || worker_loop(config, session, stream_tx, cmd_rx, worker_trust_mode));

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
) -> Session {
    let session_id = session.id.clone();
    let mut last_cfg_gen = 0u64;
    let mut engine: Option<RuntimeEngine> = None;
    let mut tools: Vec<FunctionToolSpec> = Vec::new();
    let mut mcp_targets: HashMap<String, McpFunctionTarget> = HashMap::new();

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

    while let Ok(cmd) = cmd_rx.recv() {
        // 配置变更检测：仅在 generation 变化时重建 engine 和工具列表
        let cfg_gen = config.generation();
        if engine.is_none() || cfg_gen != last_cfg_gen {
            let cfg = config.snapshot();
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
            tools = new_tools;
            mcp_targets = new_mcp_targets;
            last_cfg_gen = cfg_gen;
        }

        match cmd {
            Command::Message(content) => {
                // 记录用户消息
                session.append_message(MessageRole::User, content.clone());
                // 通知消费端：用户消息已记录（携带 session 中的 message_id）
                let user_msg_id = session.messages.last().map(|m| m.id.clone()).unwrap_or_default();
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
                );
            }
            Command::Cancel => {
                // 当前简单处理：发送错误事件
                let _ = stream_tx.send(StreamEvent::Error {
                    message: "已取消".into(),
                });
            }
            Command::Approval { .. } => {
                // TODO: 审批响应处理
            }
            Command::Shutdown => break,
        }
    }

    // 关闭内部通道，等待转发线程结束
    drop(stream_tx);
    let _ = forward_handle.join();

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
    execute_turn_inner(session, user_input, engine, tools, mcp_targets, stream_tx, cmd_rx, max_rounds)
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
                    session, user_input, engine, tools, mcp_targets, stream_tx, cmd_rx, MAX_ROUNDS,
                );
            }
        }
        return;
    }

    execute_turn_inner(session, user_input, engine, tools, mcp_targets, stream_tx, cmd_rx, MAX_ROUNDS);
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
) -> TokenUsage {
    let mut loop_context: Vec<Message> = Vec::new();
    let mut round = 0;
    let mut accumulated_usage = TokenUsage::default();

    loop {
        if round >= max_rounds {
            // 超限：强制最终回复
            force_final_response(session, &loop_context, engine, stream_tx);
            break;
        }

        // 构建 prompt
        // 用户输入统一通过 session.messages 传递，不使用独立的 user_input 通道
        // 工具调用后续轮次的续写提示作为 System 消息注入 loop_context
        if round > 0 {
            loop_context.push(Message {
                id: scru128::new().to_string(),
                role: MessageRole::System,
                content: "根据上面的工具执行结果继续处理。如果已经收集到足够信息，直接给出最终回复，不要再调用工具。".to_string(),
                reasoning_content: String::new(),
                worker_id: None,
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
            &loop_context,
        );

        let system_prompt = assembled.final_system_prompt();
        let req = ModelRequest {
            session_title: session.title.clone(),
            user_input: assembled.user_input.clone(),
            context: assembled.build_messages(),
            assembled_system_prompt: Some(system_prompt),
        };

        // 预生成本轮 assistant 消息 ID（Delta/Reasoning 事件先于消息创建）
        let pending_msg_id = scru128::new().to_string();

        // LLM 流式调用
        let tx = stream_tx.clone();
        let msg_id_for_cb = pending_msg_id.clone();
        let response = match engine.client().complete_with_functions_stream(
            &req,
            tools,
            &mut |delta: &ModelStreamChunk| {
                // 直接推送 StreamEvent（携带 message_id）
                if !delta.content.is_empty() {
                    let _ = tx.send(StreamEvent::Delta {
                        message_id: msg_id_for_cb.clone(),
                        content: delta.content.clone(),
                    });
                }
                if !delta.reasoning_content.is_empty() {
                    let _ = tx.send(StreamEvent::Reasoning {
                        message_id: msg_id_for_cb.clone(),
                        content: delta.reasoning_content.clone(),
                    });
                }
            },
        ) {
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
            session.append_message_with_id(
                pending_msg_id,
                MessageRole::Assistant,
                response.text.clone(),
                response.reasoning_content.clone(),
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
        let tool_names: Vec<String> =
            response.tool_calls.iter().map(|c| c.name.clone()).collect();

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
            created_at: now_text(),
        });

        // 执行工具
        for call in &response.tool_calls {
            let args_summary = format_call_args_summary(call);

            // 权限检查（在执行前完成，trust_mode 通过 shared Arc 实时生效）
            use crate::permission::PermissionDecision;
            let decision = engine.check_tool_permission(&call.name);
            match decision {
                PermissionDecision::Approved => {}
                PermissionDecision::Denied { reason } => {
                    let _ = stream_tx.send(StreamEvent::ToolResult {
                        name: call.name.clone(),
                        ok: false,
                        output: format!("权限拒绝：{reason}"),
                    });
                    loop_context.push(Message {
                        id: scru128::new().to_string(),
                        role: MessageRole::System,
                        content: format!("工具 {} 执行失败：权限拒绝 - {reason}", call.name),
                        reasoning_content: String::new(),
                        worker_id: None,
                        created_at: now_text(),
                    });
                    continue;
                }
                PermissionDecision::NeedsApproval { request_id } => {
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
                                session
                                    .append_message(MessageRole::User, content.clone());
                                loop_context.push(Message {
                                    id: scru128::new().to_string(),
                                    role: MessageRole::User,
                                    content,
                                    reasoning_content: String::new(),
                                    worker_id: None,
                                    created_at: now_text(),
                                });
                            }
                            Ok(Command::Shutdown) => return accumulated_usage,
                            _ => {}
                        }
                    };

                    if !approved {
                        let _ = stream_tx.send(StreamEvent::ToolResult {
                            name: call.name.clone(),
                            ok: false,
                            output: "用户拒绝执行".to_string(),
                        });
                        session.append_message(
                            MessageRole::System,
                            format!("工具 {} 被用户拒绝执行", call.name),
                        );
                        loop_context.push(Message {
                            id: scru128::new().to_string(),
                            role: MessageRole::System,
                            content: format!("工具 {} 被用户拒绝执行", call.name),
                            reasoning_content: String::new(),
                            worker_id: None,
                            created_at: now_text(),
                        });
                        continue;
                    }
                }
            }

            let _ = stream_tx.send(StreamEvent::ToolStart {
                name: call.name.clone(),
                args_summary: args_summary.clone(),
            });

            let result =
                engine.execute_tool_call(call, mcp_targets, &engine.agent_config().mcp);

            let _ = stream_tx.send(StreamEvent::ToolResult {
                name: call.name.clone(),
                ok: result.ok,
                output: result.stdout.clone(),
            });

            // 记录到 session
            session.append_message(MessageRole::System, format_tool_trace_message(&result));

            // 记录到 loop_context
            let feedback = format!(
                "工具 {} 执行{}：{}",
                call.name,
                if result.ok { "成功" } else { "失败" },
                if result.stdout.chars().count() > 2000 {
                    let truncated: String = result.stdout.chars().take(2000).collect();
                    format!("{truncated}...(截断)")
                } else if result.stdout.is_empty() {
                    result.summary.clone()
                } else {
                    result.stdout.clone()
                }
            );
            loop_context.push(Message {
                id: scru128::new().to_string(),
                role: MessageRole::System,
                content: feedback,
                reasoning_content: String::new(),
                worker_id: None,
                created_at: now_text(),
            });
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
    };

    // 预生成 message_id
    let pending_msg_id = scru128::new().to_string();

    let tx = stream_tx.clone();
    let msg_id_for_cb = pending_msg_id.clone();
    let resp = if use_stream_mode() {
        match engine
            .client()
            .complete_stream_with_callback(&req, |delta| {
                if !delta.content.is_empty() {
                    let _ = tx.send(StreamEvent::Delta {
                        message_id: msg_id_for_cb.clone(),
                        content: delta.content.clone(),
                    });
                }
            }) {
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

    session.append_message_with_id(pending_msg_id, MessageRole::Assistant, resp.text, String::new());
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
            api_model: lite_endpoint.model.clone(),
            api_lite_model: lite_endpoint.model.clone(),
        };
        engine = engine.with_lite_client(
            SingleProviderClient::new(lite_config).with_on_retry(on_retry),
        );
    }

    engine
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
            let shell = args
                .get("shell")
                .and_then(Value::as_str)
                .unwrap_or("auto");
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
