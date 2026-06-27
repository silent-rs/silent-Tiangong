//! ReactEngine: 单个 agent 的 async ReAct 循环
//!
//! 所有执行路径统一经过 `ReactEngine::execute_turn`，消除 sync/async 双版本。

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::mpsc::{Receiver as StdReceiver, Sender as StdSender};
use std::sync::{Arc, Mutex};

use futures_util::{StreamExt, stream::FuturesUnordered};
use tokio::sync::mpsc as tokio_mpsc;

use crate::agents::execution_mcp_agent::McpFunctionTarget;
use crate::app_state::formatting::{format_llm_output_message, format_tool_trace_message};
use crate::context::assembler::filter_background_task_tools;
use crate::core::command::{Command, PendingCommandEffect};
use crate::model::{ModelRequest, TokenUsage, ToolSpec};
use crate::observe::{audit_permission_with_context, audit_tool_execution};
use crate::permission::{
    PermissionDecision, evaluate_tool_permission, format_call_args_summary, infer_audit_target,
    normalize_permission_target,
};
use crate::react::context::{
    ForceFinalReason, emit_token_usage, force_final_response, maybe_update_context_summary,
    persist_error, request_for_summary_phase, select_client_for_request,
};
use crate::react::message::*;
use crate::runtime::LlmOutputRecord;
use crate::session::{Message, MessageRole, Session, now_text};
use crate::stream_throttle::ThrottledStreamSink;
use tiangong_types::{StreamEvent, StreamToolCall};

use crate::agent_team::lifecycle::TeamContext;

/// 单个 turn 内的执行阶段。
///
/// ReAct Loop 与总结阶段分离后的阶段状态机：
/// - `Initial`：外层循环第一次迭代，主模型决定是否需要工具。
/// - `ToolExecution`：工具执行阶段，LLM 只负责调用工具。
/// - `Summary`：总结阶段，主模型判断任务完成度并输出最终回复。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnPhase {
    Initial,
    ToolExecution,
    Summary,
}

/// 总结阶段的执行结果。
enum SummaryPhaseResult {
    /// 任务完成，已输出最终回复。
    Completed(TokenUsage),
    /// 任务未完成，需要重新进入工具执行阶段。
    NeedMoreWork { reason: String, usage: TokenUsage },
    /// 用户取消。
    Cancelled(TokenUsage),
    /// 总结阶段 LLM 请求失败。
    Failed { message: String, usage: TokenUsage },
}

/// 取消时 LLM 请求的处理策略，按 Provider 协议区分。
enum CancelStrategy {
    /// Anthropic: usage 在 message_start 就返回 prompt_tokens，可直接 abort
    AbortWithStreamingUsage,
    /// OpenAI 兼容: usage 仅在流式最后一个 chunk 返回，需等请求完成
    WaitForUsage,
}

impl CancelStrategy {
    fn from_protocol(protocol: tiangong_llm::model::ProviderProtocol) -> Self {
        match protocol {
            tiangong_llm::model::ProviderProtocol::Anthropic => {
                CancelStrategy::AbortWithStreamingUsage
            }
            tiangong_llm::model::ProviderProtocol::OpenAiChatCompletions
            | tiangong_llm::model::ProviderProtocol::DeepSeek => CancelStrategy::WaitForUsage,
        }
    }
}

/// select! 循环 break 时携带的取消信号，替代魔术字符串。
#[derive(Clone, Copy)]
enum CancelSignal {
    /// 直接 abort LLM future，上报已累积的 streaming_usage
    Abort,
    /// 在后台等待 LLM 自然完成以获取 usage，前端立即响应取消
    WaitForUsage,
}

impl CancelSignal {
    /// 从 anyhow::Error 中提取 CancelSignal，如果不是取消信号返回 None。
    fn from_error(err: &anyhow::Error) -> Option<Self> {
        err.downcast_ref::<Self>().copied()
    }
}

impl std::fmt::Display for CancelSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CancelSignal::Abort => write!(f, "cancelled (abort)"),
            CancelSignal::WaitForUsage => write!(f, "cancelled (wait for usage)"),
        }
    }
}

impl std::fmt::Debug for CancelSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for CancelSignal {}

/// 取消时统一上报 token usage 并发送 Error 事件。
fn emit_cancel_usage(
    stream_tx: &StdSender<StreamEvent>,
    usage: &tiangong_types::TokenUsage,
    context_limit: usize,
) {
    if usage.total_tokens > 0 {
        emit_token_usage(stream_tx, usage, None, context_limit, "cancelled", None);
    }
    let _ = stream_tx.send(StreamEvent::Error {
        message: "已取消".into(),
    });
}

#[derive(Default)]
struct SubAgentDrainResult {
    usage: TokenUsage,
    ran: bool,
    cancelled: bool,
}

type SubResult = (String, String, String, Session, Vec<Message>, TokenUsage);
type SubAgentFuture = Pin<Box<dyn Future<Output = SubResult>>>;
type ActiveSubAgent = (String, String, String, tokio_mpsc::UnboundedSender<Command>);

fn sub_agent_stream_message(
    id: impl Into<String>,
    role: MessageRole,
    content: impl Into<String>,
    reasoning_content: impl Into<String>,
) -> Message {
    Message {
        id: id.into(),
        role,
        content: vec![crate::session::ContentBlock::text(content.into())],
        reasoning_content: reasoning_content.into(),
        reasoning_signature: None,
        worker_id: None,
        media: Vec::new(),
        media_migrated: true,
        elapsed_ms: None,
        turn_status: None,
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: None,
        tool_result_is_error: false,
        compact: false,
        phase: crate::session::MessagePhase::Normal,
        created_at: now_text(),
    }
}

fn send_sub_agent_output(
    parent_tx: &StdSender<StreamEvent>,
    agent_id: &str,
    agent_role: &str,
    agent_label: &str,
    message: Message,
) {
    let _ = parent_tx.send(StreamEvent::AgentOutput {
        agent_id: agent_id.to_string(),
        agent_role: agent_role.to_string(),
        agent_label: agent_label.to_string(),
        messages: vec![message],
    });
}

fn spawn_sub_agent_stream_forwarder(
    agent_id: String,
    agent_role: String,
    agent_label: String,
    parent_tx: StdSender<StreamEvent>,
    child_rx: StdReceiver<StreamEvent>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        for event in child_rx {
            match event {
                StreamEvent::UserMessage {
                    message_id,
                    content,
                    ..
                } => send_sub_agent_output(
                    &parent_tx,
                    &agent_id,
                    &agent_role,
                    &agent_label,
                    sub_agent_stream_message(
                        format!("agent:{agent_id}:user:{message_id}"),
                        MessageRole::User,
                        content,
                        "",
                    ),
                ),
                StreamEvent::Delta {
                    message_id,
                    content,
                }
                | StreamEvent::ReactText {
                    message_id,
                    content,
                }
                | StreamEvent::SummaryText {
                    message_id,
                    content,
                } => send_sub_agent_output(
                    &parent_tx,
                    &agent_id,
                    &agent_role,
                    &agent_label,
                    sub_agent_stream_message(
                        format!("agent:{agent_id}:assistant:{message_id}"),
                        MessageRole::Assistant,
                        content,
                        "",
                    ),
                ),
                StreamEvent::Reasoning {
                    message_id,
                    content,
                } => send_sub_agent_output(
                    &parent_tx,
                    &agent_id,
                    &agent_role,
                    &agent_label,
                    sub_agent_stream_message(
                        format!("agent:{agent_id}:assistant:{message_id}"),
                        MessageRole::Assistant,
                        "",
                        content,
                    ),
                ),
                StreamEvent::ToolCalls {
                    message_id,
                    names,
                    usage,
                    ..
                } => {
                    let usage_text = usage
                        .map(|usage| {
                            format!(
                                "\ntokens: prompt={}, completion={}, total={}",
                                usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
                            )
                        })
                        .unwrap_or_default();
                    send_sub_agent_output(
                        &parent_tx,
                        &agent_id,
                        &agent_role,
                        &agent_label,
                        sub_agent_stream_message(
                            format!("agent:{agent_id}:tool-calls:{message_id}"),
                            MessageRole::System,
                            format!("LLM 输出{usage_text}\ntool_calls: {}", names.join(", ")),
                            "",
                        ),
                    );
                }
                StreamEvent::ToolStart { name, args_summary } => {
                    let mut content = format!("工具开始 [{name}]");
                    if !args_summary.is_empty() {
                        content.push_str(&format!("\n命令: {args_summary}"));
                    }
                    send_sub_agent_output(
                        &parent_tx,
                        &agent_id,
                        &agent_role,
                        &agent_label,
                        sub_agent_stream_message(
                            format!("agent:{agent_id}:tool-start:{}", scru128::new()),
                            MessageRole::System,
                            content,
                            "",
                        ),
                    );
                }
                StreamEvent::ToolResult {
                    name,
                    tool_call_id,
                    ok,
                    output,
                    full_output,
                    ..
                } => {
                    let persisted_output = full_output.unwrap_or(output);
                    let mut content = format!(
                        "工具执行 [{name}]\nok={} exit_code={}",
                        ok,
                        if ok { 0 } else { 1 }
                    );
                    if !persisted_output.trim().is_empty() {
                        content.push_str(&format!("\nstdout:\n{persisted_output}"));
                    }
                    send_sub_agent_output(
                        &parent_tx,
                        &agent_id,
                        &agent_role,
                        &agent_label,
                        sub_agent_stream_message(
                            tool_call_id.unwrap_or_else(|| {
                                format!("agent:{agent_id}:tool-result:{}", scru128::new())
                            }),
                            MessageRole::System,
                            content,
                            "",
                        ),
                    );
                }
                StreamEvent::Retry {
                    message,
                    attempt,
                    max_attempts,
                } => send_sub_agent_output(
                    &parent_tx,
                    &agent_id,
                    &agent_role,
                    &agent_label,
                    sub_agent_stream_message(
                        format!("agent:{agent_id}:retry:{}", scru128::new()),
                        MessageRole::System,
                        format!("LLM 请求重试中（{attempt}/{max_attempts}）：{message}"),
                        "",
                    ),
                ),
                StreamEvent::ApprovalNeeded {
                    tool_name,
                    args_summary,
                    ..
                } => send_sub_agent_output(
                    &parent_tx,
                    &agent_id,
                    &agent_role,
                    &agent_label,
                    sub_agent_stream_message(
                        format!("agent:{agent_id}:approval:{}", scru128::new()),
                        MessageRole::System,
                        format!("等待确认：{tool_name} {args_summary}"),
                        "",
                    ),
                ),
                StreamEvent::AgentCreated { .. }
                | StreamEvent::AgentStatusChanged { .. }
                | StreamEvent::AgentNotification { .. }
                | StreamEvent::AgentMessage { .. }
                | StreamEvent::FileLockChanged { .. }
                | StreamEvent::PhaseChanged { .. } => {
                    let _ = parent_tx.send(event);
                }
                StreamEvent::TokenUsage {
                    usage,
                    current_tokens,
                    compression_threshold_tokens,
                    context_limit_tokens,
                    source,
                    ..
                } => {
                    let _ = parent_tx.send(StreamEvent::TokenUsage {
                        usage,
                        current_tokens,
                        compression_threshold_tokens,
                        context_limit_tokens,
                        source,
                        agent_id: Some(agent_id.clone()),
                    });
                }
                StreamEvent::Error { message } => {
                    let _ = parent_tx.send(StreamEvent::AgentNotification {
                        agent_id: agent_id.clone(),
                        agent_label: agent_label.clone(),
                        content: format!("执行出错：{message}"),
                        level: "error".to_string(),
                    });
                    send_sub_agent_output(
                        &parent_tx,
                        &agent_id,
                        &agent_role,
                        &agent_label,
                        sub_agent_stream_message(
                            format!("agent:{agent_id}:error:{}", scru128::new()),
                            MessageRole::System,
                            format!("执行出错：{message}"),
                            "",
                        ),
                    );
                }
                _ => {}
            }
        }
    })
}

/// 单个 agent 的 async ReAct 执行引擎
pub(crate) struct ReactEngine {
    engine: crate::runtime::RuntimeEngine,
    tools: Vec<ToolSpec>,
    mcp_targets: HashMap<String, McpFunctionTarget>,
    /// 单次工具执行阶段（ReAct Loop 内层）的最大轮次。
    max_tool_rounds: usize,
    /// 总结阶段后重新进入工具执行阶段的最大次数。
    max_outer_iterations: u32,
    team: Option<Arc<Mutex<TeamContext>>>,
    agent_id: String,
}

impl ReactEngine {
    pub(crate) fn new(
        engine: crate::runtime::RuntimeEngine,
        tools: Vec<ToolSpec>,
        mcp_targets: HashMap<String, McpFunctionTarget>,
        max_tool_rounds: usize,
        max_outer_iterations: u32,
    ) -> Self {
        Self {
            engine,
            tools,
            mcp_targets,
            max_tool_rounds,
            max_outer_iterations,
            team: None,
            agent_id: "main".to_string(),
        }
    }

    fn build_thinking_config(
        &self,
    ) -> (
        Option<crate::model::ThinkingConfig>,
        Option<crate::model::ReasoningEffort>,
        bool,
    ) {
        let effort_str = self
            .engine
            .agent_config()
            .reasoning_effort
            .trim()
            .to_lowercase();
        match effort_str.as_str() {
            "none" | "" => (None, None, true),
            "low" => (
                Some(crate::model::ThinkingConfig {
                    budget_tokens: 4096,
                }),
                Some(crate::model::ReasoningEffort::Low),
                false,
            ),
            "medium" => (
                Some(crate::model::ThinkingConfig {
                    budget_tokens: 4096,
                }),
                Some(crate::model::ReasoningEffort::Medium),
                false,
            ),
            "high" => (
                Some(crate::model::ThinkingConfig {
                    budget_tokens: 8192,
                }),
                Some(crate::model::ReasoningEffort::High),
                false,
            ),
            "max" => (
                Some(crate::model::ThinkingConfig {
                    budget_tokens: 16384,
                }),
                Some(crate::model::ReasoningEffort::Max),
                false,
            ),
            _ => (
                Some(crate::model::ThinkingConfig {
                    budget_tokens: 4096,
                }),
                Some(crate::model::ReasoningEffort::Medium),
                false,
            ),
        }
    }

    /// 使用已有团队上下文执行指定 Agent。
    pub(crate) fn with_shared_team(
        mut self,
        team: Arc<Mutex<TeamContext>>,
        agent_id: String,
    ) -> Self {
        self.team = Some(team);
        self.agent_id = agent_id;
        self
    }

    /// 执行总结阶段。
    ///
    /// 由主模型（非 lite 模型）基于工具执行阶段的全部结果，判断任务完成度：
    /// - 完成 → 输出最终回复（Summary phase），返回 `Completed`
    /// - 需要用户输入 → 输出提问，返回 `Completed`（视作本轮结束）
    /// - 仍有遗漏且可继续 → 输出 [NEED_MORE_WORK]，返回 `NeedMoreWork`
    /// - 取消 → 返回 `Cancelled`
    /// - LLM 错误 → 返回 `Failed`
    async fn run_summary_phase(
        &self,
        session: &mut Session,
        stream_tx: &StdSender<StreamEvent>,
        cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
        iteration: u32,
    ) -> SummaryPhaseResult {
        let _phase = TurnPhase::Summary;
        let _ = stream_tx.send(StreamEvent::PhaseChanged {
            phase: "summary".to_string(),
            iteration,
        });

        if session.system_prompt_message.is_none() {
            crate::react::context::rebuild_system_prompt(session, &self.engine);
        }

        let req = request_for_summary_phase(session);
        let pending_msg_id = scru128::new().to_string();
        let sink = ThrottledStreamSink::with_text_kind(
            pending_msg_id.clone(),
            stream_tx.clone(),
            crate::stream_throttle::StreamTextKind::Summary,
        );

        let (chunk_tx, mut chunk_rx) =
            tokio_mpsc::unbounded_channel::<crate::model::ModelStreamChunk>();
        let client = select_client_for_request(&self.engine, &req).clone();
        let req_clone = req.clone();
        // 传与 ReAct 阶段相同的 tools schema（按相同 intent 过滤），保持 KV cache
        // 前缀一致；不传 tools 会导致 tools 段缺失，从该位置起 cache miss。
        // 模型不会真正调用工具——SUMMARY_PHASE_PROMPT 已指示只输出文本回复。
        let summary_tools = tools_for_current_turn(&self.tools, session, "");
        let mut llm_fut = Some(tokio::task::spawn(async move {
            client
                .stream_function_calls_with_tool_choice(req_clone, summary_tools, None, chunk_tx)
                .await
        }));
        let mut streaming_usage = tiangong_types::TokenUsage::default();
        let cancel_strategy = CancelStrategy::from_protocol(self.engine.client().protocol());

        let response_result: anyhow::Result<crate::model::ModelFunctionResponse> = loop {
            tokio::select! {
                biased;
                cmd_opt = cmd_rx.recv() => {
                    match cmd_opt {
                        Some(Command::Cancel) | Some(Command::Shutdown) | None => {
                            break Err(match cancel_strategy {
                                CancelStrategy::AbortWithStreamingUsage => {
                                    anyhow::Error::new(CancelSignal::Abort)
                                }
                                CancelStrategy::WaitForUsage => {
                                    anyhow::Error::new(CancelSignal::WaitForUsage)
                                }
                            });
                        }
                        Some(Command::Message { content, message_id, media }) => {
                            let mid = append_or_reuse_user_message(session, &content, message_id, media);
                            let media = session
                                .messages
                                .iter()
                                .find(|message| message.id == mid)
                                .map(|message| message.media.clone())
                                .unwrap_or_default();
                            let _ = stream_tx.send(StreamEvent::UserMessage {
                                message_id: mid,
                                content: content.clone(),
                                media,
                            });
                        }
                        Some(Command::UpdateCwd { cwd }) => {
                            session.cwd = cwd;
                            crate::core::apply_session_cwd(session);
                        }
                        Some(Command::ReloadConfig) => {}
                        Some(Command::Approval { .. }) => {}
                        Some(Command::CancelAgent { .. }) => {}
                        Some(Command::InjectTool { tool_name, payload }) => {
                            crate::react::message::inject_tool_to_session(
                                session,
                                stream_tx,
                                &tool_name,
                                &payload,
                            );
                        }
                        Some(Command::CompressContext) => {
                            crate::core::compress_context_for_session(
                                session,
                                &self.engine,
                                stream_tx,
                            );
                        }
                        Some(Command::ResetContext) => {
                            crate::core::reset_context_for_session(
                                session,
                                stream_tx,
                                &self.engine,
                            );
                        }
                    }
                }
                chunk_opt = chunk_rx.recv() => {
                    match chunk_opt {
                        Some(chunk) => {
                            if let Some(ref chunk_usage) = chunk.usage {
                                let tu: tiangong_types::TokenUsage = chunk_usage.clone().into();
                                streaming_usage.accumulate(&tu);
                            }
                            sink.push_chunk(&chunk);
                        }
                        None => {
                            let response_result = match llm_fut.take().unwrap().await {
                                Ok(r) => r,
                                Err(e) if e.is_cancelled() => {
                                    sink.finish();
                                    let _ = stream_tx.send(StreamEvent::Error {
                                        message: "已取消".into(),
                                    });
                                    return SummaryPhaseResult::Cancelled(streaming_usage);
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
            Ok(response) => response,
            Err(err) => {
                if let Some(signal) = CancelSignal::from_error(&err) {
                    match signal {
                        CancelSignal::Abort => {
                            if let Some(handle) = llm_fut.take() {
                                handle.abort();
                            }
                            emit_cancel_usage(
                                stream_tx,
                                &streaming_usage,
                                self.engine.context_limit,
                            );
                            return SummaryPhaseResult::Cancelled(streaming_usage);
                        }
                        CancelSignal::WaitForUsage => {
                            if streaming_usage.total_tokens > 0 {
                                emit_token_usage(
                                    stream_tx,
                                    &streaming_usage,
                                    None,
                                    self.engine.context_limit,
                                    "summary-cancelled",
                                    None,
                                );
                            }
                            let _ = stream_tx.send(StreamEvent::Error {
                                message: "已取消".into(),
                            });
                            if let Some(handle) = llm_fut.take() {
                                let ctx_limit = self.engine.context_limit;
                                let tx = stream_tx.clone();
                                tokio::task::spawn(async move {
                                    if let Ok(Ok(resp)) = handle.await
                                        && resp.usage.total_tokens > 0
                                    {
                                        emit_token_usage(
                                            &tx,
                                            &resp.usage,
                                            None,
                                            ctx_limit,
                                            "summary-cancelled-background",
                                            None,
                                        );
                                    }
                                });
                            }
                            return SummaryPhaseResult::Cancelled(streaming_usage);
                        }
                    }
                }
                return SummaryPhaseResult::Failed {
                    message: err.to_string(),
                    usage: streaming_usage,
                };
            }
        };

        emit_token_usage(
            stream_tx,
            &response.usage,
            Some(response.usage.prompt_tokens.max(session.current_tokens)),
            self.engine.context_limit,
            format!("summary-iteration-{iteration}"),
            None,
        );
        let mut usage = response.usage.clone();
        if usage.total_tokens == 0 {
            usage.accumulate(&streaming_usage);
        }

        // 解析 [NEED_MORE_WORK] 标记，判定是否需要重入工具执行阶段。
        let (needs_more_work, summary_content) = parse_summary_need_more_work(&response.text);
        session.append_message_with_id(
            pending_msg_id,
            MessageRole::Assistant,
            summary_content.clone(),
            response.reasoning_content,
        );
        if let Some(message) = session.messages.last_mut() {
            message.reasoning_signature = response.reasoning_signature;
            // 需要 more work 的总结视为过程性内容；其余视为最终回复。
            message.phase = if needs_more_work {
                crate::session::MessagePhase::React
            } else {
                crate::session::MessagePhase::Summary
            };
        }
        maybe_update_context_summary(session, &self.engine, &usage, stream_tx);
        session.persist_to_disk();

        if needs_more_work {
            SummaryPhaseResult::NeedMoreWork {
                reason: summary_content,
                usage,
            }
        } else {
            SummaryPhaseResult::Completed(usage)
        }
    }

    /// 执行一个完整的对话轮次（可能多轮工具调用），async 版
    ///
    /// 每轮之间检查 cmd_rx：新消息注入上下文，cancel 立即生效。
    #[allow(clippy::too_many_arguments, unreachable_code)]
    pub(crate) async fn execute_turn(
        &mut self,
        session: &mut Session,
        user_input: &str,
        stream_tx: &StdSender<StreamEvent>,
        cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
        memory_handle: Option<&tiangong_memory::MemoryHandle>,
        index_manager: Option<std::sync::Arc<crate::index::IndexManager>>,
    ) -> TokenUsage {
        let mut round = 0usize;
        let mut outer_iteration = 0u32;
        let mut accumulated_usage = TokenUsage::default();
        let mut memory_recall_attempted = false;
        let mut successful_tool_call_keys = HashSet::new();
        let mut failed_tool_call_keys: HashMap<String, String> = HashMap::new();
        let mut failed_tool_names = HashSet::new();
        let mut memory_candidate_count = 0usize;
        let mut last_browser_snapshot: Option<crate::browser_trait::PageSnapshot> = None;
        let mut last_browser_check: Option<std::time::Instant> = None;

        if self.agent_id == "main" {
            let routed = self
                .team
                .as_ref()
                .and_then(|team| {
                    team.lock().ok().map(|mut team| {
                        crate::agent_team::lifecycle::route_user_mentions(
                            &mut team, user_input, stream_tx,
                        )
                    })
                })
                .unwrap_or(false);
            if routed {
                let sub_result = self
                    .drain_sub_agent_inboxes(
                        session,
                        stream_tx,
                        cmd_rx,
                        memory_handle,
                        &index_manager,
                    )
                    .await;
                accumulated_usage.accumulate(&sub_result.usage);
                if sub_result.cancelled {
                    session.persist_to_disk();
                    return accumulated_usage;
                }
                session.persist_to_disk();
                let _ = stream_tx.send(StreamEvent::Done {
                    usage: Some(accumulated_usage.clone()),
                });
                return accumulated_usage;
            }
        }

        // 外层循环：ReAct Loop（工具执行）与总结阶段分离。
        // - 每次迭代先走内层 'react_loop（工具执行阶段），break 后进入总结阶段。
        // - 总结阶段判定未完成且未超 outer 上限时，注入重入上下文后 continue 'outer。
        // - Task 02/03 将实现内层逻辑改造与总结阶段；当前总结阶段为占位。
        'outer: loop {
            let _phase = if outer_iteration == 0 {
                TurnPhase::Initial
            } else {
                TurnPhase::ToolExecution
            };
            let iteration_start_round = round;
            let mut executed_tool_in_iteration = false;

            'react_loop: loop {
                // 首轮：始终重建 system prompt，确保规则段（如「文档附件解析规则」）
                // 与当前代码版本一致。旧 session 持久化的 system_prompt_message 可能
                // 是旧版本生成、缺少新增的规则段，必须重建。
                if round == 0 {
                    crate::react::context::rebuild_system_prompt(session, &self.engine);
                }
                match drain_pending_commands_async(session, &self.engine, stream_tx, cmd_rx) {
                    PendingCommandEffect::Terminate => return accumulated_usage,
                    PendingCommandEffect::MessageInjected => {
                        memory_recall_attempted = false;
                        successful_tool_call_keys.clear();
                        failed_tool_call_keys.clear();
                        failed_tool_names.clear();
                        memory_candidate_count = 0;
                    }
                    PendingCommandEffect::None => {}
                }

                // 首轮：主动获取浏览器当前状态并注入上下文
                if round == 0 {
                    maybe_inject_browser_update(
                        &self.engine,
                        session,
                        stream_tx,
                        &mut last_browser_snapshot,
                        &mut last_browser_check,
                        false,
                    )
                    .await;
                }

                // 内层工具执行阶段轮次上限：达到即结束工具阶段，进入总结。
                // 以本次外层迭代的起始轮次为基准计算，避免重入 Loop 时累计。
                if round.saturating_sub(iteration_start_round) >= self.max_tool_rounds {
                    break 'react_loop;
                }

                let request_tools = tools_for_current_turn(&self.tools, session, user_input);

                let (thinking, reasoning_effort, thinking_disabled) = self.build_thinking_config();
                let req = ModelRequest {
                    session_title: session.title.clone(),
                    // 当前用户消息已在 Command::Message 入口写入 session.messages。
                    // ReAct 多轮继续请求时不能再次追加 user_input，否则模型会把同一请求
                    // 误认为新的用户消息，反复从第一步重新开始。
                    user_input: String::new(),
                    context: session.context(),
                    thinking,
                    reasoning_effort,
                    thinking_disabled,
                    include_media: self.engine.chat_is_multimodal(),
                };

                let pending_msg_id = scru128::new().to_string();
                // 工具执行阶段的流式文本作为过程性输出（ReactText），前端紧凑展示。
                let sink = ThrottledStreamSink::with_text_kind(
                    pending_msg_id.clone(),
                    stream_tx.clone(),
                    crate::stream_throttle::StreamTextKind::React,
                );

                // async 流式调用 + select! 取消
                let (chunk_tx, mut chunk_rx) =
                    tokio_mpsc::unbounded_channel::<crate::model::ModelStreamChunk>();
                let client = select_client_for_request(&self.engine, &req).clone();
                let req_clone = req.clone();
                let tools_clone = request_tools.clone();
                let mut llm_fut = Some(tokio::task::spawn(async move {
                    client
                        .stream_function_calls_with_tool_choice(
                            req_clone,
                            tools_clone,
                            None,
                            chunk_tx,
                        )
                        .await
                }));
                let mut user_message_injected_during_stream = false;
                let mut streaming_usage = tiangong_types::TokenUsage::default();
                let cancel_strategy =
                    CancelStrategy::from_protocol(self.engine.client().protocol());
                let response_result: anyhow::Result<crate::model::ModelFunctionResponse> = loop {
                    tokio::select! {
                        biased;
                        cmd_opt = cmd_rx.recv() => {
                            match cmd_opt {
                                Some(Command::Cancel) | Some(Command::Shutdown) | None => {
                                    match cancel_strategy {
                                        CancelStrategy::AbortWithStreamingUsage => {
                                            break Err(anyhow::Error::new(CancelSignal::Abort));
                                        }
                                        CancelStrategy::WaitForUsage => {
                                            break Err(anyhow::Error::new(CancelSignal::WaitForUsage));
                                        }
                                    }
                                }
                                Some(Command::Message { content, message_id, media }) => {
                                    let mid = append_or_reuse_user_message(
                                        session, &content, message_id, media,
                                    );
                                    let media = session
                                        .messages
                                        .iter()
                                        .find(|message| message.id == mid)
                                        .map(|message| message.media.clone())
                                        .unwrap_or_default();
                                    let _ = stream_tx.send(StreamEvent::UserMessage {
                                        message_id: mid,
                                        content: content.clone(),
                                        media,
                                    });
                                    user_message_injected_during_stream = true;
                        memory_candidate_count = 0;
                                }
                                Some(Command::UpdateCwd { cwd }) => {
                                    session.cwd = cwd;
                                    crate::core::apply_session_cwd(session);
                                }
                                Some(Command::ReloadConfig) => {}
                                Some(Command::Approval { .. }) => {}
                                Some(Command::CancelAgent { .. }) => {}
                                Some(Command::InjectTool { tool_name, payload }) => {
                                    crate::react::message::inject_tool_to_session(
                                        session,
                                        stream_tx,
                                        &tool_name,
                                        &payload,
                                    );
                                }
                                Some(Command::CompressContext) => {
                                    crate::core::compress_context_for_session(
                                        session,
                                        &self.engine,
                                        stream_tx,
                                    );
                                }
                                Some(Command::ResetContext) => {
                                    crate::core::reset_context_for_session(
                                        session,
                                        stream_tx,
                                        &self.engine,
                                    );
                                }
                            }
                        }
                        chunk_opt = chunk_rx.recv() => {
                            match chunk_opt {
                                Some(chunk) => {
                                    if let Some(ref chunk_usage) = chunk.usage {
                                        let tu: tiangong_types::TokenUsage =
                                            chunk_usage.clone().into();
                                        streaming_usage.accumulate(&tu);
                                    }
                                    sink.push_chunk(&chunk)
                                }
                                None => {
                                    let response_result = match llm_fut.take().unwrap().await {
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
                        if let Some(signal) = CancelSignal::from_error(&err) {
                            match signal {
                                CancelSignal::Abort => {
                                    if let Some(handle) = llm_fut.take() {
                                        handle.abort();
                                    }
                                    accumulated_usage.accumulate(&streaming_usage);
                                    emit_cancel_usage(
                                        stream_tx,
                                        &accumulated_usage,
                                        self.engine.context_limit,
                                    );
                                    return accumulated_usage;
                                }
                                CancelSignal::WaitForUsage => {
                                    accumulated_usage.accumulate(&streaming_usage);
                                    // 先上报已知的 streaming_usage 作为保底
                                    if streaming_usage.total_tokens > 0 {
                                        emit_token_usage(
                                            stream_tx,
                                            &streaming_usage,
                                            None,
                                            self.engine.context_limit,
                                            "cancelled",
                                            None,
                                        );
                                    }
                                    // 立即通知前端取消已完成
                                    let _ = stream_tx.send(StreamEvent::Error {
                                        message: "已取消".into(),
                                    });
                                    // 后台等待 LLM 自然完成以补充完整 usage
                                    if let Some(handle) = llm_fut.take() {
                                        let ctx_limit = self.engine.context_limit;
                                        let tx = stream_tx.clone();
                                        tokio::task::spawn(async move {
                                            if let Ok(Ok(resp)) = handle.await
                                                && resp.usage.total_tokens > 0
                                            {
                                                emit_token_usage(
                                                    &tx,
                                                    &resp.usage,
                                                    None,
                                                    ctx_limit,
                                                    "cancelled_background",
                                                    None,
                                                );
                                            }
                                        });
                                    }
                                    return accumulated_usage;
                                }
                            }
                        }
                        let err_msg = err.to_string();
                        // 上下文超限或空响应时强制压缩后重试
                        if err_msg.contains("context_window_exceeded")
                            || err_msg.contains("context_length_exceeded")
                            || (err_msg.contains("content_blocks=0")
                                && err_msg.contains("stop_reason=end_turn"))
                        {
                            tracing::warn!("检测到上下文超限，尝试强制压缩");
                            let before_summary_up_to = session.summary_up_to;
                            crate::react::context::maybe_update_context_summary(
                                session,
                                &self.engine,
                                &tiangong_types::TokenUsage {
                                    prompt_tokens: self.engine.context_limit,
                                    completion_tokens: 0,
                                    total_tokens: self.engine.context_limit,
                                    prompt_cache_hit_tokens: None,
                                    prompt_cache_miss_tokens: None,
                                },
                                stream_tx,
                            );
                            if session.summary_up_to > before_summary_up_to {
                                continue 'react_loop;
                            }
                        }
                        let _ = stream_tx.send(StreamEvent::Error {
                            message: err_msg.clone(),
                        });
                        // 持久化错误到 session，避免前端 Error 事件时序丢失导致中断无痕迹。
                        persist_error(session, format!("ReAct 循环请求失败：{err_msg}"));
                        return accumulated_usage;
                    }
                };

                accumulated_usage.accumulate(&response.usage);
                emit_token_usage(
                    stream_tx,
                    &response.usage,
                    Some(response.usage.prompt_tokens.max(session.current_tokens)),
                    self.engine.context_limit,
                    format!("react-round-{round}", round = round + 1),
                    None,
                );
                round += 1;

                if response.tool_calls.is_empty() {
                    if is_synthetic_tool_call_placeholder(&response.text) {
                        break 'react_loop;
                    }

                    // 工具执行阶段：LLM 未调用工具即视为该阶段结束。
                    // 将已流式输出的过程文本保存到 session（避免上下文丢失），
                    // 由外层总结阶段接管最终回复生成。
                    session.append_message_with_id(
                        pending_msg_id,
                        MessageRole::Assistant,
                        response.text.clone(),
                        response.reasoning_content.clone(),
                    );
                    if let Some(message) = session.messages.last_mut() {
                        message.reasoning_signature = response.reasoning_signature.clone();
                        // 过程性回复标记为 React 阶段：避免与总结阶段的最终回复混淆，
                        // 前端据此将其折叠为过程内容，只展示总结阶段的最终回复。
                        message.phase = crate::session::MessagePhase::React;
                    }
                    let output = LlmOutputRecord {
                        stage: format!("react-round-{round}"),
                        content: response.text.clone(),
                        reasoning_content: response.reasoning_content.clone(),
                        tool_calls: Vec::new(),
                        usage: response.usage.clone(),
                    };
                    append_runtime_tool_message_with_reasoning(
                        session,
                        "llm_output",
                        format_llm_output_message(&output),
                        response.reasoning_content.clone(),
                    );
                    session.persist_to_disk();
                    maybe_update_context_summary(session, &self.engine, &response.usage, stream_tx);

                    if user_message_injected_during_stream {
                        memory_recall_attempted = false;
                        successful_tool_call_keys.clear();
                        failed_tool_call_keys.clear();
                        failed_tool_names.clear();
                        memory_candidate_count = 0;
                        continue 'react_loop;
                    }

                    // 简单问答快速路径：首轮、本轮未执行工具、未注入新消息且 LLM 已给出
                    // 实质文本回复时，直接将该回复提升为最终回复（Summary），跳过总结阶段
                    // 的额外模型调用，满足"简单问答零额外开销"。
                    if outer_iteration == 0
                        && !executed_tool_in_iteration
                        && !response.text.trim().is_empty()
                    {
                        if let Some(message) = session.messages.last_mut() {
                            message.phase = crate::session::MessagePhase::Summary;
                        }
                        let _ = stream_tx.send(StreamEvent::Done {
                            usage: Some(accumulated_usage.clone()),
                        });
                        return accumulated_usage;
                    }

                    // 智能提升（工具场景）：本轮执行过工具、且 LLM 已给出一段「看起来像
                    // 完整回答」的实质文本（足够长、非提问、非 [NEED_MORE_WORK]）时，
                    // 直接将其提升为最终回复（Summary），跳过总结阶段。
                    // 动机：总结阶段是一次独立 LLM 调用，被提示词引导去"总结"，常把 ReAct
                    // 阶段已有的详实回答压缩成更精简、丢细节的版本。已有好答案时，再归纳
                    // 只会退化或冗余。详见 SUMMARY_PHASE_PROMPT 改进。
                    if executed_tool_in_iteration && looks_like_final_answer(&response.text) {
                        if let Some(message) = session.messages.last_mut() {
                            message.phase = crate::session::MessagePhase::Summary;
                        }
                        session.persist_to_disk();
                        let _ = stream_tx.send(StreamEvent::Done {
                            usage: Some(accumulated_usage.clone()),
                        });
                        return accumulated_usage;
                    }

                    break 'react_loop;
                }

                // 工具调用
                let executable_calls = response.tool_calls.iter().collect::<Vec<_>>();
                if executable_calls.is_empty() {
                    let msg = "模型没有返回可执行工具调用，任务已停止".to_string();
                    let _ = stream_tx.send(StreamEvent::Error {
                        message: msg.clone(),
                    });
                    persist_error(session, msg);
                    return accumulated_usage;
                }
                let tool_names: Vec<String> =
                    executable_calls.iter().map(|c| c.name.clone()).collect();
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
                    response.reasoning_signature.clone(),
                    &executable_calls,
                );

                // 执行工具
                let mut need_failure_recovery_prompt = false;
                for call in executable_calls {
                    // 本轮已尝试执行工具调用（无论成功/失败/跳过），标记以阻止
                    // 简单问答快速路径把后续无 tool_calls 的回复误判为直接回复。
                    executed_tool_in_iteration = true;
                    match drain_pending_commands_async(session, &self.engine, stream_tx, cmd_rx) {
                        PendingCommandEffect::Terminate => return accumulated_usage,
                        PendingCommandEffect::MessageInjected => {
                            memory_recall_attempted = false;
                            successful_tool_call_keys.clear();
                            failed_tool_call_keys.clear();
                            failed_tool_names.clear();
                            session.persist_to_disk();
                            continue 'react_loop;
                        }
                        PendingCommandEffect::None => {}
                    }

                    // 工具执行间隙：检测浏览器状态变化
                    maybe_inject_browser_update(
                        &self.engine,
                        session,
                        stream_tx,
                        &mut last_browser_snapshot,
                        &mut last_browser_check,
                        false,
                    )
                    .await;

                    if let Some(parse_error) = call
                        .arguments
                        .get("__parse_error")
                        .and_then(serde_json::Value::as_str)
                    {
                        let message = parse_error.to_string();
                        let failure = ToolFailureRecord::new(
                            &call.name,
                            &call.id,
                            format_call_args_summary(call),
                            ToolFailureKind::Argument,
                            message.clone(),
                        );
                        let provider_text = structured_tool_failure_provider_text(&failure);
                        let _ = stream_tx.send(StreamEvent::ToolResult {
                            name: call.name.clone(),
                            tool_call_id: Some(call.id.clone()),
                            ok: false,
                            output: message.clone(),
                            full_output: Some(message.clone()),
                            media: vec![],
                        });
                        append_tool_result_message(
                            session,
                            &call.id,
                            &call.name,
                            provider_text.clone(),
                            true,
                        );
                        append_runtime_tool_message(
                            session,
                            &call.name,
                            format!("工具参数无效 [{}]\n{provider_text}", call.name),
                        );
                        let tool_call_key = tool_call_dedupe_key(&call.name, &call.arguments);
                        failed_tool_call_keys.insert(tool_call_key, provider_text);
                        failed_tool_names.insert(call.name.clone());
                        need_failure_recovery_prompt = true;
                        continue;
                    }

                    // 团队协作工具拦截
                    if crate::agent_team::lifecycle::is_team_tool(&call.name) {
                        let args_summary = format_call_args_summary(call);
                        let _ = stream_tx.send(StreamEvent::ToolStart {
                            name: call.name.clone(),
                            args_summary: args_summary.clone(),
                        });
                        let result = if let Some(team) = self.team.as_ref() {
                            if let Ok(mut team) = team.lock() {
                                crate::agent_team::lifecycle::execute_team_tool(
                                    &mut team,
                                    &self.agent_id,
                                    call,
                                    session,
                                    &self.tools,
                                    stream_tx,
                                )
                            } else {
                                crate::agent_team::lifecycle::error_tool_result(
                                    &call.name,
                                    "团队状态锁定失败",
                                )
                            }
                        } else {
                            crate::agent_team::lifecycle::error_tool_result(
                                &call.name,
                                "团队功能未启用",
                            )
                        };
                        let _ = stream_tx.send(StreamEvent::ToolResult {
                            name: call.name.clone(),
                            tool_call_id: Some(call.id.clone()),
                            ok: result.ok,
                            output: tool_result_stream_output(&result),
                            full_output: Some(tool_result_full_output(&result)),
                            media: vec![],
                        });
                        append_tool_result_message(
                            session,
                            &call.id,
                            &call.name,
                            if result.ok {
                                tool_result_provider_text(&call.name, &result, false)
                            } else {
                                structured_tool_failure_provider_text(&ToolFailureRecord::new(
                                    &call.name,
                                    &call.id,
                                    args_summary.clone(),
                                    classify_tool_result_failure(&result),
                                    tool_result_full_output(&result),
                                ))
                            },
                            !result.ok,
                        );
                        if result.ok {
                            let tool_call_key = tool_call_dedupe_key(&call.name, &call.arguments);
                            successful_tool_call_keys.insert(tool_call_key);
                        } else {
                            let tool_call_key = tool_call_dedupe_key(&call.name, &call.arguments);
                            failed_tool_call_keys.insert(
                                tool_call_key,
                                structured_tool_failure_provider_text(&ToolFailureRecord::new(
                                    &call.name,
                                    &call.id,
                                    args_summary.clone(),
                                    classify_tool_result_failure(&result),
                                    tool_result_full_output(&result),
                                )),
                            );
                            failed_tool_names.insert(call.name.clone());
                            need_failure_recovery_prompt = true;
                        }
                        continue;
                    }

                    let args_summary = format_call_args_summary(call);
                    let (target_scope, target_summary) = infer_audit_target(call);
                    let normalized_target = normalize_permission_target(
                        session,
                        target_scope.as_deref(),
                        target_summary.as_deref(),
                    );
                    let tool_call_key = tool_call_dedupe_key(&call.name, &call.arguments);
                    if successful_tool_call_keys.contains(&tool_call_key) {
                        append_duplicate_tool_result(session, stream_tx, &call.id, &call.name);
                        continue;
                    }
                    if let Some(original_error) = failed_tool_call_keys.get(&tool_call_key).cloned()
                    {
                        let repeated_failure = ToolFailureRecord::repeated(
                            &call.name,
                            &call.id,
                            args_summary.clone(),
                            original_error,
                        );
                        append_repeated_failed_tool_result(
                            session,
                            stream_tx,
                            &call.id,
                            &call.name,
                            &structured_tool_failure_provider_text(&repeated_failure),
                        );
                        failed_tool_names.insert(call.name.clone());
                        need_failure_recovery_prompt = true;
                        continue;
                    }

                    let decision = evaluate_tool_permission(
                        &self.engine,
                        &call.name,
                        target_scope.as_deref(),
                        normalized_target.as_deref(),
                    );
                    let trust_mode = format!("{:?}", self.engine.permission_gate().trust_mode());
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
                                media: vec![],
                            });
                            append_tool_result_message(
                                session,
                                &call.id,
                                &call.name,
                                structured_tool_failure_provider_text(&ToolFailureRecord::new(
                                    &call.name,
                                    &call.id,
                                    args_summary.clone(),
                                    ToolFailureKind::PermissionDenied,
                                    format!("权限拒绝：{reason}"),
                                )),
                                true,
                            );
                            failed_tool_call_keys.insert(
                                tool_call_key,
                                structured_tool_failure_provider_text(&ToolFailureRecord::new(
                                    &call.name,
                                    &call.id,
                                    args_summary.clone(),
                                    ToolFailureKind::PermissionDenied,
                                    format!("权限拒绝：{reason}"),
                                )),
                            );
                            failed_tool_names.insert(call.name.clone());
                            need_failure_recovery_prompt = true;
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
                                        media,
                                    }) => {
                                        let mid = append_or_reuse_user_message(
                                            session, &content, message_id, media,
                                        );
                                        let msg_media = session
                                            .messages
                                            .iter()
                                            .find(|message| message.id == mid)
                                            .map(|message| message.media.clone())
                                            .unwrap_or_default();
                                        let _ = stream_tx.send(StreamEvent::UserMessage {
                                            message_id: mid,
                                            content: content.clone(),
                                            media: msg_media,
                                        });
                                        memory_candidate_count = 0;
                                    }
                                    Some(Command::UpdateCwd { cwd }) => {
                                        session.cwd = cwd;
                                        crate::core::apply_session_cwd(session);
                                    }
                                    Some(Command::ReloadConfig) => {}
                                    Some(Command::Approval { .. }) => {}
                                    Some(Command::CancelAgent { .. }) => {}
                                    Some(Command::InjectTool { tool_name, payload }) => {
                                        crate::react::message::inject_tool_to_session(
                                            session, stream_tx, &tool_name, &payload,
                                        );
                                    }
                                    Some(Command::CompressContext) => {
                                        crate::core::compress_context_for_session(
                                            session,
                                            &self.engine,
                                            stream_tx,
                                        );
                                    }
                                    Some(Command::ResetContext) => {
                                        crate::core::reset_context_for_session(
                                            session,
                                            stream_tx,
                                            &self.engine,
                                        );
                                    }
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
                                    structured_tool_failure_provider_text(&ToolFailureRecord::new(
                                        &call.name,
                                        &call.id,
                                        args_summary.clone(),
                                        ToolFailureKind::UserRejected,
                                        "用户拒绝执行",
                                    )),
                                    true,
                                );
                                session.persist_to_disk();
                                let _ = stream_tx.send(StreamEvent::ToolResult {
                                    name: call.name.clone(),
                                    tool_call_id: Some(call.id.clone()),
                                    ok: false,
                                    output: "用户拒绝执行".to_string(),
                                    full_output: None,
                                    media: vec![],
                                });
                                let _ = stream_tx.send(StreamEvent::Done {
                                    usage: Some(accumulated_usage.clone()),
                                });
                                return accumulated_usage;
                            }
                        }
                    }

                    if check_cancel(session, &self.engine, cmd_rx, stream_tx) {
                        return accumulated_usage;
                    }

                    // 文件编辑锁检查
                    if matches!(call.name.as_str(), "write_file" | "replace_in_file")
                        && let Some(team) = self.team.as_ref()
                    {
                        let file_path = call
                            .arguments
                            .as_object()
                            .and_then(|o| o.get("path").and_then(|v| v.as_str()).map(String::from));
                        if let Some(ref path) = file_path {
                            let path_buf = std::path::PathBuf::from(path);
                            let now = chrono::Local::now().naive_local();
                            let lock_error = team
                                .lock()
                                .map_err(|_| "团队状态锁定失败".to_string())
                                .and_then(|mut team| {
                                    team.file_locks.ensure_can_write(
                                        &path_buf,
                                        &self.agent_id,
                                        &now,
                                    )
                                });
                            if let Err(message) = lock_error {
                                let failure =
                                    structured_tool_failure_provider_text(&ToolFailureRecord::new(
                                        &call.name,
                                        &call.id,
                                        args_summary.clone(),
                                        ToolFailureKind::PermissionDenied,
                                        message.clone(),
                                    ));
                                let _ = stream_tx.send(StreamEvent::ToolResult {
                                    name: call.name.clone(),
                                    tool_call_id: Some(call.id.clone()),
                                    ok: false,
                                    output: message.clone(),
                                    full_output: None,
                                    media: vec![],
                                });
                                append_tool_result_message(
                                    session,
                                    &call.id,
                                    &call.name,
                                    failure.clone(),
                                    true,
                                );
                                failed_tool_call_keys.insert(tool_call_key, failure);
                                failed_tool_names.insert(call.name.clone());
                                need_failure_recovery_prompt = true;
                                continue;
                            }
                        }
                    }

                    let _ = stream_tx.send(StreamEvent::ToolStart {
                        name: call.name.clone(),
                        args_summary: args_summary.clone(),
                    });

                    let (result, tool_llm_usage, allow_memory_context, usage_source) = {
                        let (mut result, tool_llm_usage, allow_memory_context, usage_source) =
                            if call.name == "index_search" {
                                if let Some(ref im) = index_manager {
                                    let (result, usage, allow_context) =
                                        crate::index::execute_index_search_tool(call, im, session);
                                    (result, usage, allow_context, "index_search")
                                } else {
                                    (
                                        crate::tool::ToolResult {
                                            ok: false,
                                            summary: "索引系统未初始化".to_string(),
                                            stdout: String::new(),
                                            stderr: "index manager not available".to_string(),
                                            exit_code: 1,
                                            execution: None,
                                        },
                                        tiangong_types::TokenUsage::default(),
                                        false,
                                        "index_search",
                                    )
                                }
                            } else if call.name == "recall_memory" {
                                if memory_recall_attempted {
                                    let (result, usage, allow_context) =
                                        crate::core::duplicate_memory_recall_tool_result();
                                    (result, usage, allow_context, "recall_memory")
                                } else {
                                    memory_recall_attempted = true;
                                    let (result, usage, allow_context) =
                                        crate::core::execute_memory_recall_tool(
                                            call,
                                            memory_handle,
                                            session,
                                        )
                                        .await;
                                    (result, usage, allow_context, "recall_memory")
                                }
                            } else if call.name == "analyze_attachment" {
                                let (result, usage) = crate::core::execute_attachment_analysis_tool(
                                    call,
                                    &self.engine,
                                    session,
                                );
                                (result, usage, false, "analyze_attachment")
                            } else {
                                (
                                    self.engine
                                        .execute_tool_call(
                                            call,
                                            &self.mcp_targets,
                                            &self.engine.agent_config().mcp,
                                            &session.id,
                                        )
                                        .await,
                                    tiangong_types::TokenUsage::default(),
                                    false,
                                    "",
                                )
                            };
                        crate::memory::turn_result::localize_tool_result_images(
                            &call.name,
                            &mut result,
                        );
                        (result, tool_llm_usage, allow_memory_context, usage_source)
                    };
                    accumulated_usage.accumulate(&tool_llm_usage);
                    emit_token_usage(
                        stream_tx,
                        &tool_llm_usage,
                        None,
                        self.engine.context_limit,
                        usage_source,
                        None,
                    );

                    audit_tool_execution(
                        &session.id,
                        &call.name,
                        result.ok,
                        (!args_summary.is_empty()).then_some(args_summary.as_str()),
                        target_scope.as_deref(),
                        normalized_target.as_deref().or(target_summary.as_deref()),
                        &result.summary,
                    );
                    let tool_media = if result.ok {
                        crate::memory::turn_result::parse_media_assets_from_tool_result(
                            &call.name,
                            &result.stdout,
                            &result.summary,
                        )
                    } else {
                        Vec::new()
                    };
                    let _ = stream_tx.send(StreamEvent::ToolResult {
                        name: call.name.clone(),
                        tool_call_id: Some(call.id.clone()),
                        ok: result.ok,
                        output: tool_result_stream_output(&result),
                        full_output: Some(tool_result_full_output(&result)),
                        media: tool_media.clone(),
                    });
                    // 媒体工具成功时，立即创建一条携带媒体的 assistant 消息，前端可实时渲染
                    if !tool_media.is_empty() {
                        session.append_message_with_media(
                            MessageRole::Assistant,
                            String::new(),
                            tool_media.clone(),
                        );
                    }
                    append_tool_result_message(
                        session,
                        &call.id,
                        &call.name,
                        if result.ok {
                            tool_result_provider_text(&call.name, &result, allow_memory_context)
                        } else {
                            structured_tool_failure_provider_text(&ToolFailureRecord::new(
                                &call.name,
                                &call.id,
                                args_summary.clone(),
                                classify_tool_result_failure(&result),
                                tool_result_full_output(&result),
                            ))
                        },
                        !result.ok,
                    );
                    append_runtime_tool_message(
                        session,
                        &call.name,
                        format_tool_trace_message(&result),
                    );
                    if !result.ok && check_cancel(session, &self.engine, cmd_rx, stream_tx) {
                        return accumulated_usage;
                    }

                    if result.ok {
                        failed_tool_call_keys.remove(&tool_call_key);
                        failed_tool_names.remove(&call.name);
                        successful_tool_call_keys.insert(tool_call_key);
                    } else {
                        let error_summary =
                            structured_tool_failure_provider_text(&ToolFailureRecord::new(
                                &call.name,
                                &call.id,
                                args_summary.clone(),
                                classify_tool_result_failure(&result),
                                tool_result_full_output(&result),
                            ));
                        failed_tool_call_keys.insert(tool_call_key, error_summary);
                        failed_tool_names.insert(call.name.clone());
                        need_failure_recovery_prompt = true;
                    }

                    // 记忆候选评估
                    if check_cancel(session, &self.engine, cmd_rx, stream_tx) {
                        return accumulated_usage;
                    }
                    if let Some(handle) = memory_handle {
                        let file_path =
                            if matches!(call.name.as_str(), "write_file" | "replace_in_file") {
                                call.arguments.as_object().and_then(|o| {
                                    o.get("path").and_then(|v| v.as_str()).map(String::from)
                                })
                            } else {
                                None
                            };
                        if let Some(candidate) =
                            crate::memory::turn_result::evaluate_tool_result_for_memory(
                                &call.name,
                                result.ok,
                                &result.summary,
                                file_path.as_deref(),
                                memory_candidate_count,
                            )
                        {
                            handle.submit_memory_candidate(candidate);
                            memory_candidate_count += 1;
                        }
                    }
                    maybe_update_context_summary(session, &self.engine, &response.usage, stream_tx);

                    match drain_pending_commands_async(session, &self.engine, stream_tx, cmd_rx) {
                        PendingCommandEffect::Terminate => return accumulated_usage,
                        PendingCommandEffect::MessageInjected => {
                            memory_recall_attempted = false;
                            successful_tool_call_keys.clear();
                            failed_tool_call_keys.clear();
                            failed_tool_names.clear();
                            memory_candidate_count = 0;
                            session.persist_to_disk();
                            continue 'react_loop;
                        }
                        PendingCommandEffect::None => {}
                    }

                    // 工具执行后：检测浏览器状态变化
                    maybe_inject_browser_update(
                        &self.engine,
                        session,
                        stream_tx,
                        &mut last_browser_snapshot,
                        &mut last_browser_check,
                        true,
                    )
                    .await;
                }

                if need_failure_recovery_prompt {
                    let mut failed_tools = failed_tool_names.iter().cloned().collect::<Vec<_>>();
                    failed_tools.sort();
                    let collaboration_hint = if self.agent_id == "main"
                        && request_tools.iter().any(|tool| tool.name == "create_agent")
                    {
                        "如果问题适合并行排查或需要第二视角，请创建 temporary Sub Agent 协作处理，并把失败工具、失败原因、已尝试方案和用户目标一并分配给它。"
                    } else {
                        "如果当前 Agent 无法继续独立推进，请向用户说明需要的外部条件、凭据、授权、环境调整或人工确认。"
                    };
                    let recall_hint = if request_tools
                        .iter()
                        .any(|tool| tool.name == "recall_memory")
                    {
                        memory_recall_attempted = false;
                        "优先调用 recall_memory，充分使用 Memory 系统查询这个工具以前成功调用时使用的参数、环境前置条件、配置方式、替代步骤和相关经验；只有回忆不足以解决时，再切换工具、创建子 Agent 或请求用户协作。"
                    } else {
                        ""
                    };
                    let mut reminder = Message::new(
                        MessageRole::Tool,
                        format!(
                            "<system-reminder>\n以下工具调用在本轮出现失败，暂时不要重复调用相同工具和相同参数：\n{}\n请重新规划：{}{}\n</system-reminder>",
                            failed_tools.join("\n"),
                            recall_hint,
                            collaboration_hint
                        ),
                    );
                    reminder.tool_name = Some("react_failed_tool_recovery".to_string());
                    session.messages.push(reminder);
                    session.persist_to_disk();
                    continue 'react_loop;
                }

                // 执行有待处理任务的 Sub Agent
                let sub_result = self
                    .drain_sub_agent_inboxes(
                        session,
                        stream_tx,
                        cmd_rx,
                        memory_handle,
                        &index_manager,
                    )
                    .await;
                accumulated_usage.accumulate(&sub_result.usage);
                if sub_result.cancelled {
                    session.persist_to_disk();
                    return accumulated_usage;
                }

                let main_messages = self.drain_main_agent_messages();
                if !main_messages.is_empty() {
                    for message in main_messages {
                        session.messages.push(Message::new(
                            MessageRole::User,
                            format!(
                                "[from:{} at {}]\n{}",
                                message.from, message.created_at, message.content
                            ),
                        ));
                    }
                    continue 'react_loop;
                }

                if sub_result.ran {
                    session.persist_to_disk();
                    let _ = stream_tx.send(StreamEvent::Done {
                        usage: Some(accumulated_usage.clone()),
                    });
                    return accumulated_usage;
                }

                session.persist_to_disk();
            }

            // ── 总结阶段 ──
            // 内层工具执行阶段结束后，由主模型独立判断任务完成度并输出最终回复。
            let summary_result = self
                .run_summary_phase(session, stream_tx, cmd_rx, outer_iteration + 1)
                .await;
            match summary_result {
                SummaryPhaseResult::Completed(usage) => {
                    accumulated_usage.accumulate(&usage);
                    let _ = stream_tx.send(StreamEvent::Done {
                        usage: Some(accumulated_usage.clone()),
                    });
                    return accumulated_usage;
                }
                SummaryPhaseResult::NeedMoreWork { reason, usage } => {
                    accumulated_usage.accumulate(&usage);
                    outer_iteration += 1;
                    if outer_iteration >= self.max_outer_iterations {
                        // 重入次数已达上限，强制输出最终回复。
                        force_final_response(
                            session,
                            &self.engine,
                            stream_tx,
                            ForceFinalReason::OuterLimit,
                        );
                        return accumulated_usage;
                    }
                    // 注入"上轮总结判定未完成"的上下文，重新进入工具执行阶段。
                    // 必须用合法的 tool injection pair（assistant tool_call + tool result），
                    // 而非孤立的 Tool 消息——后者破坏 OpenAI/DeepSeek 消息协议，
                    // 也会因非 append-only 的协议异常破坏 KV cache 前缀命中。
                    crate::react::message::inject_tool_to_messages(
                        session,
                        "summary_need_more_work",
                        &serde_json::json!({
                            "reason": reason.trim(),
                            "instruction": "上轮总结判定任务未完成，请根据原因继续执行剩余工作。",
                        }),
                    );
                    session.persist_to_disk();
                    memory_recall_attempted = false;
                    successful_tool_call_keys.clear();
                    failed_tool_call_keys.clear();
                    failed_tool_names.clear();
                    memory_candidate_count = 0;
                    continue 'outer;
                }
                SummaryPhaseResult::Cancelled(usage) => {
                    accumulated_usage.accumulate(&usage);
                    session.persist_to_disk();
                    return accumulated_usage;
                }
                SummaryPhaseResult::Failed { message, usage } => {
                    accumulated_usage.accumulate(&usage);
                    let _ = stream_tx.send(StreamEvent::Error {
                        message: format!("总结阶段失败：{message}"),
                    });
                    persist_error(session, format!("总结阶段失败：{message}"));
                    force_final_response(
                        session,
                        &self.engine,
                        stream_tx,
                        ForceFinalReason::SummaryError,
                    );
                    return accumulated_usage;
                }
            }
        }

        accumulated_usage
    }

    fn drain_main_agent_messages(&self) -> Vec<crate::agent_team::message_bus::AgentMessage> {
        self.team
            .as_ref()
            .and_then(|team| team.lock().ok().map(|mut team| team.drain_main_inbox()))
            .unwrap_or_default()
    }
}

/// 解析总结阶段回复，判断是否需要重入工具执行阶段。
///
/// - 首行 `[NEED_MORE_WORK]` → 返回 `(true, 去标记后的正文)`
/// - 首行 `[DONE]` / `[ASK_USER]` → 返回 `(false, 去标记后的正文)`
/// - 无标记 → 视为完成，返回 `(false, 原文)`
fn parse_summary_need_more_work(text: &str) -> (bool, String) {
    let trimmed = text.trim();
    if let Some(rest) = strip_summary_marker(trimmed, "[NEED_MORE_WORK]") {
        return (true, rest.trim().to_string());
    }
    if let Some(rest) = strip_summary_marker(trimmed, "[DONE]") {
        return (false, rest.trim().to_string());
    }
    if let Some(rest) = strip_summary_marker(trimmed, "[ASK_USER]") {
        return (false, rest.trim().to_string());
    }
    (false, trimmed.to_string())
}

fn strip_summary_marker<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
    let text = text.trim_start();
    let prefix = text.get(..marker.len())?;
    if !prefix.eq_ignore_ascii_case(marker) {
        return None;
    }
    Some(
        text[marker.len()..]
            .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, ':' | '：' | '-')),
    )
}

/// ReAct 阶段给出一段实质性回答时，可跳过总结阶段直接作为最终回复。
const FINAL_ANSWER_MIN_CHARS: usize = 200;

/// 判断 ReAct 阶段的文本回复是否「看起来像一个完整回答」（而非向用户提问）。
///
/// 用于智能提升：当本轮执行过工具、但模型已给出一段足够长的实质文本，
/// 且不像是反问用户（以问号结尾或显式请求输入）时，直接把它作为最终回复，
/// 避免总结阶段再生成一个更精简、反而丢失细节的版本。
fn looks_like_final_answer(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.chars().count() < FINAL_ANSWER_MIN_CHARS {
        return false;
    }
    // 以问号结尾 → 多为向用户提问，不作为最终回答。
    if trimmed.ends_with('?') || trimmed.ends_with('？') {
        return false;
    }
    // 显式请求用户提供信息：仅在文本以这些短语开头时才排除。
    // （较长文本里偶尔出现这些词通常是正常叙述，不应误判。）
    let intro = trimmed.chars().take(16).collect::<String>();
    let ask_intro = [
        "请问",
        "请提供",
        "请确认",
        "请选择",
        "你想",
        "你希望",
        "你是否",
    ];
    if ask_intro.iter().any(|p| intro.starts_with(p)) {
        return false;
    }
    true
}

fn format_team_roster(team_arc: &Arc<Mutex<TeamContext>>) -> String {
    let Ok(team) = team_arc.lock() else {
        return String::new();
    };
    let mut agents = team.registry.alive_agents();
    agents.sort_by(|a, b| a.role.cmp(&b.role));
    agents
        .iter()
        .map(|a| format!("- {} (@{})", a.label, a.role))
        .collect::<Vec<_>>()
        .join("\n")
}

fn tools_for_current_turn(
    tools: &[ToolSpec],
    session: &Session,
    user_input: &str,
) -> Vec<ToolSpec> {
    let intent_text = if user_input.trim().is_empty() {
        session
            .messages
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::User)
            .map(|message| message.text_content())
            .unwrap_or_default()
    } else {
        user_input.to_string()
    };
    filter_background_task_tools(tools.to_vec(), &intent_text)
}

impl ReactEngine {
    #[allow(clippy::too_many_arguments)]
    fn spawn_ready_sub_agents(
        &self,
        team_arc: &Arc<Mutex<TeamContext>>,
        stream_tx: &StdSender<StreamEvent>,
        futures: &mut FuturesUnordered<SubAgentFuture>,
        active_sub_agents: &mut Vec<ActiveSubAgent>,
        used_tokens: usize,
        max_concurrent: usize,
        token_budget: usize,
        sub_max_rounds: usize,
        memory_handle: Option<&tiangong_memory::MemoryHandle>,
        index_manager: &Option<std::sync::Arc<crate::index::IndexManager>>,
    ) -> bool {
        let remaining_slots = max_concurrent.saturating_sub(active_sub_agents.len());
        if remaining_slots == 0 {
            return false;
        }
        if used_tokens >= token_budget {
            let _ = stream_tx.send(StreamEvent::AgentNotification {
                agent_id: "system".to_string(),
                agent_label: "系统".to_string(),
                content: format!(
                    "Sub Agent token 预算已用尽（{used_tokens}/{token_budget}），剩余 Agent 将在下一轮执行"
                ),
                level: "warning".to_string(),
            });
            return false;
        }

        type PendingAgent = (String, String, String, String, Vec<String>, String, Session);
        let mut pending: Vec<PendingAgent> = Vec::new();
        {
            let Ok(mut team) = team_arc.lock() else {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: "团队状态锁定失败".to_string(),
                });
                return false;
            };
            let agent_infos: Vec<_> = team
                .registry
                .alive_agents()
                .iter()
                .filter(|a| a.status == crate::agent_team::descriptor::AgentStatus::Idle)
                .map(|a| {
                    (
                        a.agent_id.clone(),
                        a.label.clone(),
                        a.role.clone(),
                        a.system_prompt.clone(),
                        a.tools.clone(),
                    )
                })
                .collect();

            for (agent_id, agent_label, agent_role, system_prompt, tool_names) in agent_infos {
                if pending.len() >= remaining_slots {
                    break;
                }
                let messages = team.registry.drain_inbox(&agent_id);
                if messages.is_empty() {
                    continue;
                }
                let Some(child_session) = team.registry.get_session(&agent_id).cloned() else {
                    continue;
                };
                team.registry.update_status(
                    &agent_id,
                    crate::agent_team::descriptor::AgentStatus::Running,
                );
                let _ = stream_tx.send(StreamEvent::AgentStatusChanged {
                    agent_id: agent_id.clone(),
                    label: agent_label.clone(),
                    status: "running".to_string(),
                });
                let combined = messages
                    .into_iter()
                    .map(|m| format!("[from:{} at {}]\n{}", m.from, m.created_at, m.content))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                pending.push((
                    agent_id,
                    agent_label,
                    agent_role,
                    system_prompt,
                    tool_names,
                    combined,
                    child_session,
                ));
            }
        }

        if pending.is_empty() {
            return false;
        }

        for (
            agent_id,
            agent_label,
            agent_role,
            system_prompt,
            tool_names,
            combined_content,
            mut child_session,
        ) in pending
        {
            let sub_tools: Vec<ToolSpec> = self
                .tools
                .iter()
                .filter(|t| tool_names.iter().any(|name| name == &t.name))
                .filter(|t| !matches!(t.name.as_str(), "create_agent" | "dismiss_agent"))
                .cloned()
                .collect();

            let mut sub_engine = ReactEngine::new(
                self.engine.clone(),
                sub_tools,
                self.mcp_targets.clone(),
                sub_max_rounds,
                crate::agent_team::tools::SUB_AGENT_MAX_OUTER_ITERATIONS,
            )
            .with_shared_team(team_arc.clone(), agent_id.clone());

            // 通过 SubAgentPromptContext 构建 system prompt
            let base_config = crate::prompt::SystemPromptConfig::from_configs(
                self.engine.models_config(),
                self.engine.agent_config(),
                &child_session.id,
            );
            let team_roster = format_team_roster(team_arc);
            let ctx = crate::prompt::SubAgentPromptContext::new(
                &base_config,
                &system_prompt,
                &team_roster,
            );
            child_session.system_prompt_message = Some(ctx.build(&child_session));

            // 条件性赋予能力：根据 tools 列表决定是否共享 index_manager 和 memory_handle
            let has_index = tool_names.iter().any(|n| n == "index_search");
            let has_memory = tool_names.iter().any(|n| n == "recall_memory");
            let sub_index = if has_index {
                index_manager.clone()
            } else {
                None
            };
            let sub_memory = if has_memory {
                memory_handle.cloned()
            } else {
                None
            };

            let (sub_cmd_tx, mut sub_cmd_rx) = tokio_mpsc::unbounded_channel();
            let _ = sub_cmd_tx.send(Command::Message {
                content: combined_content,
                message_id: Some(scru128::new().to_string()),
                media: Vec::new(),
            });
            if let Ok(mut team) = team_arc.lock() {
                team.register_active_agent(agent_id.clone(), sub_cmd_tx.clone());
            }
            active_sub_agents.push((
                agent_id.clone(),
                agent_role.clone(),
                agent_label.clone(),
                sub_cmd_tx.clone(),
            ));

            let stream_tx_clone = stream_tx.clone();
            let id = agent_id;
            let label = agent_label;
            let role = agent_role;
            let start_message_len = child_session.messages.len();

            let fut = Box::pin(async move {
                let (child_stream_tx, child_stream_rx) = std::sync::mpsc::channel();
                let forwarder = spawn_sub_agent_stream_forwarder(
                    id.clone(),
                    role.clone(),
                    label.clone(),
                    stream_tx_clone,
                    child_stream_rx,
                );
                let _keep_sub_cmd_tx_alive = sub_cmd_tx;
                let usage = sub_engine
                    .execute_turn(
                        &mut child_session,
                        "",
                        &child_stream_tx,
                        &mut sub_cmd_rx,
                        sub_memory.as_ref(),
                        sub_index,
                    )
                    .await;
                drop(child_stream_tx);
                let _ = forwarder.join();
                let new_messages = child_session
                    .messages
                    .iter()
                    .skip(start_message_len)
                    .cloned()
                    .collect::<Vec<_>>();
                (id, label, role, child_session, new_messages, usage)
            });
            futures.push(fut);
        }

        true
    }

    /// 轮询所有活跃 Sub Agent 的收件箱，为有待处理消息的 Agent 执行 ReactEngine。
    /// 返回 Sub Agent 执行消耗的 token 总量。
    async fn drain_sub_agent_inboxes(
        &mut self,
        parent_session: &mut Session,
        stream_tx: &StdSender<StreamEvent>,
        cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
        memory_handle: Option<&tiangong_memory::MemoryHandle>,
        index_manager: &Option<std::sync::Arc<crate::index::IndexManager>>,
    ) -> SubAgentDrainResult {
        let mut result = SubAgentDrainResult::default();

        let Some(team_arc) = self.team.clone() else {
            return result;
        };

        let max_concurrent = crate::agent_team::tools::MAX_CONCURRENT_SUB_AGENTS;
        let token_budget = crate::agent_team::tools::SUB_AGENT_TOTAL_TOKEN_BUDGET;
        let sub_max_rounds = crate::agent_team::tools::SUB_AGENT_MAX_TOOL_ROUNDS;

        let (dispatch_wake_tx, mut dispatch_wake_rx) = tokio_mpsc::unbounded_channel();
        {
            let Ok(mut team) = team_arc.lock() else {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: "团队状态锁定失败".to_string(),
                });
                return result;
            };
            team.set_dispatch_waker(dispatch_wake_tx);
        }

        // 并行执行所有待执行的 Sub Agent（协作并发）
        let mut futures: FuturesUnordered<SubAgentFuture> = FuturesUnordered::new();
        let mut active_sub_agents: Vec<ActiveSubAgent> = Vec::new();
        if self.spawn_ready_sub_agents(
            &team_arc,
            stream_tx,
            &mut futures,
            &mut active_sub_agents,
            result.usage.total_tokens,
            max_concurrent,
            token_budget,
            sub_max_rounds,
            memory_handle,
            index_manager,
        ) {
            result.ran = true;
        }

        if futures.is_empty() {
            if let Ok(mut team) = team_arc.lock() {
                team.clear_dispatch_waker();
            }
            return result;
        }

        let mut results = Vec::new();
        while !futures.is_empty() {
            tokio::select! {
                maybe_result = futures.next() => {
                    if let Some(sub_result) = maybe_result {
                        active_sub_agents.retain(|(agent_id, _, _, _)| agent_id != &sub_result.0);
                        if let Ok(mut team) = team_arc.lock() {
                            team.unregister_active_agent(&sub_result.0);
                        }
                        results.push(sub_result);
                        if self.spawn_ready_sub_agents(
                            &team_arc,
                            stream_tx,
                            &mut futures,
                            &mut active_sub_agents,
                            result.usage.total_tokens,
                            max_concurrent,
                            token_budget,
                            sub_max_rounds,
                            memory_handle,
                            index_manager,
                        ) {
                            result.ran = true;
                        }
                    }
                }
                maybe_wake = dispatch_wake_rx.recv() => {
                    if maybe_wake.is_some()
                        && self.spawn_ready_sub_agents(
                            &team_arc,
                            stream_tx,
                            &mut futures,
                            &mut active_sub_agents,
                            result.usage.total_tokens,
                            max_concurrent,
                            token_budget,
                            sub_max_rounds,
                            memory_handle,
                            index_manager,
                        )
                    {
                        result.ran = true;
                    }
                }
                maybe_cmd = cmd_rx.recv() => {
                    match maybe_cmd {
                        Some(Command::Cancel) | Some(Command::Shutdown) => {
                            result.cancelled = true;
                            let _ = stream_tx.send(StreamEvent::Error {
                                message: "已取消所有 Agent".to_string(),
                            });
                            for (_, _, _, tx) in &active_sub_agents {
                                let _ = tx.send(Command::Cancel);
                            }
                        }
                        Some(Command::CancelAgent { role }) => {
                            let mut matched = false;
                            for (agent_id, agent_role, agent_label, tx) in &active_sub_agents {
                                if agent_role == &role {
                                    matched = true;
                                    let _ = tx.send(Command::Cancel);
                                    let _ = stream_tx.send(StreamEvent::AgentStatusChanged {
                                        agent_id: agent_id.clone(),
                                        label: agent_label.clone(),
                                        status: "idle".to_string(),
                                    });
                                    let _ = stream_tx.send(StreamEvent::AgentNotification {
                                        agent_id: agent_id.clone(),
                                        agent_label: agent_label.clone(),
                                        content: "已请求停止当前执行".to_string(),
                                        level: "warning".to_string(),
                                    });
                                }
                            }
                            if !matched {
                                let _ = stream_tx.send(StreamEvent::AgentNotification {
                                    agent_id: role.clone(),
                                    agent_label: role,
                                    content: "未找到正在执行的 Agent".to_string(),
                                    level: "warning".to_string(),
                                });
                            }
                        }
                        Some(Command::Message { content, message_id, media }) => {
                            let message_id = append_or_reuse_user_message(
                                parent_session,
                                &content,
                                message_id,
                                media,
                            );
                            let media = parent_session
                                .messages
                                .iter()
                                .find(|message| message.id == message_id)
                                .map(|message| message.media.clone())
                                .unwrap_or_default();
                            let _ = stream_tx.send(StreamEvent::UserMessage {
                                message_id,
                                content: content.clone(),
                                media: media.clone(),
                            });
                            if let Ok(mut team) = team_arc.lock() {
                                let _ = crate::agent_team::lifecycle::route_user_mentions_with_media(
                                    &mut team,
                                    &content,
                                    media,
                                    stream_tx,
                                );
                            }
                        }
                        Some(Command::UpdateCwd { cwd }) => {
                            parent_session.cwd = cwd;
                            crate::core::apply_session_cwd(parent_session);
                        }
                        Some(Command::CompressContext) => {
                            crate::core::compress_context_for_session(
                                parent_session,
                                &self.engine,
                                stream_tx,
                            );
                        }
                        Some(Command::ResetContext) => {
                            crate::core::reset_context_for_session(parent_session, stream_tx, &self.engine);
                        }
                        Some(Command::ReloadConfig) | Some(Command::Approval { .. }) => {}
                        Some(Command::InjectTool { tool_name, payload }) => {
                            crate::react::message::inject_tool_to_session(
                                parent_session,
                                stream_tx,
                                &tool_name,
                                &payload,
                            );
                        }
                        None => break,
                    }
                }
            }
        }

        if let Ok(mut team) = team_arc.lock() {
            team.clear_dispatch_waker();
        }

        // 处理结果
        for (agent_id, agent_label, _agent_role, child_session, _output_messages, usage) in results
        {
            // 检查 Sub Agent 是否有错误输出。
            // 错误由 persist_error 经 inject_tool_to_messages 注入（plugin_injection
            // 消息对，渲染文本含 "数据来源：react_loop_error"）。
            let is_error_message = |m: &Message| {
                m.tool_name.as_deref() == Some(crate::react::message::INJECTION_TOOL_NAME)
                    && m.text_content().contains("数据来源：react_loop_error")
            };
            let has_error = child_session.messages.iter().any(is_error_message);

            let completion_content = if has_error {
                let error_content = child_session
                    .messages
                    .iter()
                    .filter(|m: &&Message| is_error_message(m))
                    .map(|m| m.text_content())
                    .collect::<Vec<_>>()
                    .join("; ");
                format!("[{agent_label}] 执行出错：{error_content}")
            } else {
                // 优先收集总结阶段（Summary phase）的最终回复作为汇报内容；
                // 若无（旧架构或快速路径未标记），回退到所有 Assistant 消息。
                let summary = {
                    let summaries: Vec<String> = child_session
                        .messages
                        .iter()
                        .filter(|m| {
                            m.role == MessageRole::Assistant
                                && m.phase == crate::session::MessagePhase::Summary
                        })
                        .filter_map(|m| {
                            let c = m.text_content().trim().to_string();
                            if c.is_empty() { None } else { Some(c) }
                        })
                        .collect();
                    if summaries.is_empty() {
                        child_session
                            .messages
                            .iter()
                            .filter(|m| m.role == MessageRole::Assistant)
                            .filter_map(|m| {
                                let c = m.text_content().trim().to_string();
                                if c.is_empty() { None } else { Some(c) }
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    } else {
                        summaries.join("\n")
                    }
                };

                if summary.is_empty() {
                    format!("[{agent_label}] 已完成本轮工作，但没有生成文本输出。")
                } else {
                    let brief = if summary.chars().count() > 500 {
                        format!("{}...", summary.chars().take(500).collect::<String>())
                    } else {
                        summary
                    };
                    format!("[{agent_label}] 执行完成\n{brief}")
                }
            };

            result.usage.accumulate(&usage);

            // 临时 Agent 执行完毕后自动销毁
            let Ok(mut team) = team_arc.lock() else {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: "团队状态锁定失败".to_string(),
                });
                continue;
            };
            team.deliver_main_message(crate::agent_team::message_bus::AgentMessage {
                id: scru128::new().to_string(),
                from: agent_id.clone(),
                to: "main".to_string(),
                content: completion_content.clone(),
                priority: crate::agent_team::message_bus::MessagePriority::Normal,
                created_at: now_text(),
            });
            let _ = stream_tx.send(StreamEvent::AgentMessage {
                from_agent_id: agent_id.clone(),
                from_agent_label: agent_label.clone(),
                to_agent_id: "main".to_string(),
                to_agent_label: "Main Agent".to_string(),
                content: completion_content.clone(),
            });
            append_runtime_tool_message(
                parent_session,
                &format!("sub_agent_{agent_label}"),
                completion_content,
            );

            let status = if has_error { "error" } else { "idle" };
            let _ = stream_tx.send(StreamEvent::AgentStatusChanged {
                agent_id: agent_id.clone(),
                label: agent_label.clone(),
                status: status.to_string(),
            });
            team.registry.set_session(&agent_id, child_session);
            team.registry
                .update_status(&agent_id, crate::agent_team::descriptor::AgentStatus::Idle);

            // 持久化 child_session 到磁盘
            if let Some(child) = team.registry.get_session(&agent_id) {
                crate::agent_team::lifecycle::persist_child_session(
                    parent_session,
                    &agent_id,
                    child,
                );
            }
        }

        result
    }
}

fn drain_pending_commands_async(
    session: &mut Session,
    engine: &crate::runtime::RuntimeEngine,
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
            Command::CancelAgent { .. } => {}
            Command::Shutdown => return PendingCommandEffect::Terminate,
            Command::Message {
                content,
                message_id,
                media,
            } => {
                let mid = append_or_reuse_user_message(session, &content, message_id, media);
                let msg_media = session
                    .messages
                    .iter()
                    .find(|message| message.id == mid)
                    .map(|message| message.media.clone())
                    .unwrap_or_default();
                let _ = stream_tx.send(StreamEvent::UserMessage {
                    message_id: mid,
                    content: content.clone(),
                    media: msg_media,
                });
                injected_message = true;
            }
            Command::UpdateCwd { cwd } => {
                session.cwd = cwd;
                crate::core::apply_session_cwd(session);
            }
            Command::ReloadConfig => {}
            Command::Approval { .. } => {}
            Command::InjectTool { tool_name, payload } => {
                crate::react::message::inject_tool_to_session(
                    session, stream_tx, &tool_name, &payload,
                );
                injected_message = true;
            }
            Command::CompressContext => {
                crate::core::compress_context_for_session(session, engine, stream_tx);
            }
            Command::ResetContext => {
                crate::core::reset_context_for_session(session, stream_tx, engine);
            }
        }
    }

    if injected_message {
        PendingCommandEffect::MessageInjected
    } else {
        PendingCommandEffect::None
    }
}

async fn maybe_inject_browser_update(
    engine: &crate::runtime::RuntimeEngine,
    session: &mut Session,
    stream_tx: &StdSender<StreamEvent>,
    last_snapshot: &mut Option<crate::browser_trait::PageSnapshot>,
    last_check: &mut Option<std::time::Instant>,
    force_check: bool,
) {
    let fetcher = match engine.page_fetcher() {
        Some(f) => f,
        None => {
            tracing::debug!(
                session_id = %session.id,
                force_check,
                "skip browser auto observe: no page fetcher"
            );
            return;
        }
    };
    // 首次检测无间隔限制；后续检测至少间隔 5 秒，避免频繁 observe_page 拖慢执行
    let now = std::time::Instant::now();
    if last_snapshot.is_some() && !force_check {
        let min_interval = std::time::Duration::from_secs(5);
        if let Some(prev) = last_check
            && now.duration_since(*prev) < min_interval
        {
            tracing::debug!(
                session_id = %session.id,
                elapsed_ms = now.duration_since(*prev).as_millis() as u64,
                "skip browser auto observe: throttled"
            );
            return;
        }
    }
    *last_check = Some(now);
    let snapshot =
        match tokio::time::timeout(std::time::Duration::from_secs(3), fetcher.observe_page()).await
        {
            Ok(Some(s)) => s,
            Ok(None) => {
                tracing::debug!(
                    session_id = %session.id,
                    "skip browser auto observe: no snapshot"
                );
                return;
            }
            Err(_) => {
                tracing::warn!(
                    session_id = %session.id,
                    "browser auto observe timeout"
                );
                return;
            }
        };
    if snapshot.url.is_empty() {
        tracing::debug!(
            session_id = %session.id,
            "skip browser auto observe: empty url"
        );
        return;
    }

    let feedback = crate::browser_trait::format_browser_events(&snapshot.events);
    let has_feedback = feedback.is_some();
    tracing::debug!(
        session_id = %session.id,
        url = %snapshot.url,
        title = %snapshot.title,
        text_len = snapshot.text.len(),
        events_len = snapshot.events.len(),
        has_feedback,
        force_check,
        "browser auto observe snapshot"
    );
    let should_inject = match last_snapshot {
        None => true,
        Some(prev) => {
            has_feedback
                || prev.url != snapshot.url
                || (snapshot.text.len() as i64 - prev.text.len() as i64).unsigned_abs() > 500
        }
    };
    if !should_inject {
        tracing::debug!(
            session_id = %session.id,
            url = %snapshot.url,
            text_len = snapshot.text.len(),
            "skip browser auto inject: unchanged"
        );
        *last_snapshot = Some(snapshot);
        return;
    }

    let tabs: Vec<(String, String, String)> = snapshot
        .tabs
        .iter()
        .map(|t| (t.id.clone(), t.url.clone(), t.title.clone()))
        .collect();
    crate::react::message::inject_tool_to_session(
        session,
        stream_tx,
        "browser_data",
        &serde_json::json!({
            "title": snapshot.title,
            "url": snapshot.url,
            "text": snapshot.text,
            "tabs": tabs,
            "active_tab_id": snapshot.active_tab_id,
            "feedback": feedback,
        }),
    );
    tracing::info!(
        session_id = %session.id,
        url = %snapshot.url,
        text_len = snapshot.text.len(),
        events_len = snapshot.events.len(),
        has_feedback,
        "browser auto content injected"
    );
    *last_snapshot = Some(snapshot);
}

/// 非阻塞检查是否有取消或关闭命令待处理。
fn check_cancel(
    session: &mut Session,
    engine: &crate::runtime::RuntimeEngine,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
    stream_tx: &StdSender<StreamEvent>,
) -> bool {
    while let Ok(cmd) = cmd_rx.try_recv() {
        match cmd {
            Command::Cancel | Command::Shutdown => {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: "已取消".into(),
                });
                return true;
            }
            Command::CancelAgent { .. } => {}
            Command::CompressContext => {
                crate::core::compress_context_for_session(session, engine, stream_tx);
            }
            Command::ResetContext => {
                crate::core::reset_context_for_session(session, stream_tx, engine);
            }
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.to_string(),
            description: String::new(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn tool_names(tools: Vec<ToolSpec>) -> Vec<String> {
        tools.into_iter().map(|tool| tool.name).collect()
    }

    #[test]
    fn looks_like_final_answer_short_text_is_not_final() {
        // 过短文本不视为完整回答（即便执行过工具）。
        assert!(!looks_like_final_answer("好的，已完成。"));
    }

    #[test]
    fn looks_like_final_answer_long_substantive_is_final() {
        // 一段足够长、不以问号结尾、不以提问短语开头的实质文本 → 视为完整回答。
        let text = "我重新检查了当前分支的全部改动，结论是核心问题已经修复。\
                    首先，AgentTurn 不再把整轮 elapsed_ms 当作深度思考耗时传给 ThinkingBlock，\
                    语义已经修正。其次，历史思考块固定 isActive 为 false，避免误计时与误展开。\
                    第三，showProcess 通过 useEffect 跟随 isActive 同步，完成后自动折叠过程。\
                    最后，summaryFrag 改为数组，多条非 react 助手回复不再互相覆盖。\
                    前端构建通过，整体改动合理，建议合并。";
        assert!(text.chars().count() >= FINAL_ANSWER_MIN_CHARS);
        assert!(looks_like_final_answer(text));
    }

    #[test]
    fn looks_like_final_answer_ending_with_question_is_not_final() {
        let mut text = "我重新检查了代码，发现了一些问题，但还需要你确认以下几点：".to_string();
        // 补足长度后仍以问号结尾 → 视为向用户提问，不作为最终回答。
        while text.chars().count() < FINAL_ANSWER_MIN_CHARS + 50 {
            text.push_str("补充说明内容。");
        }
        text.push('？');
        assert!(!looks_like_final_answer(&text));
    }

    #[test]
    fn looks_like_final_answer_intro_question_phrase_is_not_final() {
        let mut text = "请提供你的 API 凭据以便继续：".to_string();
        while text.chars().count() < FINAL_ANSWER_MIN_CHARS + 50 {
            text.push_str("这里需要更多信息。");
        }
        assert!(!looks_like_final_answer(&text));
    }

    #[test]
    fn current_turn_hides_background_task_tools_for_normal_commands() {
        let session = Session::new("test");
        let tools = vec![tool("run_shell"), tool("spawn_task"), tool("wait_tasks")];

        let names = tool_names(tools_for_current_turn(
            &tools,
            &session,
            "执行 git diff 看一下改动",
        ));

        assert_eq!(names, vec!["run_shell"]);
    }

    #[test]
    fn current_turn_uses_latest_user_message_when_input_is_empty() {
        let mut session = Session::new("test");
        session
            .messages
            .push(Message::new(MessageRole::User, "执行 git diff 看一下改动"));
        let tools = vec![tool("run_shell"), tool("spawn_task"), tool("wait_tasks")];

        let names = tool_names(tools_for_current_turn(&tools, &session, ""));

        assert_eq!(names, vec!["run_shell"]);
    }

    #[test]
    fn current_turn_keeps_background_task_tools_for_background_intent() {
        let mut session = Session::new("test");
        session.messages.push(Message::new(
            MessageRole::User,
            "后台启动 dev server，不要阻塞",
        ));
        let tools = vec![tool("run_shell"), tool("spawn_task"), tool("wait_tasks")];

        let names = tool_names(tools_for_current_turn(&tools, &session, ""));

        assert_eq!(names, vec!["run_shell", "spawn_task", "wait_tasks"]);
    }
}
