//! 单轮 Agent Loop 的执行过程。
//!
//! 本模块只负责从已构建的 TurnContext 执行模型请求、工具调用与总结阶段；
//! turn 的插件生命周期、状态提交和最终持久化由 react/turn.rs 负责。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use tokio::sync::mpsc as tokio_mpsc;
use tokio::task::{Id as TaskId, JoinSet};

use crate::context::compressor::{CompressionUpdate, ContextCompressor, mark_compact_boundary};
use crate::context::organizer::ContextOrganizer;
use crate::core::command::Command;
use crate::core::plugin::Plugin;
use crate::formatting::{format_llm_output_message, format_tool_trace_message};
use crate::model::{
    ModelFunctionResponse, ModelRequest, ModelStreamChunk, TokenUsage, ToolCall, ToolChoice,
    ToolSpec,
};
use crate::permission::TrustMode;
use crate::react::context::{
    build_thinking_config, emit_token_usage, observed_total_tokens, persist_error,
    rebuild_system_prompt, select_client_for_request,
};
use crate::react::message::*;
use crate::runtime::LlmOutputRecord;
use crate::session::{Message, MessagePhase, MessageRole, Session};
use crate::stream_throttle::{StreamTextKind, ThrottledStreamSink};
use crate::tool::ToolResult;
use crate::turn_context::TurnContext;
use tiangong_types::{
    DeferredToolInjection, StreamEvent, StreamToolCall, stream::ContextCompressAction,
};

use super::cancel::{abort_and_join, emit_cancel_usage};
use super::helpers::{looks_like_final_answer, record_plugin_usage};
use super::outcome::TurnExecutionResult;
use super::summary::{
    ForceFinalReason, SummaryDecision, build_force_final_request, commit_summary_message,
    parse_summary_phase_output, persist_partial_summary, promote_last_react_message_to_summary,
    request_for_summary_phase,
};
use super::tool_call::start_tool_call;

#[derive(Default)]
struct ToolCallHistory {
    successful_keys: HashSet<String>,
    failed_calls: HashMap<String, String>,
    failed_names: HashSet<String>,
}

impl ToolCallHistory {
    fn clear(&mut self) {
        self.successful_keys.clear();
        self.failed_calls.clear();
        self.failed_names.clear();
    }
}

struct ToolInjectionBuffer {
    session_deferred: Vec<DeferredToolInjection>,
    pending: VecDeque<DeferredToolInjection>,
    generation: u64,
}

impl ToolInjectionBuffer {
    fn new(ctx: &TurnContext) -> Self {
        Self {
            session_deferred: ctx.session.deferred_tool_injections.clone(),
            pending: VecDeque::new(),
            generation: 0,
        }
    }

    fn generation(&self) -> u64 {
        self.generation
    }

    /// 接收即向宿主发布待处理快照；真正写入 Session 仍等待当前子过程释放 ctx。
    fn receive(
        &mut self,
        stream_tx: &std::sync::mpsc::Sender<StreamEvent>,
        tool_name: String,
        payload: serde_json::Value,
    ) {
        self.pending
            .push_back(DeferredToolInjection { tool_name, payload });
        self.generation = self.generation.saturating_add(1);

        let mut injections = Vec::with_capacity(self.session_deferred.len() + self.pending.len());
        injections.extend(self.session_deferred.iter().cloned());
        injections.extend(self.pending.iter().cloned());
        let _ = stream_tx.send(StreamEvent::DeferredToolInjectionsChanged { injections });
    }

    /// 在工具协议安全时注入完整消息对；否则持久化到 Session 延迟队列等待下一边界。
    fn commit(&mut self, ctx: &mut TurnContext) {
        let received_new_injections = !self.pending.is_empty();
        while let Some(injection) = self.pending.pop_front() {
            ctx.session
                .defer_tool_injection(injection.tool_name, injection.payload);
        }
        if ctx.session.deferred_tool_injections.is_empty() {
            self.session_deferred.clear();
            return;
        }
        if ctx.session.has_unfinished_tool_calls() {
            if received_new_injections {
                ctx.session.persist_to_disk();
            }
            self.session_deferred = ctx.session.deferred_tool_injections.clone();
            return;
        }
        crate::react::message::flush_deferred_tool_injections(ctx);
        self.session_deferred = ctx.session.deferred_tool_injections.clone();
    }
}

/// 立即更新本轮权限判断和插件运行态；Session 在 turn 结束时统一接收最终值。
fn set_runtime_trust_mode(
    trust_mode: &mut TrustMode,
    plugins: &[Arc<dyn Plugin>],
    mode: TrustMode,
) {
    if *trust_mode == mode {
        return;
    }
    *trust_mode = mode;
    for plugin in plugins {
        plugin.set_trust_mode(mode);
    }
}

// TurnContext 定义与基础能力方法位于 `crate::turn_context`。本文件只实现单轮
// Agent Loop 的阶段编排与具体执行步骤。

// ===== turn 执行辅助 =====

fn record_tool_calls(
    ctx: &mut TurnContext,
    pending_msg_id: &str,
    response: &ModelFunctionResponse,
    round: usize,
) -> Vec<ToolCall> {
    let calls = response.tool_calls.clone();
    debug_assert!(!calls.is_empty(), "工具批次不能为空");

    let tool_names = calls
        .iter()
        .map(|call| call.name.clone())
        .collect::<Vec<_>>();
    let output = LlmOutputRecord {
        stage: format!("react-round-{round}"),
        content: response.text.clone(),
        reasoning_content: response.reasoning_content.clone(),
        tool_calls: tool_names.clone(),
        usage: response.usage.clone(),
    };
    append_runtime_tool_message_with_reasoning(
        &mut ctx.session,
        "llm_output",
        format_llm_output_message(&output),
        response.reasoning_content.clone(),
    );
    let _ = ctx.stream_tx.send(StreamEvent::ToolCalls {
        message_id: pending_msg_id.to_string(),
        names: tool_names,
        calls: calls
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
        &mut ctx.session,
        pending_msg_id.to_string(),
        &response.text,
        &response.reasoning_content,
        response.reasoning_signature.clone(),
        &calls.iter().collect::<Vec<_>>(),
    );
    emit_session_message_upsert(ctx, pending_msg_id);
    calls
}

enum ToolPreflightOutcome {
    Execute {
        args_summary: String,
        dedupe_key: String,
    },
    Skip {
        needs_recovery: bool,
    },
}

fn prepare_tool_call(
    ctx: &mut TurnContext,
    call: &ToolCall,
    history: &mut ToolCallHistory,
) -> ToolPreflightOutcome {
    if let Some(parse_error) = call
        .arguments
        .get("__parse_error")
        .and_then(serde_json::Value::as_str)
    {
        let message = parse_error.to_string();
        let failure = ToolFailureRecord::new(
            &call.name,
            &call.id,
            format_tool_args_summary(call),
            ToolFailureKind::Argument,
            message.clone(),
        );
        let provider_text = failure.render_for_model();
        let _ = ctx.stream_tx.send(StreamEvent::ToolResult {
            name: call.name.clone(),
            tool_call_id: Some(call.id.clone()),
            ok: false,
            output: message.clone(),
            full_output: Some(message),
            duration_ms: None,
        });
        append_tool_result_message(
            &mut ctx.session,
            &call.id,
            &call.name,
            provider_text.clone(),
            true,
        );
        append_runtime_tool_message(
            &mut ctx.session,
            &call.name,
            format!("工具参数无效 [{}]\n{provider_text}", call.name),
        );
        let dedupe_key = tool_call_dedupe_key(&call.name, &call.arguments);
        history.failed_calls.insert(dedupe_key, provider_text);
        history.failed_names.insert(call.name.clone());
        return ToolPreflightOutcome::Skip {
            needs_recovery: true,
        };
    }

    let args_summary = format_tool_args_summary(call);
    let dedupe_key = tool_call_dedupe_key(&call.name, &call.arguments);
    if history.successful_keys.contains(&dedupe_key) {
        append_duplicate_tool_result(ctx, &call.id, &call.name);
        return ToolPreflightOutcome::Skip {
            needs_recovery: false,
        };
    }
    if let Some(original_error) = history.failed_calls.get(&dedupe_key).cloned() {
        let repeated_failure =
            ToolFailureRecord::repeated(&call.name, &call.id, args_summary, original_error);
        append_repeated_failed_tool_result(
            ctx,
            &call.id,
            &call.name,
            &repeated_failure.render_for_model(),
        );
        history.failed_names.insert(call.name.clone());
        return ToolPreflightOutcome::Skip {
            needs_recovery: true,
        };
    }

    ToolPreflightOutcome::Execute {
        args_summary,
        dedupe_key,
    }
}

fn record_parallel_duplicate_tool_call(ctx: &mut TurnContext, call: &ToolCall) {
    let message = format!(
        "同一批次已经安排了完全相同的 {} 工具调用，本次重复调用已跳过；请直接使用同批调用返回的结果。",
        call.name
    );
    append_tool_result_message(
        &mut ctx.session,
        &call.id,
        &call.name,
        message.clone(),
        false,
    );
    append_runtime_tool_message(
        &mut ctx.session,
        &call.name,
        format!("跳过同批重复工具调用 [{}]\n{message}", call.name),
    );
    ctx.session.persist_to_disk();
    let _ = ctx.stream_tx.send(StreamEvent::ToolResult {
        name: call.name.clone(),
        tool_call_id: Some(call.id.clone()),
        ok: true,
        output: message.clone(),
        full_output: Some(message),
        duration_ms: None,
    });
}

fn record_rejected_tool_call(ctx: &mut TurnContext, call: &ToolCall, args_summary: &str) {
    ctx.observer.audit_tool_execution(
        &ctx.session.id,
        &call.name,
        false,
        (!args_summary.is_empty()).then_some(args_summary),
        "用户拒绝执行",
    );
    append_runtime_tool_message(
        &mut ctx.session,
        &call.name,
        format!("工具 {} 被用户拒绝执行", call.name),
    );
    append_tool_result_message(
        &mut ctx.session,
        &call.id,
        &call.name,
        ToolFailureRecord::new(
            &call.name,
            &call.id,
            args_summary.to_string(),
            ToolFailureKind::UserRejected,
            "用户拒绝执行",
        )
        .render_for_model(),
        true,
    );
    ctx.session.persist_to_disk();
    let _ = ctx.stream_tx.send(StreamEvent::ToolResult {
        name: call.name.clone(),
        tool_call_id: Some(call.id.clone()),
        ok: false,
        output: "用户拒绝执行".to_string(),
        full_output: None,
        duration_ms: None,
    });
}

struct CompletedToolCall<'a> {
    call: &'a ToolCall,
    args_summary: &'a str,
    dedupe_key: String,
    result: &'a ToolResult,
    duration_ms: u64,
}

fn record_completed_tool_call(
    ctx: &mut TurnContext,
    completion: CompletedToolCall<'_>,
    history: &mut ToolCallHistory,
) -> bool {
    let CompletedToolCall {
        call,
        args_summary,
        dedupe_key,
        result,
        duration_ms,
    } = completion;
    ctx.observer.audit_tool_execution(
        &ctx.session.id,
        &call.name,
        result.ok,
        (!args_summary.is_empty()).then_some(args_summary),
        &result.summary,
    );
    append_tool_result_message(
        &mut ctx.session,
        &call.id,
        &call.name,
        if result.ok {
            tool_result_provider_text(&call.name, result, false)
        } else {
            ToolFailureRecord::new(
                &call.name,
                &call.id,
                args_summary.to_string(),
                classify_tool_result_failure(result),
                tool_result_full_output(result),
            )
            .render_for_model()
        },
        !result.ok,
    );
    append_runtime_tool_message(
        &mut ctx.session,
        &call.name,
        format_tool_trace_message(result),
    );

    let needs_recovery = if result.ok {
        history.failed_calls.remove(&dedupe_key);
        history.failed_names.remove(&call.name);
        history.successful_keys.insert(dedupe_key);
        false
    } else {
        let error_summary = ToolFailureRecord::new(
            &call.name,
            &call.id,
            args_summary.to_string(),
            classify_tool_result_failure(result),
            tool_result_full_output(result),
        )
        .render_for_model();
        history.failed_calls.insert(dedupe_key, error_summary);
        history.failed_names.insert(call.name.clone());
        true
    };
    ctx.session.persist_to_disk();
    let _ = ctx.stream_tx.send(StreamEvent::ToolResult {
        name: call.name.clone(),
        tool_call_id: Some(call.id.clone()),
        ok: result.ok,
        output: tool_result_stream_output(result),
        full_output: Some(tool_result_full_output(result)),
        duration_ms: Some(duration_ms),
    });
    needs_recovery
}

fn append_failure_recovery_prompt(
    ctx: &mut TurnContext,
    history: &ToolCallHistory,
    request_tools: &[ToolSpec],
) {
    let mut failed_tools = history.failed_names.iter().cloned().collect::<Vec<_>>();
    failed_tools.sort();
    let collaboration_hint = "如果当前执行单元无法继续推进，请优先使用当前已注册工具中合适的协作或替代能力；仍无法解决时，再向用户说明需要的外部条件、凭据、授权、环境调整或人工确认。";
    let recall_hint = if request_tools
        .iter()
        .any(|tool| tool.name == "recall_memory")
    {
        "优先调用 recall_memory，充分查询这个工具以前成功调用时使用的参数、环境前置条件、配置方式、替代步骤和相关经验；只有回忆不足以解决时，再切换工具、使用其他已注册协作能力或请求用户协作。"
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
    ctx.session.messages.push(reminder);
    ctx.session.persist_to_disk();
}

enum NextStep {
    StartReact,
    DriveTools,
    StartSummary,
    StartForceFinal {
        reason: ForceFinalReason,
        request_injection_generation: u64,
        summary_error: Option<String>,
    },
    Waiting,
    Finish(TurnExecutionResult),
}

struct AgentLoopState {
    round: usize,
    react_rounds_in_phase: usize,
    outer_iteration: u32,
    executed_tool_in_phase: bool,
    accumulated_usage: TokenUsage,
    tool_history: ToolCallHistory,
    tool_batch: Option<ToolBatchState>,
    next_step: NextStep,
}

impl AgentLoopState {
    fn new() -> Self {
        Self {
            round: 0,
            react_rounds_in_phase: 0,
            outer_iteration: 0,
            executed_tool_in_phase: false,
            accumulated_usage: TokenUsage::default(),
            tool_history: ToolCallHistory::default(),
            tool_batch: None,
            next_step: NextStep::StartReact,
        }
    }

    fn reset_react_phase(&mut self) {
        self.react_rounds_in_phase = 0;
        self.executed_tool_in_phase = false;
        self.tool_history.clear();
        self.tool_batch = None;
    }
}

struct ToolBatchState {
    calls: VecDeque<(usize, ToolCall)>,
    ready_tools: Vec<PreparedToolCall>,
    prepared_keys: HashSet<String>,
    response_usage: TokenUsage,
    request_injection_generation: u64,
    needs_failure_recovery: bool,
}

struct PreparedToolCall {
    index: usize,
    call: ToolCall,
    args_summary: String,
    dedupe_key: String,
}

struct PendingApproval {
    request_id: String,
    tool: PreparedToolCall,
}

struct RunningToolCall {
    tool: PreparedToolCall,
    started_at: std::time::Instant,
}

struct ToolTaskOutput {
    result: ToolResult,
    duration_ms: u64,
}

enum LlmPurpose {
    React {
        request_injection_generation: u64,
    },
    Summary {
        iteration: u32,
        request_injection_generation: u64,
    },
    ForceFinal {
        request_injection_generation: u64,
        summary_error: Option<String>,
    },
}

struct ActiveLlm {
    purpose: LlmPurpose,
    pending_msg_id: String,
    sink: ThrottledStreamSink,
    chunk_rx: tokio_mpsc::UnboundedReceiver<ModelStreamChunk>,
    task: tokio::task::JoinHandle<anyhow::Result<ModelFunctionResponse>>,
    streamed_text: String,
    streamed_reasoning: String,
    streaming_usage: TokenUsage,
}

#[derive(Clone, Copy)]
enum ReactTextDisposition {
    Complete,
    EnterSummary,
}

enum CompressionContinuation {
    ReactText {
        pending_msg_id: String,
        disposition: ReactTextDisposition,
        request_injection_generation: u64,
    },
    ToolBatch,
    Summary {
        decision: SummaryDecision,
        request_injection_generation: u64,
    },
    ContextRetry {
        previous_summary_up_to: usize,
        error_message: String,
    },
}

type CompressionTaskOutput = (Session, anyhow::Result<CompressionUpdate>);

struct ActiveCompression {
    task: tokio::task::JoinHandle<CompressionTaskOutput>,
    observed_usage: TokenUsage,
    previous_summary_up_to: usize,
    total_messages: usize,
    continuation: CompressionContinuation,
}

fn build_react_request(ctx: &TurnContext) -> ModelRequest {
    let (thinking, reasoning_effort, thinking_disabled) = build_thinking_config(ctx);
    ModelRequest {
        session_title: ctx.session.title.clone(),
        user_input: String::new(),
        context: ctx.session.context(),
        thinking,
        reasoning_effort,
        thinking_disabled,
    }
}

fn start_llm_request(
    ctx: &TurnContext,
    request: ModelRequest,
    purpose: LlmPurpose,
    text_kind: StreamTextKind,
    tool_choice: Option<ToolChoice>,
) -> ActiveLlm {
    let pending_msg_id = scru128::new().to_string();
    let sink = ThrottledStreamSink::with_text_kind(
        pending_msg_id.clone(),
        ctx.stream_tx.clone(),
        text_kind,
    );
    let (chunk_tx, chunk_rx) = tokio_mpsc::unbounded_channel();
    let client = select_client_for_request(ctx, &request).clone();
    let tools = ctx.tools.clone();
    let task = tokio::spawn(async move {
        client
            .stream_function_calls_with_tool_choice(request, tools, tool_choice, chunk_tx)
            .await
    });

    ActiveLlm {
        purpose,
        pending_msg_id,
        sink,
        chunk_rx,
        task,
        streamed_text: String::new(),
        streamed_reasoning: String::new(),
        streaming_usage: TokenUsage::default(),
    }
}

fn persist_streamed_react_message(
    ctx: &mut TurnContext,
    pending_msg_id: &str,
    streamed_text: &str,
    streamed_reasoning: &str,
) {
    if streamed_text.trim().is_empty() && streamed_reasoning.trim().is_empty() {
        return;
    }
    upsert_assistant_text_message(
        &mut ctx.session,
        pending_msg_id,
        streamed_text,
        streamed_reasoning,
        MessagePhase::React,
    );
    emit_session_message_upsert(ctx, pending_msg_id);
}

fn handle_react_text_response(
    ctx: &mut TurnContext,
    pending_msg_id: &str,
    response: &ModelFunctionResponse,
    state: &AgentLoopState,
) -> ReactTextDisposition {
    if is_synthetic_tool_call_placeholder(&response.text) {
        return ReactTextDisposition::EnterSummary;
    }

    upsert_assistant_text_message(
        &mut ctx.session,
        pending_msg_id,
        &response.text,
        &response.reasoning_content,
        MessagePhase::React,
    );
    if let Some(message) = ctx
        .session
        .messages
        .iter_mut()
        .find(|message| message.id == pending_msg_id)
    {
        message.reasoning_signature = response.reasoning_signature.clone();
    }
    emit_session_message_upsert(ctx, pending_msg_id);
    append_runtime_tool_message_with_reasoning(
        &mut ctx.session,
        "llm_output",
        format_llm_output_message(&LlmOutputRecord {
            stage: format!("react-round-{}", state.round),
            content: response.text.clone(),
            reasoning_content: response.reasoning_content.clone(),
            tool_calls: Vec::new(),
            usage: response.usage.clone(),
        }),
        response.reasoning_content.clone(),
    );
    ctx.session.persist_to_disk();

    let direct_answer = state.outer_iteration == 0
        && !state.executed_tool_in_phase
        && !response.text.trim().is_empty();
    let tool_answer = state.executed_tool_in_phase && looks_like_final_answer(&response.text);
    if direct_answer || tool_answer {
        ReactTextDisposition::Complete
    } else {
        ReactTextDisposition::EnterSummary
    }
}

fn start_tool_execution(
    ctx: &mut TurnContext,
    tool: PreparedToolCall,
    tasks: &mut JoinSet<ToolTaskOutput>,
    running_tools: &mut HashMap<TaskId, RunningToolCall>,
) {
    let _ = ctx.stream_tx.send(StreamEvent::ToolStart {
        name: tool.call.name.clone(),
        args_summary: tool.args_summary.clone(),
    });
    let actor_id = ctx.session.id.clone();
    let future = start_tool_call(ctx, &tool.call, &actor_id);
    let started_at = std::time::Instant::now();
    let task = tasks.spawn(async move {
        let result = future.await;
        ToolTaskOutput {
            result,
            duration_ms: started_at.elapsed().as_millis() as u64,
        }
    });
    running_tools.insert(task.id(), RunningToolCall { tool, started_at });
}

fn start_context_compression(
    ctx: &TurnContext,
    observed_usage: TokenUsage,
    continuation: CompressionContinuation,
) -> ActiveCompression {
    let previous_summary_up_to = ctx.session.summary_up_to;
    let total_messages = ctx.session.messages.len();
    let _ = ctx.stream_tx.send(StreamEvent::ContextCompressing {
        summary_up_to: previous_summary_up_to,
        total_messages,
    });
    let mut session = ctx.session.clone();
    let client = ctx.client.clone();
    let task = tokio::spawn(async move {
        let result = ContextCompressor::new(6)
            .update_summary_with_usage_async(&mut session, &client)
            .await;
        (session, result)
    });
    ActiveCompression {
        task,
        observed_usage,
        previous_summary_up_to,
        total_messages,
        continuation,
    }
}

fn apply_context_compression(
    ctx: &mut TurnContext,
    active: ActiveCompression,
    result: Result<CompressionTaskOutput, tokio::task::JoinError>,
) -> CompressionContinuation {
    let ActiveCompression {
        observed_usage,
        previous_summary_up_to,
        total_messages,
        continuation,
        ..
    } = active;
    let result = match result {
        Ok((session, result)) => result.map(|update| (session, update)),
        Err(error) => Err(anyhow::anyhow!(error.to_string())),
    };

    match result {
        Ok((session, update)) if update.compressed => {
            ctx.session.context_summary = session.context_summary;
            ctx.session.summary_up_to = session.summary_up_to;
            mark_compact_boundary(&mut ctx.session.messages, ctx.session.summary_up_to);
            let remaining = ctx
                .session
                .messages
                .len()
                .saturating_sub(ctx.session.summary_up_to);
            let current_tokens = (observed_total_tokens(&observed_usage) as f64
                * (remaining as f64 / total_messages.max(1) as f64))
                as usize;
            ctx.session.current_tokens = current_tokens;
            ctx.session.token_usage.accumulate(&update.usage);
            rebuild_system_prompt(ctx);
            if let Err(error) = ctx.session.try_persist_to_disk() {
                tracing::warn!(%error, session_id = %ctx.session.id, "上下文压缩落盘失败");
                return continuation;
            }
            emit_token_usage(
                &ctx.stream_tx,
                &update.usage,
                Some(current_tokens),
                ctx.context_limit,
                "context_summary",
                None,
            );
            let _ = ctx.stream_tx.send(StreamEvent::ContextCompressed {
                action: ContextCompressAction::Auto,
                summary_up_to: ctx.session.summary_up_to,
                remaining_messages: remaining,
            });
            tracing::info!(
                session_id = %ctx.session.id,
                observed_tokens = observed_total_tokens(&observed_usage),
                old_summary_up_to = previous_summary_up_to,
                summary_up_to = ctx.session.summary_up_to,
                "上下文摘要已更新"
            );
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(
                session_id = %ctx.session.id,
                error = %error,
                "上下文压缩失败，继续使用原始上下文"
            );
        }
    }

    continuation
}

fn needs_context_compression(ctx: &TurnContext, usage: &TokenUsage) -> bool {
    ContextOrganizer::new(ctx.context_limit)
        .with_threshold(0.95)
        .needs_compression(observed_total_tokens(usage))
}

fn finish_react_text(
    ctx: &mut TurnContext,
    state: &mut AgentLoopState,
    injections: &mut ToolInjectionBuffer,
    pending_msg_id: String,
    disposition: ReactTextDisposition,
    request_injection_generation: u64,
) {
    let received_new_injection = injections.generation() > request_injection_generation;
    injections.commit(ctx);
    if received_new_injection {
        state.next_step = NextStep::StartReact;
        return;
    }

    match disposition {
        ReactTextDisposition::Complete => {
            if let Some(message) = ctx
                .session
                .messages
                .iter_mut()
                .find(|message| message.id == pending_msg_id)
            {
                message.phase = MessagePhase::Summary;
            }
            emit_session_message_upsert(ctx, &pending_msg_id);
            ctx.session.persist_to_disk();
            state.next_step = NextStep::Finish(TurnExecutionResult::success(
                state.accumulated_usage.clone(),
            ));
        }
        ReactTextDisposition::EnterSummary => state.next_step = NextStep::StartSummary,
    }
}

fn finish_summary(
    ctx: &mut TurnContext,
    state: &mut AgentLoopState,
    injections: &mut ToolInjectionBuffer,
    decision: SummaryDecision,
    request_injection_generation: u64,
) {
    ctx.session.persist_to_disk();
    let received_new_injection = injections.generation() > request_injection_generation;
    injections.commit(ctx);
    if received_new_injection {
        state.reset_react_phase();
        state.next_step = NextStep::StartReact;
        return;
    }

    match decision {
        SummaryDecision::Done(_) | SummaryDecision::AskUser(_) => {
            state.next_step = NextStep::Finish(TurnExecutionResult::success(
                state.accumulated_usage.clone(),
            ));
        }
        SummaryDecision::NeedMoreWork(reason) => {
            state.outer_iteration += 1;
            if state.outer_iteration >= ctx.max_outer_iterations {
                state.next_step = NextStep::StartForceFinal {
                    reason: ForceFinalReason::OuterLimit,
                    request_injection_generation,
                    summary_error: None,
                };
                return;
            }
            inject_tool_to_messages(
                &mut ctx.session,
                "summary_need_more_work",
                &serde_json::json!({
                    "reason": reason.trim(),
                    "instruction": "上轮总结判定任务未完成，请根据原因继续执行剩余工作。",
                }),
            );
            ctx.session.persist_to_disk();
            state.reset_react_phase();
            state.next_step = NextStep::StartReact;
        }
    }
}

/// 执行一个完整的对话轮次。
///
/// `cmd_rx` 只在这一处消费。模型 chunk、工具结果和上下文压缩结果与命令进入同一个
/// `tokio::select!`，因此任何异步阶段运行期间都能立即响应取消和运行态反馈。
pub(super) async fn execute_turn(
    ctx: &mut TurnContext,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
) -> TurnExecutionResult {
    let stream_tx = ctx.stream_tx.clone();
    let context_limit = ctx.context_limit;
    let plugins = ctx.plugins.clone();
    let request_tools = ctx.tools.clone();
    let mut trust_mode = ctx.trust_mode;
    let mut injections = ToolInjectionBuffer::new(ctx);
    let mut state = AgentLoopState::new();
    let mut active_llm: Option<ActiveLlm> = None;
    let mut tool_tasks = JoinSet::<ToolTaskOutput>::new();
    let mut running_tools = HashMap::<TaskId, RunningToolCall>::new();
    let mut active_compression: Option<ActiveCompression> = None;
    let mut pending_approval: Option<PendingApproval> = None;

    let result = 'agent_loop: loop {
        let can_advance = active_llm.is_none()
            && tool_tasks.is_empty()
            && active_compression.is_none()
            && pending_approval.is_none()
            && !matches!(state.next_step, NextStep::Waiting);

        tokio::select! {
            biased;

            // 命令永远具有最高优先级；Cancel/Shutdown 会关闭接收端并直接终止活跃任务。
            command = cmd_rx.recv() => match command {
                Some(Command::Cancel | Command::Shutdown) | None => {
                    cmd_rx.close();

                    if let Some(active) = active_llm.take() {
                        let ActiveLlm {
                            purpose,
                            pending_msg_id,
                            sink,
                            task,
                            streamed_text,
                            streamed_reasoning,
                            streaming_usage,
                            ..
                        } = active;
                        sink.finish();
                        abort_and_join(task).await;
                        match purpose {
                            LlmPurpose::React { .. } => persist_streamed_react_message(
                                ctx,
                                &pending_msg_id,
                                &streamed_text,
                                &streamed_reasoning,
                            ),
                            LlmPurpose::Summary { .. } => persist_partial_summary(
                                ctx,
                                &pending_msg_id,
                                &streamed_text,
                                &streamed_reasoning,
                            ),
                            LlmPurpose::ForceFinal { .. } => {}
                        }
                        emit_cancel_usage(&stream_tx, &streaming_usage, context_limit);
                        state.accumulated_usage.accumulate(&streaming_usage);
                    }

                    if !running_tools.is_empty() {
                        tool_tasks.shutdown().await;
                        let mut interrupted = running_tools.drain().map(|(_, tool)| tool).collect::<Vec<_>>();
                        interrupted.sort_by_key(|running| running.tool.index);
                        let mut interrupted_events = Vec::with_capacity(interrupted.len());
                        for running in interrupted {
                            let duration_ms = running.started_at.elapsed().as_millis() as u64;
                            let output = "工具调用因执行取消或会话关闭而中断。".to_string();
                            append_tool_result_message(
                                &mut ctx.session,
                                &running.tool.call.id,
                                &running.tool.call.name,
                                output,
                                true,
                            );
                            interrupted_events.push(StreamEvent::ToolResult {
                                name: running.tool.call.name,
                                tool_call_id: Some(running.tool.call.id),
                                ok: false,
                                output: "工具调用因执行取消或会话关闭而中断。".to_string(),
                                full_output: None,
                                duration_ms: Some(duration_ms),
                            });
                        }
                        ctx.session.persist_to_disk();
                        for event in interrupted_events {
                            let _ = stream_tx.send(event);
                        }
                    }

                    if let Some(active) = active_compression.take() {
                        abort_and_join(active.task).await;
                    }

                    // 取消前已接收的插件结果仍属于本轮，必须进入 Session 或延迟队列。
                    injections.commit(ctx);
                    break 'agent_loop TurnExecutionResult::cancelled(state.accumulated_usage);
                }
                Some(Command::Approval { request_id, approved }) => {
                    let matches_pending = pending_approval
                        .as_ref()
                        .is_some_and(|pending| pending.request_id == request_id);
                    if matches_pending {
                        let pending = pending_approval.take().expect("审批状态必须存在");
                        if approved {
                            start_tool_execution(
                                ctx,
                                pending.tool,
                                &mut tool_tasks,
                                &mut running_tools,
                            );
                            state.next_step = NextStep::Waiting;
                        } else {
                            record_rejected_tool_call(
                                ctx,
                                &pending.tool.call,
                                &pending.tool.args_summary,
                            );
                            let request_generation = state
                                .tool_batch
                                .take()
                                .map(|batch| batch.request_injection_generation)
                                .unwrap_or_else(|| injections.generation());
                            let received_new_injection =
                                injections.generation() > request_generation;
                            injections.commit(ctx);
                            state.next_step = if received_new_injection {
                                NextStep::StartReact
                            } else {
                                NextStep::Finish(TurnExecutionResult::success(
                                    state.accumulated_usage.clone(),
                                ))
                            };
                        }
                    }
                }
                Some(Command::SetTrustMode(mode)) => {
                    set_runtime_trust_mode(&mut trust_mode, &plugins, mode);
                    if mode == TrustMode::FullTrust
                        && let Some(pending) = pending_approval.take()
                    {
                        state
                            .tool_batch
                            .as_mut()
                            .expect("审批必须属于活跃工具批次")
                            .ready_tools
                            .push(pending.tool);
                        state.next_step = NextStep::DriveTools;
                    }
                }
                Some(Command::InjectTool { tool_name, payload }) => {
                    injections.receive(&stream_tx, tool_name, payload);
                }
                Some(Command::EmitStreamEvent(event)) => {
                    let _ = stream_tx.send(*event);
                }
                Some(Command::ReportUsage {
                    usage,
                    source,
                    emit_event,
                }) => record_plugin_usage(
                    &stream_tx,
                    context_limit,
                    &mut state.accumulated_usage,
                    usage,
                    source,
                    emit_event,
                ),
            },

            // 模型任务把流式内容写入此通道；通道关闭表示完整响应已经可以收取。
            chunk = async {
                active_llm
                    .as_mut()
                    .expect("LLM 分支只在请求活跃时启用")
                    .chunk_rx
                    .recv()
                    .await
            }, if active_llm.is_some() => {
                if let Some(chunk) = chunk {
                    let active = active_llm.as_mut().expect("LLM 状态必须存在");
                    if let Some(chunk_usage) = &chunk.usage {
                        let usage: TokenUsage = chunk_usage.clone().into();
                        active.streaming_usage.accumulate(&usage);
                    }
                    active.streamed_text.push_str(&chunk.content);
                    active.streamed_reasoning.push_str(&chunk.reasoning_content);
                    active.sink.push_chunk(&chunk);
                    continue 'agent_loop;
                }

                let active = active_llm.take().expect("LLM 状态必须存在");
                let ActiveLlm {
                    purpose,
                    pending_msg_id,
                    sink,
                    task,
                    streamed_text,
                    streamed_reasoning,
                    streaming_usage,
                    ..
                } = active;
                sink.finish();
                let response_result = match task.await {
                    Ok(result) => result,
                    Err(error) => Err(anyhow::anyhow!(error.to_string())),
                };

                match purpose {
                    LlmPurpose::React {
                        request_injection_generation,
                    } => {
                        persist_streamed_react_message(
                            ctx,
                            &pending_msg_id,
                            &streamed_text,
                            &streamed_reasoning,
                        );
                        let response = match response_result {
                            Ok(response) => response,
                            Err(error) => {
                                let error_message = error.to_string();
                                let should_compress = error_message.contains("context_window_exceeded")
                                    || error_message.contains("context_length_exceeded")
                                    || (error_message.contains("content_blocks=0")
                                        && error_message.contains("stop_reason=end_turn"));
                                if should_compress {
                                    tracing::warn!("检测到上下文超限，尝试强制压缩");
                                    let previous_summary_up_to = ctx.session.summary_up_to;
                                    let forced_usage = TokenUsage {
                                        prompt_tokens: context_limit,
                                        completion_tokens: 0,
                                        total_tokens: context_limit,
                                        prompt_cache_hit_tokens: None,
                                        prompt_cache_miss_tokens: None,
                                    };
                                    active_compression = Some(start_context_compression(
                                        ctx,
                                        forced_usage,
                                        CompressionContinuation::ContextRetry {
                                            previous_summary_up_to,
                                            error_message,
                                        },
                                    ));
                                    state.next_step = NextStep::Waiting;
                                } else {
                                    injections.commit(ctx);
                                    persist_error(ctx, format!("ReAct 循环请求失败：{error_message}"));
                                    state.next_step = NextStep::Finish(TurnExecutionResult::failed(
                                        state.accumulated_usage.clone(),
                                        error_message,
                                    ));
                                }
                                continue 'agent_loop;
                            }
                        };

                        state.accumulated_usage.accumulate(&response.usage);
                        emit_token_usage(
                            &stream_tx,
                            &response.usage,
                            Some(response.usage.prompt_tokens.max(ctx.session.current_tokens)),
                            context_limit,
                            format!("react-round-{}", state.round + 1),
                            None,
                        );
                        state.round += 1;
                        state.react_rounds_in_phase += 1;

                        if response.tool_calls.is_empty() {
                            let disposition = handle_react_text_response(
                                ctx,
                                &pending_msg_id,
                                &response,
                                &state,
                            );
                            if needs_context_compression(ctx, &response.usage) {
                                active_compression = Some(start_context_compression(
                                    ctx,
                                    response.usage,
                                    CompressionContinuation::ReactText {
                                        pending_msg_id,
                                        disposition,
                                        request_injection_generation,
                                    },
                                ));
                                state.next_step = NextStep::Waiting;
                            } else {
                                finish_react_text(
                                    ctx,
                                    &mut state,
                                    &mut injections,
                                    pending_msg_id,
                                    disposition,
                                    request_injection_generation,
                                );
                            }
                        } else {
                            state.executed_tool_in_phase = true;
                            let calls = record_tool_calls(ctx, &pending_msg_id, &response, state.round);
                            state.tool_batch = Some(ToolBatchState {
                                calls: calls.into_iter().enumerate().collect(),
                                ready_tools: Vec::new(),
                                prepared_keys: HashSet::new(),
                                response_usage: response.usage,
                                request_injection_generation,
                                needs_failure_recovery: false,
                            });
                            state.next_step = NextStep::DriveTools;
                        }
                    }
                    LlmPurpose::Summary {
                        iteration,
                        request_injection_generation,
                    } => {
                        let response = match response_result {
                            Ok(response) => response,
                            Err(error) => {
                                let message = error.to_string();
                                persist_partial_summary(
                                    ctx,
                                    &pending_msg_id,
                                    &streamed_text,
                                    &streamed_reasoning,
                                );
                                state.accumulated_usage.accumulate(&streaming_usage);
                                persist_error(ctx, format!("总结阶段失败：{message}"));
                                state.next_step = NextStep::StartForceFinal {
                                    reason: ForceFinalReason::SummaryError,
                                    request_injection_generation,
                                    summary_error: Some(message),
                                };
                                continue 'agent_loop;
                            }
                        };

                        emit_token_usage(
                            &stream_tx,
                            &response.usage,
                            Some(response.usage.prompt_tokens.max(ctx.session.current_tokens)),
                            context_limit,
                            format!("summary-iteration-{iteration}"),
                            None,
                        );
                        let mut usage = response.usage.clone();
                        if usage.total_tokens == 0 {
                            usage.accumulate(&streaming_usage);
                        }
                        state.accumulated_usage.accumulate(&usage);

                        if !response.tool_calls.is_empty() {
                            tracing::warn!(
                                count = response.tool_calls.len(),
                                protocol = ?ctx.client().protocol(),
                                "summary phase returned tool calls despite ToolChoice::None"
                            );
                            if response.text.trim().is_empty() {
                                let message =
                                    "总结阶段无视 ToolChoice::None 返回了工具调用且无文本回复"
                                        .to_string();
                                persist_error(ctx, format!("总结阶段失败：{message}"));
                                state.next_step = NextStep::StartForceFinal {
                                    reason: ForceFinalReason::SummaryError,
                                    request_injection_generation,
                                    summary_error: Some(message),
                                };
                                continue 'agent_loop;
                            }
                        }

                        let decision = parse_summary_phase_output(&response.text);
                        let summary_content = decision.payload().to_string();
                        let needs_more_work = matches!(decision, SummaryDecision::NeedMoreWork(_));
                        if !needs_more_work && summary_content.trim().is_empty() {
                            if let Some(message_id) = promote_last_react_message_to_summary(&mut ctx.session) {
                                emit_session_message_upsert(ctx, &message_id);
                            }
                        } else {
                            upsert_assistant_text_message(
                                &mut ctx.session,
                                &pending_msg_id,
                                &summary_content,
                                &response.reasoning_content,
                                MessagePhase::Normal,
                            );
                            if let Some(message) = ctx
                                .session
                                .messages
                                .iter_mut()
                                .find(|message| message.id == pending_msg_id)
                            {
                                message.reasoning_signature = response.reasoning_signature;
                                message.phase = if needs_more_work {
                                    MessagePhase::React
                                } else {
                                    MessagePhase::Summary
                                };
                            }
                            emit_session_message_upsert(ctx, &pending_msg_id);
                        }

                        if needs_context_compression(ctx, &usage) {
                            active_compression = Some(start_context_compression(
                                ctx,
                                usage,
                                CompressionContinuation::Summary {
                                    decision,
                                    request_injection_generation,
                                },
                            ));
                            state.next_step = NextStep::Waiting;
                        } else {
                            finish_summary(
                                ctx,
                                &mut state,
                                &mut injections,
                                decision,
                                request_injection_generation,
                            );
                        }
                    }
                    LlmPurpose::ForceFinal {
                        request_injection_generation,
                        summary_error,
                    } => {
                        let force_result = match response_result {
                            Ok(response) => match commit_summary_message(
                                ctx,
                                &pending_msg_id,
                                &response,
                                "force_final_response",
                            ) {
                                Ok(()) => {
                                    state.accumulated_usage.accumulate(&response.usage);
                                    Ok(())
                                }
                                Err(message) => Err(message),
                            },
                            Err(error) => {
                                let message = error.to_string();
                                persist_error(ctx, format!("force_final_response 失败：{message}"));
                                Err(message)
                            }
                        };
                        let received_new_injection =
                            injections.generation() > request_injection_generation;
                        injections.commit(ctx);
                        if received_new_injection {
                            state.reset_react_phase();
                            state.next_step = NextStep::StartReact;
                            continue 'agent_loop;
                        }
                        state.next_step = match force_result {
                            Ok(()) => NextStep::Finish(TurnExecutionResult::success(
                                state.accumulated_usage.clone(),
                            )),
                            Err(message) => {
                                let message = summary_error.map_or(message.clone(), |summary_error| {
                                    format!(
                                        "总结阶段失败：{summary_error}；强制最终回复失败：{message}"
                                    )
                                });
                                NextStep::Finish(TurnExecutionResult::failed(
                                    state.accumulated_usage.clone(),
                                    message,
                                ))
                            }
                        };
                    }
                }
            }

            // 每个工具都是独立 Tokio task；单项完成后立即反馈并持久化，但必须等整批结束才继续。
            tool_result = tool_tasks.join_next_with_id(), if !tool_tasks.is_empty() => {
                let joined = tool_result.expect("工具任务集合非空时必须返回结果");
                let (task_id, task_output) = match joined {
                    Ok((task_id, output)) => (task_id, output),
                    Err(error) => {
                        let task_id = error.id();
                        let running = running_tools
                            .get(&task_id)
                            .expect("异常工具任务必须存在运行记录");
                        let message = format!("工具任务异常结束：{error}");
                        (
                            task_id,
                            ToolTaskOutput {
                                result: ToolResult {
                                    ok: false,
                                    summary: message.clone(),
                                    stdout: String::new(),
                                    stderr: message,
                                    exit_code: 1,
                                    execution: None,
                                },
                                duration_ms: running.started_at.elapsed().as_millis() as u64,
                            },
                        )
                    }
                };
                let running = running_tools
                    .remove(&task_id)
                    .expect("完成的工具任务必须存在运行记录");
                let needs_recovery = record_completed_tool_call(
                    ctx,
                    CompletedToolCall {
                        call: &running.tool.call,
                        args_summary: &running.tool.args_summary,
                        dedupe_key: running.tool.dedupe_key,
                        result: &task_output.result,
                        duration_ms: task_output.duration_ms,
                    },
                    &mut state.tool_history,
                );
                // record_completed_tool_call 已先持久化该结果，再向 App 发布完成事件。
                state
                    .tool_batch
                    .as_mut()
                    .expect("工具结果必须属于活跃批次")
                    .needs_failure_recovery |= needs_recovery;

                if !tool_tasks.is_empty() {
                    state.next_step = NextStep::Waiting;
                    continue 'agent_loop;
                }

                let batch = state
                    .tool_batch
                    .as_ref()
                    .expect("工具结果必须属于活跃批次");
                if !batch.calls.is_empty() || !batch.ready_tools.is_empty() {
                    state.next_step = NextStep::DriveTools;
                    continue 'agent_loop;
                }

                let usage = batch.response_usage.clone();
                if needs_context_compression(ctx, &usage) {
                    active_compression = Some(start_context_compression(
                        ctx,
                        usage,
                        CompressionContinuation::ToolBatch,
                    ));
                    state.next_step = NextStep::Waiting;
                } else {
                    state.next_step = NextStep::DriveTools;
                }
            }

            // 自动压缩也在同一循环中等待，避免压缩请求期间失去取消响应。
            compression_result = async {
                (&mut active_compression
                    .as_mut()
                    .expect("压缩分支只在任务活跃时启用")
                    .task)
                    .await
            }, if active_compression.is_some() =>
            {
                let active = active_compression.take().expect("压缩状态必须存在");
                let continuation = apply_context_compression(ctx, active, compression_result);
                match continuation {
                    CompressionContinuation::ReactText {
                        pending_msg_id,
                        disposition,
                        request_injection_generation,
                    } => finish_react_text(
                        ctx,
                        &mut state,
                        &mut injections,
                        pending_msg_id,
                        disposition,
                        request_injection_generation,
                    ),
                    CompressionContinuation::ToolBatch => {
                        state.next_step = NextStep::DriveTools;
                    }
                    CompressionContinuation::Summary {
                        decision,
                        request_injection_generation,
                    } => finish_summary(
                        ctx,
                        &mut state,
                        &mut injections,
                        decision,
                        request_injection_generation,
                    ),
                    CompressionContinuation::ContextRetry {
                        previous_summary_up_to,
                        error_message,
                    } => {
                        injections.commit(ctx);
                        if ctx.session.summary_up_to > previous_summary_up_to {
                            state.next_step = NextStep::StartReact;
                        } else {
                            persist_error(ctx, format!("ReAct 循环请求失败：{error_message}"));
                            state.next_step = NextStep::Finish(TurnExecutionResult::failed(
                                state.accumulated_usage.clone(),
                                error_message,
                            ));
                        }
                    }
                }
            }

            // 无异步阶段活跃时推进一次状态；命令分支 biased 在前，会先排空已到达命令。
            _ = std::future::ready(()), if can_advance => {
                let next_step = std::mem::replace(&mut state.next_step, NextStep::Waiting);
                match next_step {
                    NextStep::StartReact => {
                        if state.react_rounds_in_phase >= ctx.max_tool_rounds {
                            state.next_step = NextStep::StartSummary;
                            continue 'agent_loop;
                        }
                        injections.commit(ctx);
                        let request_injection_generation = injections.generation();
                        if state.round == 0 {
                            debug_assert!(
                                ctx.session.system_prompt_message.is_some(),
                                "TurnContext 构建前应已注入 system prompt"
                            );
                        } else {
                            let _ = stream_tx.send(StreamEvent::PhaseChanged {
                                phase: "analyzing".to_string(),
                                iteration: (state.round + 1) as u32,
                            });
                        }
                        active_llm = Some(start_llm_request(
                            ctx,
                            build_react_request(ctx),
                            LlmPurpose::React {
                                request_injection_generation,
                            },
                            StreamTextKind::React,
                            None,
                        ));
                    }
                    NextStep::DriveTools => {
                        let call = state
                            .tool_batch
                            .as_mut()
                            .and_then(|batch| batch.calls.pop_front());
                        let Some((index, call)) = call else {
                            let ready_tools = std::mem::take(
                                &mut state
                                    .tool_batch
                                    .as_mut()
                                    .expect("工具批次必须存在")
                                    .ready_tools,
                            );
                            if !ready_tools.is_empty() {
                                for tool in ready_tools {
                                    start_tool_execution(
                                        ctx,
                                        tool,
                                        &mut tool_tasks,
                                        &mut running_tools,
                                    );
                                }
                                state.next_step = NextStep::Waiting;
                                continue 'agent_loop;
                            }

                            let batch = state.tool_batch.take().expect("工具批次必须存在");
                            if batch.needs_failure_recovery {
                                append_failure_recovery_prompt(ctx, &state.tool_history, &request_tools);
                            } else {
                                ctx.session.persist_to_disk();
                            }
                            injections.commit(ctx);
                            state.next_step = NextStep::StartReact;
                            continue 'agent_loop;
                        };

                        match prepare_tool_call(ctx, &call, &mut state.tool_history) {
                            ToolPreflightOutcome::Skip { needs_recovery } => {
                                state
                                    .tool_batch
                                    .as_mut()
                                    .expect("工具批次必须存在")
                                    .needs_failure_recovery |= needs_recovery;
                                ctx.session.persist_to_disk();
                                state.next_step = NextStep::DriveTools;
                            }
                            ToolPreflightOutcome::Execute {
                                args_summary,
                                dedupe_key,
                            } => {
                                let first_in_batch = state
                                    .tool_batch
                                    .as_mut()
                                    .expect("工具批次必须存在")
                                    .prepared_keys
                                    .insert(dedupe_key.clone());
                                if !first_in_batch {
                                    record_parallel_duplicate_tool_call(ctx, &call);
                                    state.next_step = NextStep::DriveTools;
                                    continue 'agent_loop;
                                }
                                let tool = PreparedToolCall {
                                    index,
                                    call,
                                    args_summary,
                                    dedupe_key,
                                };
                                let trust_mode_label = format!("{trust_mode:?}");
                                if trust_mode == TrustMode::FullTrust {
                                    ctx.observer.audit_permission(
                                        &ctx.session.id,
                                        &tool.call.name,
                                        "approved",
                                        &trust_mode_label,
                                        (!tool.args_summary.is_empty()).then_some(tool.args_summary.as_str()),
                                    );
                                    state
                                        .tool_batch
                                        .as_mut()
                                        .expect("工具批次必须存在")
                                        .ready_tools
                                        .push(tool);
                                    state.next_step = NextStep::DriveTools;
                                } else {
                                    let request_id = scru128::new().to_string();
                                    ctx.observer.audit_permission(
                                        &ctx.session.id,
                                        &tool.call.name,
                                        "needs_approval",
                                        &trust_mode_label,
                                        (!tool.args_summary.is_empty()).then_some(tool.args_summary.as_str()),
                                    );
                                    let _ = stream_tx.send(StreamEvent::ApprovalNeeded {
                                        request_id: request_id.clone(),
                                        tool_name: tool.call.name.clone(),
                                        args_summary: tool.args_summary.clone(),
                                    });
                                    pending_approval = Some(PendingApproval { request_id, tool });
                                }
                            }
                        }
                    }
                    NextStep::StartSummary => {
                        injections.commit(ctx);
                        let request_injection_generation = injections.generation();
                        let iteration = state.outer_iteration + 1;
                        let _ = stream_tx.send(StreamEvent::PhaseChanged {
                            phase: "summary".to_string(),
                            iteration,
                        });
                        if ctx.session.system_prompt_message.is_none() {
                            rebuild_system_prompt(ctx);
                        }
                        active_llm = Some(start_llm_request(
                            ctx,
                            request_for_summary_phase(&ctx.session),
                            LlmPurpose::Summary {
                                iteration,
                                request_injection_generation,
                            },
                            StreamTextKind::Summary,
                            Some(ToolChoice::None),
                        ));
                    }
                    NextStep::StartForceFinal {
                        reason,
                        request_injection_generation,
                        summary_error,
                    } => {
                        let request = build_force_final_request(ctx, reason);
                        active_llm = Some(start_llm_request(
                            ctx,
                            request,
                            LlmPurpose::ForceFinal {
                                request_injection_generation,
                                summary_error,
                            },
                            StreamTextKind::Summary,
                            Some(ToolChoice::None),
                        ));
                    }
                    NextStep::Finish(result) => break 'agent_loop result,
                    NextStep::Waiting => unreachable!("等待状态不能主动推进"),
                }
            }
        }
    };

    // 信任模式运行时即时生效，Session 仍只在本轮唯一出口接收最终值。
    ctx.trust_mode = trust_mode;
    ctx.session.trust_mode = trust_mode;
    injections.commit(ctx);
    result
}

/// 通用工具参数摘要:把 JSON arguments 的 key=value 拼成简短字符串。
fn format_tool_args_summary(call: &crate::model::ToolCall) -> String {
    let Some(obj) = call.arguments.as_object() else {
        return String::new();
    };
    if obj.is_empty() {
        return String::new();
    }
    obj.iter()
        .map(|(k, v)| {
            let val = match v {
                serde_json::Value::String(s) if s.chars().count() > 80 => {
                    format!("{}...", s.chars().take(77).collect::<String>())
                }
                serde_json::Value::String(s) => s.clone(),
                other => {
                    let s = other.to_string();
                    if s.chars().count() > 80 {
                        format!("{}...", s.chars().take(77).collect::<String>())
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

#[cfg(test)]
mod tests {
    //! `execute_turn` 单元测试。
    //!
    //! 模型调用通过 `wiremock` 起本地 HTTP 服务器返回 OpenAI Chat Completions SSE
    //! 流,避免依赖真实网络。`execute_turn` 是 `pub(super)`,测试只能放在 `react`
    //! 模块内部。turn 结束的信号是 `execute_turn` future 返回(它本身不发
    //! `StreamEvent::Done`,终态事件由上层 `run_turn` 发送)。

    use super::super::outcome::{TurnExecutionOutcome, TurnExecutionResult};
    use super::execute_turn;
    use crate::agent_config::AgentConfig;
    use crate::core::command::Command;
    use crate::core::plugin::Plugin;
    use crate::model::SingleProviderClient;
    use crate::model::{ToolCall, ToolSpec};
    use crate::observe::Observer;
    use crate::permission::TrustMode;
    use crate::prompt::SystemPromptConfig;
    use crate::session::{MessageRole, Session};
    use crate::tool::ToolResult;
    use crate::tool_override::{PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider};
    use crate::turn_context::TurnContext;
    use std::collections::HashMap;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tiangong_llm::{ModelEndpoint, ProviderProtocol};
    use tiangong_types::{StreamEvent, TokenUsage};
    use tokio::sync::{Barrier, Notify};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// 构造一条 OpenAI SSE chunk(`data: {json}\n\n`),末尾追加 `[DONE]`。
    fn sse_body(chunks: &[serde_json::Value]) -> Vec<u8> {
        let mut body = String::new();
        for chunk in chunks {
            body.push_str(&format!("data: {}\n\n", chunk));
        }
        body.push_str("data: [DONE]\n\n");
        body.into_bytes()
    }

    /// 纯文本 delta chunk。
    fn text_delta_chunk(content: &str) -> serde_json::Value {
        serde_json::json!({
            "id": "chatcmpl-1",
            "object": "chat.completion.chunk",
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant", "content": content},
                "finish_reason": null,
            }],
        })
    }

    /// usage chunk(`choices: []` + usage,符合 stream_options.include_usage 约定)。
    fn usage_chunk(prompt: u64, completion: u64) -> serde_json::Value {
        serde_json::json!({
            "id": "chatcmpl-1",
            "object": "chat.completion.chunk",
            "model": "test-model",
            "choices": [],
            "usage": {
                "prompt_tokens": prompt,
                "completion_tokens": completion,
                "total_tokens": prompt + completion,
            },
        })
    }

    /// tool_calls delta chunk(单个工具调用,一次性给出 name + 完整 arguments)。
    fn tool_call_chunk(call_id: &str, name: &str, arguments: &str) -> serde_json::Value {
        serde_json::json!({
            "id": "chatcmpl-1",
            "object": "chat.completion.chunk",
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": call_id,
                        "type": "function",
                        "function": {"name": name, "arguments": arguments},
                    }],
                },
                "finish_reason": "tool_calls",
            }],
        })
    }

    /// 在 mock 服务器上挂载一条 SSE 响应。
    ///
    /// 多次调用时,wiremock 按挂载顺序(FIFO)匹配;`up_to_n_times(1)` 让每条
    /// mock 只响应一次,从而实现「第 N 次请求返回第 N 条响应」的顺序语义。
    async fn mount_sse(server: &MockServer, chunks: Vec<serde_json::Value>) {
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(sse_body(&chunks), "text/event-stream"),
            )
            .up_to_n_times(1)
            .mount(server)
            .await;
    }

    /// 挂载一条不重试的 OpenAI 兼容请求错误。
    async fn mount_request_error(server: &MockServer, message: &str) {
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": {
                    "message": message,
                    "type": "invalid_request_error",
                    "param": null,
                    "code": "test_error",
                }
            })))
            .up_to_n_times(1)
            .mount(server)
            .await;
    }

    /// 一个总是返回固定成功结果的工具覆盖处理器,用于测试工具调用路径。
    struct EchoTool {
        invocations: Arc<Mutex<Vec<ToolCall>>>,
    }

    impl ToolOverrideHandler for EchoTool {
        fn handle(
            &self,
            call: &ToolCall,
            _session: &mut Session,
            _actor_id: &str,
        ) -> Pin<Box<dyn Future<Output = Option<ToolResult>> + Send>> {
            let name = call.name.clone();
            self.invocations.lock().unwrap().push(call.clone());
            Box::pin(async move {
                Some(ToolResult {
                    ok: true,
                    summary: format!("{name} 已执行"),
                    stdout: "done".to_string(),
                    stderr: String::new(),
                    exit_code: 0,
                    execution: None,
                })
            })
        }
    }

    /// 同批工具全部启动后一直等待取消，用于确认 Cancel 会终止整个并行批次。
    struct BlockingBatchTool {
        barrier: Arc<Barrier>,
        all_started: Arc<Notify>,
    }

    impl ToolOverrideHandler for BlockingBatchTool {
        fn handle(
            &self,
            _call: &ToolCall,
            _session: &mut Session,
            _actor_id: &str,
        ) -> Pin<Box<dyn Future<Output = Option<ToolResult>> + Send>> {
            let barrier = self.barrier.clone();
            let all_started = self.all_started.clone();
            Box::pin(async move {
                if barrier.wait().await.is_leader() {
                    all_started.notify_one();
                }
                std::future::pending::<Option<ToolResult>>().await
            })
        }
    }

    struct ParallelTool {
        barrier: Arc<Barrier>,
        completed: Arc<Mutex<Vec<String>>>,
    }

    impl ToolOverrideHandler for ParallelTool {
        fn handle(
            &self,
            call: &ToolCall,
            _session: &mut Session,
            _actor_id: &str,
        ) -> Pin<Box<dyn Future<Output = Option<ToolResult>> + Send>> {
            let name = call.name.clone();
            let barrier = self.barrier.clone();
            let completed = self.completed.clone();
            Box::pin(async move {
                barrier.wait().await;
                let delay_ms = if name == "slow_probe" { 100 } else { 10 };
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                completed.lock().unwrap().push(name.clone());
                Some(ToolResult {
                    ok: true,
                    summary: format!("{name} 已完成"),
                    stdout: name,
                    stderr: String::new(),
                    exit_code: 0,
                    execution: None,
                })
            })
        }
    }

    struct FailingTool;

    impl ToolOverrideHandler for FailingTool {
        fn handle(
            &self,
            _call: &ToolCall,
            _session: &mut Session,
            _actor_id: &str,
        ) -> Pin<Box<dyn Future<Output = Option<ToolResult>> + Send>> {
            Box::pin(async {
                Some(ToolResult {
                    ok: false,
                    summary: "测试工具执行失败".to_string(),
                    stdout: String::new(),
                    stderr: "test failure".to_string(),
                    exit_code: 1,
                    execution: None,
                })
            })
        }
    }

    struct TrustTrackingPlugin {
        modes: Arc<Mutex<Vec<TrustMode>>>,
    }

    impl ToolOverrideHandler for TrustTrackingPlugin {}
    impl ToolSpecProvider for TrustTrackingPlugin {}
    impl PromptSectionProvider for TrustTrackingPlugin {}

    impl Plugin for TrustTrackingPlugin {
        fn id(&self) -> &str {
            "trust-tracker"
        }

        fn set_trust_mode(&self, trust: TrustMode) {
            self.modes.lock().unwrap().push(trust);
        }
    }

    fn tool_spec(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.to_string(),
            description: format!("测试工具 {name}"),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        }
    }

    /// 构造一个指向 mock 服务器的 ModelEndpoint。
    fn endpoint(server: &MockServer) -> ModelEndpoint {
        ModelEndpoint {
            base_url: server.uri(),
            api_key: "test-key".to_string(),
            model: "test-model".to_string(),
            protocol: ProviderProtocol::OpenAiChatCompletions,
            timeout_ms: 5_000,
            options: serde_json::Value::Object(serde_json::Map::new()),
        }
    }

    /// 测试用的 TurnContext 构造辅助。
    struct TestHarness {
        ctx: TurnContext,
        stream_rx: std::sync::mpsc::Receiver<StreamEvent>,
        cmd_tx: tokio::sync::mpsc::UnboundedSender<Command>,
        cmd_rx: tokio::sync::mpsc::UnboundedReceiver<Command>,
    }

    impl TestHarness {
        /// `extra_overrides` / `tools` 用于工具调用路径测试。
        fn new(
            server: &MockServer,
            tools: Vec<ToolSpec>,
            tool_overrides: HashMap<String, Arc<dyn ToolOverrideHandler>>,
        ) -> Self {
            let root = tempfile::tempdir().expect("创建临时目录失败");
            let mut session = Session::new("test-session".to_string());
            session.bind_storage_root(root.path());
            session.append_message(MessageRole::User, "你好");
            session.rebuild_system_prompt(&SystemPromptConfig::from_plugin_sections(Vec::new()));
            // 让 tempdir 存活到 turn 结束(用 `leak` 避免 Rust 借用检查器抱怨;
            // 测试进程结束即回收)。
            std::mem::forget(root);

            let agent_config = AgentConfig {
                reasoning_effort: "none".to_string(),
                ..Default::default()
            };
            let client = SingleProviderClient::new(endpoint(server));
            let (stream_tx, stream_rx) = std::sync::mpsc::channel::<StreamEvent>();
            let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<Command>();

            let ctx = TurnContext::builder()
                .client(client)
                .session(session)
                .stream_tx(stream_tx)
                .plugins(Vec::new())
                .context_limit(200_000)
                .agent_config(agent_config)
                .trust_mode(TrustMode::FullTrust)
                .observer(Observer::new(std::env::temp_dir()))
                .tool_overrides(tool_overrides)
                .tools(tools)
                .build();

            Self {
                ctx,
                stream_rx,
                cmd_tx,
                cmd_rx,
            }
        }

        /// 排空 stream 通道里的所有积压事件(非阻塞),避免 channel 满导致 send 阻塞。
        fn drain_stream(&self) {
            while self.stream_rx.try_recv().is_ok() {}
        }
    }

    /// 首轮纯文本响应应直接作为最终回复,返回 `Success`(跳过总结阶段)。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn completes_with_direct_text_answer() {
        let server = MockServer::start().await;
        // 单次请求:纯文本 "你好,我是测试助手。",首轮无工具 → can_promote_direct_answer。
        mount_sse(
            &server,
            vec![text_delta_chunk("你好,我是测试助手。"), usage_chunk(10, 5)],
        )
        .await;

        let mut harness = TestHarness::new(&server, Vec::new(), HashMap::new());
        let result: TurnExecutionResult = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;

        assert!(
            matches!(result.outcome, TurnExecutionOutcome::Success),
            "纯文本直接回答应返回 Success,实际: {:?}",
            result.outcome
        );
        assert_eq!(result.usage.total_tokens, 15);
        assert!(
            !harness
                .stream_rx
                .try_iter()
                .any(|event| matches!(event, StreamEvent::DeferredToolInjectionsChanged { .. }))
        );
    }

    /// 发送 `Command::Cancel` 应中断执行并返回 `Cancelled`(覆盖本次重构的
    /// oneshot 取消传播路径)。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn returns_cancelled_on_cancel_command() {
        let server = MockServer::start().await;
        // 挂一条延迟 2s 的响应,确保 cancel 能在模型请求完成前到达。
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(
                        sse_body(&[text_delta_chunk("正在思考")]),
                        "text/event-stream",
                    )
                    .set_delay(std::time::Duration::from_secs(2)),
            )
            .mount(&server)
            .await;

        let mut harness = TestHarness::new(&server, Vec::new(), HashMap::new());
        let trust_modes = Arc::new(Mutex::new(Vec::new()));
        harness.ctx.plugins.push(Arc::new(TrustTrackingPlugin {
            modes: trust_modes.clone(),
        }));

        // cmd_tx 是 Send 的,可以移到独立任务里延时发送 Cancel;
        // 主任务独占 ctx + cmd_rx 跑 execute_turn。
        let cmd_tx = harness.cmd_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let _ = cmd_tx.send(Command::SetTrustMode(TrustMode::Supervised));
            let _ = cmd_tx.send(Command::InjectTool {
                tool_name: "cancelled_probe".to_string(),
                payload: serde_json::json!({"value": 1}),
            });
            let _ = cmd_tx.send(Command::Cancel);
        });

        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;

        assert!(
            matches!(result.outcome, TurnExecutionOutcome::Cancelled),
            "收到 Cancel 应返回 Cancelled,实际: {:?}",
            result.outcome
        );
        assert_eq!(*trust_modes.lock().unwrap(), vec![TrustMode::Supervised]);
        assert_eq!(harness.ctx.trust_mode, TrustMode::Supervised);
        assert_eq!(harness.ctx.session.trust_mode, TrustMode::Supervised);
        assert!(harness.ctx.session.deferred_tool_injections.is_empty());
        assert!(harness.ctx.session.messages.iter().any(|message| {
            message.role == MessageRole::Tool && message.text_content().contains("cancelled_probe")
        }));
        let injection_snapshots = harness
            .stream_rx
            .try_iter()
            .filter_map(|event| match event {
                StreamEvent::DeferredToolInjectionsChanged { injections } => Some(injections),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(injection_snapshots.len(), 2);
        assert_eq!(injection_snapshots[0].len(), 1);
        assert!(injection_snapshots[1].is_empty());
    }

    /// 工具调用路径:模型先调用工具 → 工具执行 → 模型给出非最终回答(问号结尾)→
    /// 进入总结阶段 → 总结完成 → `Success`。
    ///
    /// 覆盖 ReAct loop 多轮 + summary 阶段的完整链路。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn runs_tool_then_completes_via_summary() {
        let server = MockServer::start().await;
        let invocations = Arc::new(Mutex::new(Vec::new()));

        let mut overrides: HashMap<String, Arc<dyn ToolOverrideHandler>> = HashMap::new();
        overrides.insert(
            "echo".to_string(),
            Arc::new(EchoTool {
                invocations: invocations.clone(),
            }),
        );
        let tools = vec![ToolSpec {
            name: "echo".to_string(),
            description: "回显输入".to_string(),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        }];

        // 按请求顺序挂载响应(wiremock FIFO:先挂载的先匹配;up_to_n_times(1)
        // 让每条 mock 只响应一次,从而实现顺序响应)。
        // 1) 首轮:工具调用 echo。
        mount_sse(
            &server,
            vec![tool_call_chunk("call_1", "echo", "{}"), usage_chunk(15, 3)],
        )
        .await;
        // 2) 工具执行后第二轮:文本以问号结尾 → looks_like_final_answer=false → EnterSummary。
        mount_sse(
            &server,
            vec![
                text_delta_chunk("结果还需要我做什么吗?"),
                usage_chunk(25, 5),
            ],
        )
        .await;
        // 3) 总结阶段:纯文本 → SummaryDecision::Done → Completed。
        mount_sse(
            &server,
            vec![text_delta_chunk("已完成。"), usage_chunk(30, 4)],
        )
        .await;

        let mut harness = TestHarness::new(&server, tools, overrides);
        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;

        assert!(
            matches!(result.outcome, TurnExecutionOutcome::Success),
            "工具+总结完整链路应返回 Success,实际: {:?}",
            result.outcome
        );
        // 工具应被调用一次。
        assert_eq!(
            invocations.lock().unwrap().len(),
            1,
            "echo 工具应被调用一次"
        );
        harness.drain_stream();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn runs_tool_batch_in_parallel_and_persists_each_completion() {
        let server = MockServer::start().await;
        mount_sse(
            &server,
            vec![
                tool_call_chunk("call_1_slow", "slow_probe", "{}"),
                tool_call_chunk("call_2_fast", "fast_probe", "{}"),
                usage_chunk(15, 3),
            ],
        )
        .await;
        mount_sse(
            &server,
            vec![
                text_delta_chunk("两个并行工具均已完成，结果已经保存。"),
                usage_chunk(20, 4),
            ],
        )
        .await;

        let completed = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<dyn ToolOverrideHandler> = Arc::new(ParallelTool {
            barrier: Arc::new(Barrier::new(2)),
            completed: completed.clone(),
        });
        let mut overrides = HashMap::new();
        overrides.insert("slow_probe".to_string(), handler.clone());
        overrides.insert("fast_probe".to_string(), handler);
        let harness = TestHarness::new(
            &server,
            vec![tool_spec("slow_probe"), tool_spec("fast_probe")],
            overrides,
        );
        let TestHarness {
            mut ctx,
            stream_rx,
            mut cmd_rx,
            ..
        } = harness;
        let storage_root = ctx
            .session
            .bound_storage_root()
            .expect("测试 Session 必须绑定存储目录")
            .to_path_buf();
        let session_id = ctx.session.id.clone();
        let first_result_task = tokio::task::spawn_blocking(move || {
            loop {
                let event = stream_rx
                    .recv_timeout(Duration::from_secs(5))
                    .expect("等待首个并行工具结果超时");
                if let StreamEvent::ToolResult {
                    name, tool_call_id, ..
                } = event
                {
                    return (name, tool_call_id, stream_rx);
                }
            }
        });

        let mut execution = Box::pin(execute_turn(&mut ctx, &mut cmd_rx));
        let (first_name, first_call_id, stream_rx) = tokio::select! {
            event = first_result_task => event.expect("首个工具结果监听任务失败"),
            result = &mut execution => panic!("全部工具完成前不应结束 turn：{:?}", result.outcome),
        };
        assert_eq!(first_name, "fast_probe");
        assert_eq!(first_call_id.as_deref(), Some("call_2_fast"));

        // ToolResult 事件发出前已经完成落盘，此时慢工具仍在运行。
        let persisted = Session::load_from_storage(&storage_root, &session_id)
            .expect("首个工具完成后应能从磁盘恢复 Session");
        let persisted_results = persisted
            .messages
            .iter()
            .filter_map(|message| message.tool_call_id.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(persisted_results, vec!["call_2_fast"]);

        let result = execution.await;
        assert!(matches!(result.outcome, TurnExecutionOutcome::Success));
        assert_eq!(
            *completed.lock().unwrap(),
            vec!["fast_probe".to_string(), "slow_probe".to_string()]
        );
        let session_results = ctx
            .session
            .messages
            .iter()
            .filter_map(|message| message.tool_call_id.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(session_results, vec!["call_2_fast", "call_1_slow"]);
        assert!(stream_rx.try_iter().any(|event| matches!(
            event,
            StreamEvent::ToolResult { name, .. } if name == "slow_probe"
        )));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn returns_failed_when_react_request_fails() {
        let server = MockServer::start().await;
        mount_request_error(&server, "execute turn request rejected").await;

        let mut harness = TestHarness::new(&server, Vec::new(), HashMap::new());
        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;

        let TurnExecutionOutcome::Failed(message) = &result.outcome else {
            panic!("模型请求失败应返回 Failed，实际: {:?}", result.outcome);
        };
        assert!(!message.is_empty(), "模型请求失败必须返回错误原因");
        assert!(
            harness
                .ctx
                .session
                .messages
                .iter()
                .any(|session_message| session_message.text_content().contains(message))
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handles_runtime_feedback_while_request_is_running() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(
                        sse_body(&[text_delta_chunk("运行时命令已处理。"), usage_chunk(10, 5)]),
                        "text/event-stream",
                    )
                    .set_delay(Duration::from_millis(500)),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        mount_sse(
            &server,
            vec![text_delta_chunk("插件结果已处理。"), usage_chunk(6, 2)],
        )
        .await;

        let harness = TestHarness::new(&server, Vec::new(), HashMap::new());
        let TestHarness {
            mut ctx,
            stream_rx,
            cmd_tx,
            mut cmd_rx,
        } = harness;
        let runtime_cmd_tx = cmd_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            runtime_cmd_tx
                .send(Command::InjectTool {
                    tool_name: "runtime_probe".to_string(),
                    payload: serde_json::json!({"value": 1}),
                })
                .unwrap();
            runtime_cmd_tx
                .send(Command::EmitStreamEvent(Box::new(
                    StreamEvent::AgentNotification {
                        agent_id: "runtime-probe".to_string(),
                        agent_label: "测试".to_string(),
                        content: "命令已转发".to_string(),
                        level: "info".to_string(),
                    },
                )))
                .unwrap();
            runtime_cmd_tx
                .send(Command::ReportUsage {
                    usage: TokenUsage {
                        prompt_tokens: 4,
                        completion_tokens: 3,
                        total_tokens: 7,
                        prompt_cache_hit_tokens: None,
                        prompt_cache_miss_tokens: None,
                    },
                    source: "runtime-probe".to_string(),
                    emit_event: true,
                })
                .unwrap();
        });
        let pending_event_task = tokio::task::spawn_blocking(move || {
            let event = stream_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("模型请求完成前应收到插件待处理快照");
            (event, stream_rx)
        });
        let mut execution = Box::pin(execute_turn(&mut ctx, &mut cmd_rx));
        let (first_event, stream_rx) = tokio::select! {
            event = pending_event_task => event.unwrap(),
            result = &mut execution => panic!("插件待处理快照晚于 turn 结束：{:?}", result.outcome),
        };
        assert!(matches!(
            &first_event,
            StreamEvent::DeferredToolInjectionsChanged { injections }
                if injections.len() == 1 && injections[0].tool_name == "runtime_probe"
        ));

        let result = execution.await;
        drop(cmd_tx);
        assert!(
            matches!(result.outcome, TurnExecutionOutcome::Success),
            "运行时命令处理结果: {:?}",
            result.outcome
        );
        assert_eq!(result.usage.total_tokens, 30);
        assert!(ctx.session.messages.iter().any(|message| {
            message.role == MessageRole::Tool && message.text_content().contains("runtime_probe")
        }));
        assert!(ctx.session.deferred_tool_injections.is_empty());

        let mut events = vec![first_event];
        events.extend(stream_rx.try_iter());
        assert!(events.iter().any(|event| matches!(
            event,
            StreamEvent::AgentNotification { agent_id, .. } if agent_id == "runtime-probe"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            StreamEvent::TokenUsage { source, .. } if source == "runtime-probe"
        )));
        let injection_snapshots = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::DeferredToolInjectionsChanged { injections } => Some(injections),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(injection_snapshots.len(), 2);
        assert_eq!(injection_snapshots[0].len(), 1);
        assert!(injection_snapshots[1].is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejects_supervised_tool_after_approval_response() {
        let server = MockServer::start().await;
        mount_sse(
            &server,
            vec![
                tool_call_chunk("call_reject", "echo", "{}"),
                usage_chunk(8, 2),
            ],
        )
        .await;

        let invocations = Arc::new(Mutex::new(Vec::new()));
        let mut overrides: HashMap<String, Arc<dyn ToolOverrideHandler>> = HashMap::new();
        overrides.insert(
            "echo".to_string(),
            Arc::new(EchoTool {
                invocations: invocations.clone(),
            }),
        );
        let harness = TestHarness::new(&server, vec![tool_spec("echo")], overrides);
        let TestHarness {
            mut ctx,
            stream_rx,
            cmd_tx,
            mut cmd_rx,
        } = harness;
        cmd_tx
            .send(Command::SetTrustMode(TrustMode::Supervised))
            .unwrap();
        let approval_cmd_tx = cmd_tx.clone();
        let approval_task = tokio::task::spawn_blocking(move || {
            loop {
                let event = stream_rx
                    .recv_timeout(Duration::from_secs(5))
                    .expect("等待审批事件超时");
                if let StreamEvent::ApprovalNeeded { request_id, .. } = event {
                    approval_cmd_tx
                        .send(Command::Approval {
                            request_id,
                            approved: false,
                        })
                        .unwrap();
                    break;
                }
            }
        });

        let result = execute_turn(&mut ctx, &mut cmd_rx).await;
        approval_task.await.unwrap();

        assert!(
            matches!(result.outcome, TurnExecutionOutcome::Success),
            "监督模式拒绝后的结果: {:?}",
            result.outcome
        );
        assert_eq!(ctx.trust_mode, TrustMode::Supervised);
        assert!(invocations.lock().unwrap().is_empty());
        assert!(
            ctx.session
                .messages
                .iter()
                .any(|message| message.text_content().contains("用户拒绝执行"))
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn executes_supervised_tool_after_approval_response() {
        let server = MockServer::start().await;
        mount_sse(
            &server,
            vec![
                tool_call_chunk("call_approve", "echo", "{}"),
                usage_chunk(8, 2),
            ],
        )
        .await;
        mount_sse(
            &server,
            vec![text_delta_chunk("已完成。"), usage_chunk(10, 3)],
        )
        .await;

        let invocations = Arc::new(Mutex::new(Vec::new()));
        let mut overrides: HashMap<String, Arc<dyn ToolOverrideHandler>> = HashMap::new();
        overrides.insert(
            "echo".to_string(),
            Arc::new(EchoTool {
                invocations: invocations.clone(),
            }),
        );
        let harness = TestHarness::new(&server, vec![tool_spec("echo")], overrides);
        let TestHarness {
            mut ctx,
            stream_rx,
            cmd_tx,
            mut cmd_rx,
        } = harness;
        cmd_tx
            .send(Command::SetTrustMode(TrustMode::Supervised))
            .unwrap();
        let approval_cmd_tx = cmd_tx.clone();
        let approval_task = tokio::task::spawn_blocking(move || {
            loop {
                let event = stream_rx
                    .recv_timeout(Duration::from_secs(5))
                    .expect("等待审批事件超时");
                if let StreamEvent::ApprovalNeeded { request_id, .. } = event {
                    approval_cmd_tx
                        .send(Command::Approval {
                            request_id,
                            approved: true,
                        })
                        .unwrap();
                    break;
                }
            }
        });

        let result = execute_turn(&mut ctx, &mut cmd_rx).await;
        approval_task.await.unwrap();

        assert!(
            matches!(result.outcome, TurnExecutionOutcome::Success),
            "监督模式批准后的结果: {:?}",
            result.outcome
        );
        assert_eq!(result.usage.total_tokens, 23);
        assert_eq!(invocations.lock().unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn returns_cancelled_when_tool_execution_is_cancelled() {
        let server = MockServer::start().await;
        mount_sse(
            &server,
            vec![
                tool_call_chunk("call_block_1", "blocking_1", "{}"),
                tool_call_chunk("call_block_2", "blocking_2", "{}"),
                usage_chunk(9, 2),
            ],
        )
        .await;

        let all_started = Arc::new(Notify::new());
        let handler: Arc<dyn ToolOverrideHandler> = Arc::new(BlockingBatchTool {
            barrier: Arc::new(Barrier::new(2)),
            all_started: all_started.clone(),
        });
        let mut overrides: HashMap<String, Arc<dyn ToolOverrideHandler>> = HashMap::new();
        overrides.insert("blocking_1".to_string(), handler.clone());
        overrides.insert("blocking_2".to_string(), handler);
        let mut harness = TestHarness::new(
            &server,
            vec![tool_spec("blocking_1"), tool_spec("blocking_2")],
            overrides,
        );
        let cmd_tx = harness.cmd_tx.clone();
        let cancel_task = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(5), all_started.notified())
                .await
                .expect("并行阻塞工具未全部开始执行");
            cmd_tx.send(Command::Cancel).unwrap();
        });

        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;
        cancel_task.await.unwrap();

        assert!(matches!(result.outcome, TurnExecutionOutcome::Cancelled));
        let interrupted_ids = harness
            .ctx
            .session
            .messages
            .iter()
            .filter(|message| {
                message.role == MessageRole::Tool && message.text_content().contains("中断")
            })
            .filter_map(|message| message.tool_call_id.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(interrupted_ids, vec!["call_block_1", "call_block_2"]);
        let app_results = harness
            .stream_rx
            .try_iter()
            .filter_map(|event| match event {
                StreamEvent::ToolResult { tool_call_id, .. } => tool_call_id,
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(app_results, vec!["call_block_1", "call_block_2"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn continues_after_tool_failure_with_recovery_context() {
        let server = MockServer::start().await;
        mount_sse(
            &server,
            vec![
                tool_call_chunk("call_fail", "failing", "{}"),
                usage_chunk(9, 2),
            ],
        )
        .await;
        mount_sse(
            &server,
            vec![text_delta_chunk("已改用其他方案完成。"), usage_chunk(11, 3)],
        )
        .await;

        let mut overrides: HashMap<String, Arc<dyn ToolOverrideHandler>> = HashMap::new();
        overrides.insert("failing".to_string(), Arc::new(FailingTool));
        let mut harness = TestHarness::new(&server, vec![tool_spec("failing")], overrides);

        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;

        assert!(
            matches!(result.outcome, TurnExecutionOutcome::Success),
            "工具失败恢复后的结果: {:?}",
            result.outcome
        );
        assert_eq!(result.usage.total_tokens, 25);
        assert!(
            harness.ctx.session.messages.iter().any(|message| {
                message.tool_name.as_deref() == Some("react_failed_tool_recovery")
            })
        );
        assert!(harness.ctx.session.messages.iter().any(|message| {
            message.role == MessageRole::Tool
                && message.tool_name.as_deref() == Some("failing")
                && message.text_content().contains("test failure")
        }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn returns_cancelled_when_summary_is_cancelled() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(
                        sse_body(&[text_delta_chunk("[DONE]\n不应完成")]),
                        "text/event-stream",
                    )
                    .set_delay(Duration::from_secs(2)),
            )
            .mount(&server)
            .await;

        let mut harness = TestHarness::new(&server, Vec::new(), HashMap::new());
        harness.ctx.max_tool_rounds = 0;
        let cmd_tx = harness.cmd_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cmd_tx.send(Command::Cancel).unwrap();
        });

        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;
        assert!(matches!(result.outcome, TurnExecutionOutcome::Cancelled));
        assert!(harness.stream_rx.try_iter().any(|event| matches!(
            event,
            StreamEvent::PhaseChanged { phase, .. } if phase == "summary"
        )));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reenters_agent_loop_when_summary_needs_more_work() {
        let server = MockServer::start().await;
        mount_sse(
            &server,
            vec![text_delta_chunk("[调用工具:测试]"), usage_chunk(10, 1)],
        )
        .await;
        mount_sse(
            &server,
            vec![
                text_delta_chunk("[NEED_MORE_WORK]\n继续处理剩余步骤"),
                usage_chunk(12, 2),
            ],
        )
        .await;
        mount_sse(
            &server,
            vec![text_delta_chunk("继续执行后的结果"), usage_chunk(20, 3)],
        )
        .await;
        mount_sse(
            &server,
            vec![text_delta_chunk("[DONE]\n任务完成"), usage_chunk(24, 4)],
        )
        .await;

        let mut harness = TestHarness::new(&server, Vec::new(), HashMap::new());
        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;

        assert!(
            matches!(result.outcome, TurnExecutionOutcome::Success),
            "总结重入结果: {:?}",
            result.outcome
        );
        assert_eq!(result.usage.total_tokens, 76);
        let summary_phases = harness
            .stream_rx
            .try_iter()
            .filter(|event| {
                matches!(
                    event,
                    StreamEvent::PhaseChanged { phase, .. } if phase == "summary"
                )
            })
            .count();
        assert_eq!(summary_phases, 2);
        assert!(harness.ctx.session.messages.iter().any(|message| {
            message.text_content().contains("summary_need_more_work")
                || message.text_content().contains("继续处理剩余步骤")
        }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reenters_agent_loop_when_plugin_result_arrives_during_summary() {
        let server = MockServer::start().await;
        mount_sse(
            &server,
            vec![text_delta_chunk("[调用工具:测试]"), usage_chunk(10, 1)],
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(
                        sse_body(&[text_delta_chunk("[DONE]\n旧总结"), usage_chunk(12, 2)]),
                        "text/event-stream",
                    )
                    .set_delay(Duration::from_millis(500)),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        mount_sse(
            &server,
            vec![text_delta_chunk("新插件结果已处理。"), usage_chunk(20, 3)],
        )
        .await;

        let harness = TestHarness::new(&server, Vec::new(), HashMap::new());
        let TestHarness {
            mut ctx,
            stream_rx,
            cmd_tx,
            mut cmd_rx,
        } = harness;
        let injection_tx = cmd_tx.clone();
        let injection_task = tokio::task::spawn_blocking(move || {
            loop {
                let event = stream_rx
                    .recv_timeout(Duration::from_secs(5))
                    .expect("等待总结阶段事件超时");
                if matches!(
                    event,
                    StreamEvent::PhaseChanged { ref phase, .. } if phase == "summary"
                ) {
                    injection_tx
                        .send(Command::InjectTool {
                            tool_name: "summary_probe".to_string(),
                            payload: serde_json::json!({"value": 1}),
                        })
                        .unwrap();
                    break;
                }
            }
            stream_rx
        });

        let result = execute_turn(&mut ctx, &mut cmd_rx).await;
        let stream_rx = injection_task.await.unwrap();
        drop(cmd_tx);

        assert!(matches!(result.outcome, TurnExecutionOutcome::Success));
        assert_eq!(result.usage.total_tokens, 48);
        assert!(ctx.session.messages.iter().any(|message| {
            message.role == MessageRole::Tool && message.text_content().contains("summary_probe")
        }));
        let injection_snapshots = stream_rx
            .try_iter()
            .filter_map(|event| match event {
                StreamEvent::DeferredToolInjectionsChanged { injections } => Some(injections),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(injection_snapshots.len(), 2);
        assert_eq!(injection_snapshots[0].len(), 1);
        assert!(injection_snapshots[1].is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn forces_final_response_when_outer_iteration_limit_is_reached() {
        let server = MockServer::start().await;
        mount_sse(
            &server,
            vec![text_delta_chunk("[调用工具:测试]"), usage_chunk(5, 1)],
        )
        .await;
        mount_sse(
            &server,
            vec![
                text_delta_chunk("[NEED_MORE_WORK]\n仍有一步"),
                usage_chunk(7, 2),
            ],
        )
        .await;
        mount_sse(
            &server,
            vec![text_delta_chunk("强制收尾完成"), usage_chunk(8, 3)],
        )
        .await;

        let mut harness = TestHarness::new(&server, Vec::new(), HashMap::new());
        harness.ctx.max_outer_iterations = 1;
        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;

        assert!(
            matches!(result.outcome, TurnExecutionOutcome::Success),
            "外层上限强制收尾结果: {:?}",
            result.outcome
        );
        assert_eq!(result.usage.total_tokens, 26);
        assert!(
            harness
                .ctx
                .session
                .messages
                .iter()
                .any(|message| message.text_content().contains("强制收尾完成"))
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn forces_final_response_after_summary_request_fails() {
        let server = MockServer::start().await;
        mount_sse(
            &server,
            vec![text_delta_chunk("[调用工具:测试]"), usage_chunk(5, 1)],
        )
        .await;
        mount_request_error(&server, "summary request rejected").await;
        mount_request_error(&server, "summary request rejected").await;
        mount_sse(
            &server,
            vec![text_delta_chunk("总结失败后完成收尾"), usage_chunk(8, 3)],
        )
        .await;

        let mut harness = TestHarness::new(&server, Vec::new(), HashMap::new());
        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;

        assert!(
            matches!(result.outcome, TurnExecutionOutcome::Success),
            "总结失败兜底结果: {:?}",
            result.outcome
        );
        assert_eq!(result.usage.total_tokens, 17);
        assert!(
            harness
                .ctx
                .session
                .messages
                .iter()
                .any(|message| message.text_content().contains("总结阶段失败"))
        );
        assert!(
            harness
                .ctx
                .session
                .messages
                .iter()
                .any(|message| message.text_content().contains("总结失败后完成收尾"))
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn returns_failed_when_summary_and_force_final_both_fail() {
        let server = MockServer::start().await;
        mount_sse(
            &server,
            vec![text_delta_chunk("[调用工具:测试]"), usage_chunk(5, 1)],
        )
        .await;
        mount_request_error(&server, "summary stream rejected").await;
        mount_request_error(&server, "summary fallback rejected").await;
        mount_request_error(&server, "force final stream rejected").await;
        mount_request_error(&server, "force final fallback rejected").await;

        let mut harness = TestHarness::new(&server, Vec::new(), HashMap::new());
        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;

        let TurnExecutionOutcome::Failed(message) = result.outcome else {
            panic!("总结与强制收尾都失败时必须返回 Failed");
        };
        assert!(message.contains("总结阶段失败"));
        assert!(message.contains("强制最终回复失败"));
        assert_eq!(result.usage.total_tokens, 6);
    }
}
