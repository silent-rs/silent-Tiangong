//! 单轮 Agent Loop 的执行过程。
//!
//! 本模块只负责从已构建的 TurnContext 执行模型请求、工具调用与总结阶段；
//! turn 的插件生命周期、状态提交和最终持久化由 react/turn.rs 负责。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use tokio::sync::mpsc as tokio_mpsc;
use tokio::task::JoinSet;

use crate::context::organizer::ContextOrganizer;
use crate::core::command::Command;
use crate::core::plugin::Plugin;
use crate::formatting::{format_llm_output_message, format_tool_trace_message};
use crate::model::{
    InvalidToolCall, ModelFunctionResponse, ModelRequest, TokenUsage, ToolCall, ToolChoice,
    ToolSpec,
};
use crate::permission::TrustMode;
use crate::react::context::{
    build_thinking_config, emit_token_usage, persist_error, rebuild_system_prompt,
    select_client_for_request,
};
use crate::react::message::*;
use crate::runtime::LlmOutputRecord;
use crate::session::{Message, MessagePhase, MessageRole};
use crate::stream_throttle::{StreamTextKind, ThrottledStreamSink};
use crate::tool::ToolResult;
use crate::turn_context::TurnContext;
use tiangong_types::{DeferredToolInjection, StreamEvent, StreamToolCall};

use super::cancel::{abort_and_join, emit_cancel_usage};
use super::compression::{
    ActiveCompression, CompressionContinuation, ReactTextDisposition, observed_total_tokens,
};
use super::helpers::{looks_like_final_answer, record_plugin_usage};
use super::outcome::TurnExecutionResult;
use super::phase::{
    ActiveLlm, ApprovalPhase, CompressingPhase, ExecutionBudget, ExecutionPhase, LlmPurpose,
    PendingApproval, PreparedToolCall, RunningToolCall, ToolBatchState, ToolExecutionPhase,
    ToolTaskOutput,
};
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
    stage: String,
) -> Vec<ToolCall> {
    let calls = response.tool_calls.clone();
    debug_assert!(!calls.is_empty(), "工具批次不能为空");

    let tool_names = calls
        .iter()
        .map(|call| call.name.clone())
        .collect::<Vec<_>>();
    let output = LlmOutputRecord {
        stage,
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

fn append_invalid_tool_calls_context(ctx: &mut TurnContext, invalid_calls: &[InvalidToolCall]) {
    if invalid_calls.is_empty() {
        return;
    }

    let calls = invalid_calls
        .iter()
        .map(|call| {
            serde_json::json!({
                "id": call.id,
                "name": call.name,
                "arguments": call.arguments,
                "validation_error": call.reason,
            })
        })
        .collect::<Vec<_>>();
    inject_tool_to_messages(
        &mut ctx.session,
        "invalid_tool_calls",
        &serde_json::json!({
            "calls": calls,
            "instruction": "以上工具调用已从执行列表剔除，其他合法调用不受影响。请根据校验错误修正参数，并在下一轮按需生成新的工具调用；不要复述校验错误。",
        }),
    );
    ctx.session.persist_to_disk();
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

/// Agent Loop 状态：单一 `ExecutionPhase`（take/install 所有权模式，见 design.md
/// 3.1）+ 预算 + 累计用量 + 工具去重记录。阶段持有的活动资源（模型请求/工具
/// 任务/审批/压缩）都在 phase 变体内，不再有并列活动 `Option`（ALR-001）。
struct AgentLoopState {
    phase: Option<ExecutionPhase>,
    budget: ExecutionBudget,
    accumulated_usage: TokenUsage,
    tool_history: ToolCallHistory,
}

impl AgentLoopState {
    fn new() -> Self {
        Self {
            phase: Some(ExecutionPhase::NeedModel),
            budget: ExecutionBudget::default(),
            accumulated_usage: TokenUsage::default(),
            tool_history: ToolCallHistory::default(),
        }
    }

    /// 取出当前阶段（ALR-205：取出后所有退出路径必须 install 新阶段或形成终态）。
    fn take_phase(&mut self) -> ExecutionPhase {
        self.phase.take().expect("take_phase 时阶段必须存在")
    }

    /// 安装新阶段。install 前 phase 必须为 None（已 take），避免双阶段并存。
    fn install_phase(&mut self, phase: ExecutionPhase) {
        debug_assert!(
            self.phase.is_none(),
            "install_phase 前必须先 take，避免双阶段并存"
        );
        self.phase = Some(phase);
    }

    fn reset_react_phase(&mut self) {
        self.budget.reset_react_phase();
        self.tool_history.clear();
    }
}

// 阶段数据类型（ToolBatchState / PreparedToolCall / PendingApproval /
// RunningToolCall / ToolTaskOutput / LlmPurpose / ActiveLlm 及
// ToolExecutionPhase / ApprovalPhase）统一定义在 super::phase。

fn build_react_request(ctx: &TurnContext) -> ModelRequest {
    let (thinking, reasoning_effort, thinking_disabled) = build_thinking_config(ctx);
    ModelRequest {
        user_input: String::new(),
        context: ctx.session.context(),
        thinking,
        reasoning_effort,
        thinking_disabled,
        max_output_tokens: None,
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
            stage: format!("react-round-{}", state.budget.request_round),
            content: response.text.clone(),
            reasoning_content: response.reasoning_content.clone(),
            tool_calls: Vec::new(),
            usage: response.usage.clone(),
        }),
        response.reasoning_content.clone(),
    );
    ctx.session.persist_to_disk();

    let direct_answer = state.budget.continuation_count == 0
        && !state.budget.executed_tool_in_phase
        && !response.text.trim().is_empty();
    let tool_answer =
        state.budget.executed_tool_in_phase && looks_like_final_answer(&response.text);
    if direct_answer || tool_answer {
        ReactTextDisposition::Complete
    } else {
        ReactTextDisposition::EnterSummary
    }
}

fn start_tool_execution(
    ctx: &mut TurnContext,
    tool: PreparedToolCall,
    tools: &mut ToolExecutionPhase,
) {
    let _ = ctx.stream_tx.send(StreamEvent::ToolStart {
        name: tool.call.name.clone(),
        args_summary: tool.args_summary.clone(),
    });
    let actor_id = ctx.session.id.clone();
    let future = start_tool_call(ctx, &tool.call, &actor_id);
    let started_at = std::time::Instant::now();
    let task = tools.tasks.spawn(async move {
        let result = future.await;
        ToolTaskOutput {
            result,
            duration_ms: started_at.elapsed().as_millis() as u64,
        }
    });
    tools
        .running
        .insert(task.id(), RunningToolCall { tool, started_at });
}

fn finish_react_text(
    ctx: &mut TurnContext,
    state: &mut AgentLoopState,
    injections: &mut ToolInjectionBuffer,
    pending_msg_id: String,
    disposition: ReactTextDisposition,
    request_injection_generation: u64,
) -> ExecutionPhase {
    let received_new_injection = injections.generation() > request_injection_generation;
    injections.commit(ctx);
    if received_new_injection {
        return ExecutionPhase::NeedModel;
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
            ExecutionPhase::PendingFinish(TurnExecutionResult::success(
                state.accumulated_usage.clone(),
            ))
        }
        ReactTextDisposition::EnterSummary => ExecutionPhase::StartCheckingCompletion,
    }
}

fn finish_summary(
    ctx: &mut TurnContext,
    state: &mut AgentLoopState,
    injections: &mut ToolInjectionBuffer,
    decision: SummaryDecision,
    request_injection_generation: u64,
) -> ExecutionPhase {
    ctx.session.persist_to_disk();
    let received_new_injection = injections.generation() > request_injection_generation;
    injections.commit(ctx);
    if received_new_injection {
        state.reset_react_phase();
        return ExecutionPhase::NeedModel;
    }

    match decision {
        SummaryDecision::Done(_) | SummaryDecision::AskUser(_) => ExecutionPhase::PendingFinish(
            TurnExecutionResult::success(state.accumulated_usage.clone()),
        ),
        SummaryDecision::NeedMoreWork(reason) => {
            state.budget.continuation_count += 1;
            if state.budget.continuation_count >= ctx.max_outer_iterations {
                return ExecutionPhase::StartForceFinal {
                    reason: ForceFinalReason::OuterLimit,
                    request_injection_generation,
                    summary_error: None,
                };
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
            ExecutionPhase::NeedModel
        }
    }
}

/// 持久化被中断的 LLM 请求已流式收到的部分输出。
///
/// Summary 的部分输出降级为 React 过程消息（ALR-104）——半截总结不得保持最终
/// Summary 身份；ForceFinal 无可靠完整内容，丢弃不提交。
fn persist_interrupted_llm_output(
    ctx: &mut TurnContext,
    purpose: &LlmPurpose,
    pending_msg_id: &str,
    streamed_text: &str,
    streamed_reasoning: &str,
) {
    match purpose {
        LlmPurpose::React { .. } | LlmPurpose::Summary { .. } => {
            persist_streamed_react_message(ctx, pending_msg_id, streamed_text, streamed_reasoning);
        }
        LlmPurpose::ForceFinal { .. } => {}
    }
}

/// 中断主循环直接拥有的活动（阶段感知）：消费当前阶段持有的资源并完成收尾。
///
/// `reason` 区分两种中断语义：
/// - 引导消息（ALR-101）：Summary 部分输出**降级**为 React 过程消息（ALR-104）；
/// - 取消/关闭：Summary 部分输出按取消路径持久化（保持 Summary 身份，与旧行为
///   一致），工具/压缩/审批处理相同。
///
/// 两者都**不取消插件独立持有的后台任务**（ALR-103）。中断后安装 `NeedModel`
/// 之外的目标阶段由调用方决定；本函数保证阶段资源全部转移、取消或完成（ALR-205）。
async fn interrupt_active_work(
    ctx: &mut TurnContext,
    state: &mut AgentLoopState,
    injections: &mut ToolInjectionBuffer,
    stream_tx: &std::sync::mpsc::Sender<StreamEvent>,
    context_limit: usize,
    downgrade_summary: bool,
) {
    let phase = state.take_phase();
    match phase {
        ExecutionPhase::WaitingModel(active)
        | ExecutionPhase::CheckingCompletion(active)
        | ExecutionPhase::ForceFinalPhase(active) => {
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
            if downgrade_summary {
                persist_interrupted_llm_output(
                    ctx,
                    &purpose,
                    &pending_msg_id,
                    &streamed_text,
                    &streamed_reasoning,
                );
            } else {
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
            }
            emit_cancel_usage(stream_tx, &streaming_usage, context_limit);
            state.accumulated_usage.accumulate(&streaming_usage);
        }
        ExecutionPhase::WaitingTools(mut tools) => {
            tools.tasks.shutdown().await;
            let mut interrupted = tools
                .running
                .drain()
                .map(|(_, tool)| tool)
                .collect::<Vec<_>>();
            interrupted.sort_by_key(|running| running.tool.index);
            let mut interrupted_events = Vec::with_capacity(interrupted.len());
            for running in interrupted {
                let duration_ms = running.started_at.elapsed().as_millis() as u64;
                let output = "工具调用因用户发送新消息而中断。".to_string();
                append_tool_result_message(
                    &mut ctx.session,
                    &running.tool.call.id,
                    &running.tool.call.name,
                    output.clone(),
                    true,
                );
                interrupted_events.push(StreamEvent::ToolResult {
                    name: running.tool.call.name,
                    tool_call_id: Some(running.tool.call.id),
                    ok: false,
                    output,
                    full_output: None,
                    duration_ms: Some(duration_ms),
                });
            }
            ctx.session.persist_to_disk();
            for event in interrupted_events {
                let _ = stream_tx.send(event);
            }
            // 批次中尚未执行的调用一并闭合。
            let closed = ctx
                .session
                .close_unfinished_tool_calls_with_reason("工具调用因用户发送新消息而中断。");
            for (tool_call_id, tool_name, output) in closed {
                let _ = stream_tx.send(StreamEvent::ToolResult {
                    name: tool_name,
                    tool_call_id: Some(tool_call_id),
                    ok: false,
                    output,
                    full_output: None,
                    duration_ms: None,
                });
            }
        }
        ExecutionPhase::Compressing(compressing) => {
            compressing.active.cancel(ctx).await;
        }
        ExecutionPhase::WaitingApproval(approval) => {
            record_rejected_tool_call(
                ctx,
                &approval.pending.tool.call,
                &approval.pending.tool.args_summary,
            );
        }
        ExecutionPhase::PreparingTools(_) | ExecutionPhase::PendingFinish(_) => {
            // 无在途活动；PreparingTools 的悬空调用由下方统一闭合。
        }
        ExecutionPhase::NeedModel | ExecutionPhase::StartCheckingCompletion => {}
        ExecutionPhase::StartForceFinal { .. } => {}
    }

    injections.commit(ctx);

    // 闭合残留的未完成 tool calls（模型已返回但工具未开始执行的）。
    let closed = ctx
        .session
        .close_unfinished_tool_calls_with_reason("工具调用因用户发送新消息而中断。");
    for (tool_call_id, tool_name, output) in closed {
        let _ = stream_tx.send(StreamEvent::ToolResult {
            name: tool_name,
            tool_call_id: Some(tool_call_id),
            ok: false,
            output,
            full_output: None,
            duration_ms: None,
        });
    }
}

/// 校验并事务性保存运行中注入的用户消息；成功才向界面确认接收，并重置为新用户
/// 意图（ALR-102：重置阶段预算与工具去重记录，保留物理 turn 累计用量）。
/// 调用前须先 interrupt_active_work；成功后由调用方安装 `NeedModel`。
fn save_user_message_and_restart(
    ctx: &mut TurnContext,
    state: &mut AgentLoopState,
    stream_tx: &std::sync::mpsc::Sender<StreamEvent>,
    message_id: String,
    content: Vec<tiangong_types::ContentBlock>,
) -> Result<(), String> {
    let content_text = tiangong_types::content_blocks_text(&content);
    let content_blocks = tiangong_types::stable_content_blocks(&content);
    let event_message_id = message_id.clone();
    ctx.session
        .try_append_prepared_user_message_with_id(message_id, content)?;
    let _ = stream_tx.send(StreamEvent::UserMessage {
        message_id: event_message_id,
        content: content_text,
        content_blocks,
        media: Vec::new(),
        model_excluded: false,
    });
    tracing::info!(
        session_id = %ctx.session.id,
        "运行中注入用户消息：中断当前执行并追加新消息"
    );
    state.budget.reset_for_new_intent();
    state.tool_history.clear();
    Ok(())
}

/// 执行一个完整的对话轮次。
///
/// `cmd_rx` 只在这一处消费。模型、工具和压缩任务与命令进入同一个
/// `tokio::select!`，因此所有异步阶段都能及时响应取消和运行态反馈。
pub(super) async fn execute_turn(
    ctx: &mut TurnContext,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
) -> TurnExecutionResult {
    let stream_tx = ctx.stream_tx.clone();
    let context_limit = ctx.context_limit;
    let context_organizer = ContextOrganizer::new(context_limit);
    let plugins = ctx.plugins.clone();
    let request_tools = ctx.tools.clone();
    let mut trust_mode = ctx.trust_mode;
    let mut injections = ToolInjectionBuffer::new(ctx);
    let mut state = AgentLoopState::new();
    // Waiting 阶段 select 出的命令：先归还阶段再由统一命令处理接管（命令优先）。
    enum Deferred {
        Command(Command),
        Closed,
    }
    let mut deferred: Option<Deferred> = None;

    let result = 'agent_loop: loop {
        // ── 命令优先（统一处理）──
        // 已到达的命令先于阶段推进处理；Waiting 阶段 select 出的命令经 deferred
        // 在此处接管。Cancel/Shutdown/通道关闭中断当前阶段并终止本轮；其余命令
        // 按阶段语义处理副作用或触发阶段迁移。
        let pending_command = match deferred.take() {
            Some(d) => Some(d),
            None => match cmd_rx.try_recv() {
                Ok(command) => Some(Deferred::Command(command)),
                Err(tokio_mpsc::error::TryRecvError::Empty) => None,
                Err(tokio_mpsc::error::TryRecvError::Disconnected) => Some(Deferred::Closed),
            },
        };
        if let Some(deferred_command) = pending_command {
            let is_cancel = matches!(
                &deferred_command,
                Deferred::Closed
                    | Deferred::Command(Command::Cancel)
                    | Deferred::Command(Command::Shutdown)
            );
            if is_cancel {
                cmd_rx.close();
                // 取消路径：Summary 部分输出保持取消语义（不降级）；插件 on_cancel
                // 由 run_turn 在终态判定后调用。
                interrupt_active_work(
                    ctx,
                    &mut state,
                    &mut injections,
                    &stream_tx,
                    context_limit,
                    false,
                )
                .await;
                break 'agent_loop TurnExecutionResult::cancelled(state.accumulated_usage);
            }
            match deferred_command {
                Deferred::Command(Command::InjectUserMessage {
                    message_id,
                    content,
                }) => {
                    // 引导消息：中断主循环直接拥有的活动（Summary 降级 ALR-104），
                    // 校验并保存，成功才确认，然后从新意图重启（ALR-101/102）。
                    interrupt_active_work(
                        ctx,
                        &mut state,
                        &mut injections,
                        &stream_tx,
                        context_limit,
                        true,
                    )
                    .await;
                    match save_user_message_and_restart(
                        ctx, &mut state, &stream_tx, message_id, content,
                    ) {
                        Ok(()) => state.install_phase(ExecutionPhase::NeedModel),
                        Err(error) => {
                            tracing::warn!(
                                %error,
                                session_id = %ctx.session.id,
                                "运行中注入用户消息保存失败"
                            );
                            break 'agent_loop TurnExecutionResult::failed(
                                state.accumulated_usage.clone(),
                                format!("用户消息保存失败：{error}"),
                            );
                        }
                    }
                }
                Deferred::Command(Command::Approval {
                    request_id,
                    approved,
                }) => {
                    let phase = state.take_phase();
                    match phase {
                        ExecutionPhase::WaitingApproval(approval)
                            if approval.pending.request_id == request_id =>
                        {
                            if approved {
                                let mut tools = ToolExecutionPhase {
                                    tasks: JoinSet::new(),
                                    running: HashMap::new(),
                                    batch: approval.batch,
                                };
                                start_tool_execution(ctx, approval.pending.tool, &mut tools);
                                state.install_phase(ExecutionPhase::WaitingTools(tools));
                            } else {
                                record_rejected_tool_call(
                                    ctx,
                                    &approval.pending.tool.call,
                                    &approval.pending.tool.args_summary,
                                );
                                let request_generation =
                                    approval.batch.request_injection_generation;
                                let received_new_injection =
                                    injections.generation() > request_generation;
                                injections.commit(ctx);
                                if received_new_injection {
                                    state.install_phase(ExecutionPhase::NeedModel);
                                } else {
                                    state.install_phase(ExecutionPhase::PendingFinish(
                                        TurnExecutionResult::success(
                                            state.accumulated_usage.clone(),
                                        ),
                                    ));
                                }
                            }
                        }
                        other => {
                            // 迟到或不匹配的审批：明确忽略，不影响当前阶段。
                            state.install_phase(other);
                        }
                    }
                }
                Deferred::Command(Command::SetTrustMode(mode)) => {
                    set_runtime_trust_mode(&mut trust_mode, &plugins, mode);
                    let phase = state.take_phase();
                    match phase {
                        ExecutionPhase::WaitingApproval(mut approval)
                            if mode == TrustMode::FullTrust =>
                        {
                            let tool = approval.pending.tool;
                            approval.batch.ready_tools.push(tool);
                            state.install_phase(ExecutionPhase::PreparingTools(approval.batch));
                        }
                        other => state.install_phase(other),
                    }
                }
                Deferred::Command(Command::SetReasoningEffort(effort)) => {
                    ctx.agent_config.reasoning_effort = effort.clone();
                    ctx.session.reasoning_effort = Some(effort);
                }
                Deferred::Command(Command::InjectTool { tool_name, payload }) => {
                    injections.receive(&stream_tx, tool_name, payload);
                }
                Deferred::Command(Command::SetTitle {
                    title,
                    only_if_default,
                }) => {
                    if !only_if_default || crate::core::is_default_title(&ctx.session.title) {
                        ctx.session.title = title.clone();
                        ctx.session.updated_at = tiangong_types::now_text();
                        // 通知消费线程转发 sessions_updated（core 层不碰 tauri，走自有 StreamEvent 通道）。
                        let _ = stream_tx.send(tiangong_types::StreamEvent::TitleChanged { title });
                    }
                    // 不立即 persist：turn 结束 run_turn 统一落盘。
                }
                Deferred::Command(Command::EmitStreamEvent(event)) => {
                    let _ = stream_tx.send(*event);
                }
                Deferred::Command(Command::ReportUsage {
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
                Deferred::Command(Command::Cancel | Command::Shutdown) => {
                    unreachable!("Cancel/Shutdown 已在上方取消分支处理")
                }
                Deferred::Closed => unreachable!("Closed 已在上方取消分支处理"),
            }
            continue 'agent_loop;
        }

        // ── 阶段驱动（take → drive → install）──
        let phase = state.take_phase();
        match phase {
            // ── Ready：需要下一次 ReAct 模型请求 ──
            ExecutionPhase::NeedModel => {
                if state.budget.react_rounds_in_phase >= ctx.max_tool_rounds {
                    state.install_phase(ExecutionPhase::StartCheckingCompletion);
                    continue 'agent_loop;
                }
                injections.commit(ctx);
                let request_injection_generation = injections.generation();
                if state.budget.request_round == 0 {
                    debug_assert!(
                        ctx.session.system_prompt_message.is_some(),
                        "TurnContext 构建前应已注入 system prompt"
                    );
                } else {
                    let _ = stream_tx.send(StreamEvent::PhaseChanged {
                        phase: "analyzing".to_string(),
                        iteration: (state.budget.request_round + 1) as u32,
                    });
                }
                let active = start_llm_request(
                    ctx,
                    build_react_request(ctx),
                    LlmPurpose::React {
                        request_injection_generation,
                    },
                    StreamTextKind::React,
                    None,
                );
                state.install_phase(ExecutionPhase::WaitingModel(active));
            }

            // ── Ready（同步过渡）：发起完成度检查请求 ──
            ExecutionPhase::StartCheckingCompletion => {
                injections.commit(ctx);
                let request_injection_generation = injections.generation();
                let iteration = state.budget.continuation_count + 1;
                let _ = stream_tx.send(StreamEvent::PhaseChanged {
                    phase: "summary".to_string(),
                    iteration,
                });
                if ctx.session.system_prompt_message.is_none() {
                    rebuild_system_prompt(ctx);
                }
                let active = start_llm_request(
                    ctx,
                    request_for_summary_phase(&ctx.session),
                    LlmPurpose::Summary {
                        iteration,
                        request_injection_generation,
                    },
                    StreamTextKind::Summary,
                    Some(ToolChoice::None),
                );
                state.install_phase(ExecutionPhase::CheckingCompletion(active));
            }

            // ── Ready（同步过渡）：发起强制最终回复请求 ──
            ExecutionPhase::StartForceFinal {
                reason,
                request_injection_generation,
                summary_error,
            } => {
                let request = build_force_final_request(ctx, reason);
                let active = start_llm_request(
                    ctx,
                    request,
                    LlmPurpose::ForceFinal {
                        request_injection_generation,
                        summary_error,
                    },
                    StreamTextKind::Summary,
                    Some(ToolChoice::None),
                );
                state.install_phase(ExecutionPhase::ForceFinalPhase(active));
            }

            // ── Ready：暂定完成，提交结果（任务 07 接入命令仲裁后此处先排空命令）──
            ExecutionPhase::PendingFinish(result) => break 'agent_loop result,

            // ── Ready（兼容层，任务 05 迁移）：推进工具批次准备/执行 ──
            ExecutionPhase::PreparingTools(mut batch) => {
                let call = batch.calls.pop_front();
                let Some((index, call)) = call else {
                    if !batch.ready_tools.is_empty() {
                        let ready_tools = std::mem::take(&mut batch.ready_tools);
                        let mut tools = ToolExecutionPhase {
                            tasks: JoinSet::new(),
                            running: HashMap::new(),
                            batch,
                        };
                        for tool in ready_tools {
                            start_tool_execution(ctx, tool, &mut tools);
                        }
                        state.install_phase(ExecutionPhase::WaitingTools(tools));
                        continue 'agent_loop;
                    }

                    append_invalid_tool_calls_context(ctx, &batch.invalid_tool_calls);
                    if batch.needs_failure_recovery {
                        append_failure_recovery_prompt(ctx, &state.tool_history, &request_tools);
                    } else {
                        ctx.session.persist_to_disk();
                    }
                    injections.commit(ctx);
                    state.install_phase(ExecutionPhase::NeedModel);
                    continue 'agent_loop;
                };

                match prepare_tool_call(ctx, &call, &mut state.tool_history) {
                    ToolPreflightOutcome::Skip { needs_recovery } => {
                        batch.needs_failure_recovery |= needs_recovery;
                        ctx.session.persist_to_disk();
                        state.install_phase(ExecutionPhase::PreparingTools(batch));
                    }
                    ToolPreflightOutcome::Execute {
                        args_summary,
                        dedupe_key,
                    } => {
                        let first_in_batch = batch.prepared_keys.insert(dedupe_key.clone());
                        if !first_in_batch {
                            record_parallel_duplicate_tool_call(ctx, &call);
                            state.install_phase(ExecutionPhase::PreparingTools(batch));
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
                                (!tool.args_summary.is_empty())
                                    .then_some(tool.args_summary.as_str()),
                            );
                            batch.ready_tools.push(tool);
                            state.install_phase(ExecutionPhase::PreparingTools(batch));
                        } else {
                            let request_id = scru128::new().to_string();
                            ctx.observer.audit_permission(
                                &ctx.session.id,
                                &tool.call.name,
                                "needs_approval",
                                &trust_mode_label,
                                (!tool.args_summary.is_empty())
                                    .then_some(tool.args_summary.as_str()),
                            );
                            let _ = stream_tx.send(StreamEvent::ApprovalNeeded {
                                request_id: request_id.clone(),
                                tool_name: tool.call.name.clone(),
                                args_summary: tool.args_summary.clone(),
                            });
                            state.install_phase(ExecutionPhase::WaitingApproval(ApprovalPhase {
                                pending: PendingApproval { request_id, tool },
                                batch,
                            }));
                        }
                    }
                }
            }

            // ── Waiting：模型请求（ReAct/完成度检查/ForceFinal 共用流式驱动）──
            ExecutionPhase::WaitingModel(mut active)
            | ExecutionPhase::CheckingCompletion(mut active)
            | ExecutionPhase::ForceFinalPhase(mut active) => {
                tokio::select! {
                                    biased;
                                    command = cmd_rx.recv() => {
                                        // 命令优先：归还阶段，交由统一命令处理接管。
                                        deferred = Some(match command {
                                            Some(command) => Deferred::Command(command),
                                            None => Deferred::Closed,
                                        });
                                        state.install_phase(reinstate_llm_phase(active));
                                        continue 'agent_loop;
                                    }
                                    chunk = active.chunk_rx.recv() => {
                                        if let Some(chunk) = chunk {
                                            if let Some(chunk_usage) = &chunk.usage {
                                                let usage: TokenUsage = chunk_usage.clone().into();
                                                active.streaming_usage.accumulate(&usage);
                                            }
                                            active.streamed_text.push_str(&chunk.content);
                                            active.streamed_reasoning.push_str(&chunk.reasoning_content);
                                            active.sink.push_chunk(&chunk);
                                            state.install_phase(reinstate_llm_phase(active));
                                            continue 'agent_loop;
                                        }
                                        // 通道关闭：完整响应可收取。
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
                                        let next = complete_llm_request(
                                            ctx,
                                            &mut state,
                                            &mut injections,
                                            &context_organizer,
                                            &stream_tx,
                                            purpose,
                                            pending_msg_id,
                                            streamed_text,
                                            streamed_reasoning,
                                            streaming_usage,
                                            response_result,
                                        )
                ;
                                        state.install_phase(next);
                                    }
                                }
            }

            // ── Waiting（兼容层，任务 05 迁移）：工具任务运行中 ──
            ExecutionPhase::WaitingTools(mut tools) => {
                tokio::select! {
                    biased;
                    command = cmd_rx.recv() => {
                        deferred = Some(match command {
                            Some(command) => Deferred::Command(command),
                            None => Deferred::Closed,
                        });
                        state.install_phase(ExecutionPhase::WaitingTools(tools));
                        continue 'agent_loop;
                    }
                    tool_result = tools.tasks.join_next_with_id() => {
                        let joined = tool_result.expect("工具任务集合非空时必须返回结果");
                        let (task_id, task_output) = match joined {
                            Ok((task_id, output)) => (task_id, output),
                            Err(error) => {
                                let task_id = error.id();
                                let running = tools
                                    .running
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
                        let running = tools
                            .running
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
                        tools.batch.needs_failure_recovery |= needs_recovery;

                        if !tools.tasks.is_empty() {
                            state.install_phase(ExecutionPhase::WaitingTools(tools));
                            continue 'agent_loop;
                        }

                        if !tools.batch.calls.is_empty() || !tools.batch.ready_tools.is_empty() {
                            state.install_phase(ExecutionPhase::PreparingTools(tools.batch));
                            continue 'agent_loop;
                        }

                        let observed_tokens = observed_total_tokens(&tools.batch.response_usage);
                        if context_organizer.needs_compression(observed_tokens) {
                            let active = ActiveCompression::start(
                                ctx,
                                &context_organizer,
                                observed_tokens,
                                CompressionContinuation::ToolBatch,
                            );
                            state.install_phase(ExecutionPhase::Compressing(CompressingPhase {
                                active,
                                suspended_batch: Some(tools.batch),
                            }));
                        } else {
                            state.install_phase(ExecutionPhase::PreparingTools(tools.batch));
                        }
                    }
                }
            }

            // ── Waiting（兼容层，任务 05 迁移）：等待审批（仅命令可推进）──
            ExecutionPhase::WaitingApproval(approval) => {
                let command = cmd_rx.recv().await;
                deferred = Some(match command {
                    Some(command) => Deferred::Command(command),
                    None => Deferred::Closed,
                });
                state.install_phase(ExecutionPhase::WaitingApproval(approval));
            }

            // ── Waiting（兼容层，任务 06 迁移）：上下文压缩进行中 ──
            ExecutionPhase::Compressing(mut compressing) => {
                tokio::select! {
                    biased;
                    command = cmd_rx.recv() => {
                        deferred = Some(match command {
                            Some(command) => Deferred::Command(command),
                            None => Deferred::Closed,
                        });
                        state.install_phase(ExecutionPhase::Compressing(compressing));
                        continue 'agent_loop;
                    }
                    compression_result = compressing.active.wait() => {
                        let CompressingPhase {
                            active,
                            suspended_batch,
                        } = compressing;
                        let continuation = active.complete(
                            ctx,
                            &mut state.accumulated_usage,
                            compression_result,
                        );

                        let next = match continuation {
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
                                ExecutionPhase::PreparingTools(
                                    suspended_batch.expect("ToolBatch 续接必须挂起批次"),
                                )
                            }
                            CompressionContinuation::InvalidToolCalls => {
                                ExecutionPhase::NeedModel
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
                                    ExecutionPhase::NeedModel
                                } else {
                                    persist_error(
                                        ctx,
                                        format!("ReAct 循环请求失败：{error_message}"),
                                    );
                                    ExecutionPhase::PendingFinish(TurnExecutionResult::failed(
                                        state.accumulated_usage.clone(),
                                        error_message,
                                    ))
                                }
                            }
                        };
                        state.install_phase(next);
                    }
                }
            }
        }
    };

    // 运行配置即时生效，Session 在本轮唯一出口接收最终值。
    ctx.trust_mode = trust_mode;
    ctx.session.trust_mode = trust_mode;
    ctx.session.reasoning_effort = Some(ctx.agent_config.reasoning_effort.clone());
    injections.commit(ctx);
    result
}

/// 按 purpose 重建对应的模型 Waiting 阶段（构造时变体与 purpose 一一对应）。
fn reinstate_llm_phase(active: ActiveLlm) -> ExecutionPhase {
    match &active.purpose {
        LlmPurpose::React { .. } => ExecutionPhase::WaitingModel(active),
        LlmPurpose::Summary { .. } => ExecutionPhase::CheckingCompletion(active),
        LlmPurpose::ForceFinal { .. } => ExecutionPhase::ForceFinalPhase(active),
    }
}

/// 模型请求完成后的统一处理：按 purpose 归一化响应并产出下一阶段。
///
/// 这是旧 select「chunk 通道关闭」分支的主体：错误分流（上下文超限 → 压缩重试）、
/// 工具调用 → PreparingTools、文本回复 → 完成度检查/直接完成、Summary 判定 →
/// 续作/完成/ForceFinal、ForceFinal → 提交结果。
#[allow(clippy::too_many_arguments)]
fn complete_llm_request(
    ctx: &mut TurnContext,
    state: &mut AgentLoopState,
    injections: &mut ToolInjectionBuffer,
    context_organizer: &ContextOrganizer,
    stream_tx: &std::sync::mpsc::Sender<StreamEvent>,
    purpose: LlmPurpose,
    pending_msg_id: String,
    streamed_text: String,
    streamed_reasoning: String,
    streaming_usage: TokenUsage,
    response_result: anyhow::Result<ModelFunctionResponse>,
) -> ExecutionPhase {
    let context_limit = ctx.context_limit;
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
                    let error_message = format!("{error:#}");
                    let should_compress = error_message.contains("context_window_exceeded")
                        || error_message.contains("context_length_exceeded")
                        || (error_message.contains("content_blocks=0")
                            && error_message.contains("stop_reason=end_turn"));
                    if should_compress {
                        tracing::warn!("检测到上下文超限，尝试强制压缩");
                        let previous_summary_up_to = ctx.session.summary_up_to;
                        let active = ActiveCompression::start_forced(
                            ctx,
                            context_organizer,
                            CompressionContinuation::ContextRetry {
                                previous_summary_up_to,
                                error_message,
                            },
                        );
                        return ExecutionPhase::Compressing(CompressingPhase {
                            active,
                            suspended_batch: None,
                        });
                    }
                    injections.commit(ctx);
                    persist_error(ctx, format!("ReAct 循环请求失败：{error_message}"));
                    return ExecutionPhase::PendingFinish(TurnExecutionResult::failed(
                        state.accumulated_usage.clone(),
                        error_message,
                    ));
                }
            };

            state.accumulated_usage.accumulate(&response.usage);
            emit_token_usage(
                stream_tx,
                &response.usage,
                Some(response.usage.prompt_tokens.max(ctx.session.current_tokens)),
                context_limit,
                format!("react-round-{}", state.budget.request_round + 1),
                None,
            );
            state.budget.request_round += 1;
            state.budget.react_rounds_in_phase += 1;

            if response.tool_calls.is_empty() && !response.invalid_tool_calls.is_empty() {
                append_invalid_tool_calls_context(ctx, &response.invalid_tool_calls);
                let observed_tokens = observed_total_tokens(&response.usage);
                if context_organizer.needs_compression(observed_tokens) {
                    let active = ActiveCompression::start(
                        ctx,
                        context_organizer,
                        observed_tokens,
                        CompressionContinuation::InvalidToolCalls,
                    );
                    ExecutionPhase::Compressing(CompressingPhase {
                        active,
                        suspended_batch: None,
                    })
                } else {
                    ExecutionPhase::NeedModel
                }
            } else if response.tool_calls.is_empty() {
                let disposition =
                    handle_react_text_response(ctx, &pending_msg_id, &response, state);
                let observed_tokens = observed_total_tokens(&response.usage);
                if context_organizer.needs_compression(observed_tokens) {
                    let active = ActiveCompression::start(
                        ctx,
                        context_organizer,
                        observed_tokens,
                        CompressionContinuation::ReactText {
                            pending_msg_id,
                            disposition,
                            request_injection_generation,
                        },
                    );
                    ExecutionPhase::Compressing(CompressingPhase {
                        active,
                        suspended_batch: None,
                    })
                } else {
                    finish_react_text(
                        ctx,
                        state,
                        injections,
                        pending_msg_id,
                        disposition,
                        request_injection_generation,
                    )
                }
            } else {
                state.budget.executed_tool_in_phase = true;
                let calls = record_tool_calls(
                    ctx,
                    &pending_msg_id,
                    &response,
                    format!("react-round-{}", state.budget.request_round),
                );
                ExecutionPhase::PreparingTools(ToolBatchState {
                    calls: calls.into_iter().enumerate().collect(),
                    ready_tools: Vec::new(),
                    prepared_keys: HashSet::new(),
                    invalid_tool_calls: response.invalid_tool_calls,
                    response_usage: response.usage,
                    request_injection_generation,
                    needs_failure_recovery: false,
                })
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
                    return ExecutionPhase::StartForceFinal {
                        reason: ForceFinalReason::SummaryError,
                        request_injection_generation,
                        summary_error: Some(message),
                    };
                }
            };

            emit_token_usage(
                stream_tx,
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

            if response.tool_calls.is_empty() && !response.invalid_tool_calls.is_empty() {
                append_invalid_tool_calls_context(ctx, &response.invalid_tool_calls);
                let decision = SummaryDecision::NeedMoreWork(
                    "总结阶段返回的工具调用未通过 schema 校验，请根据校验原因继续完成任务。"
                        .to_string(),
                );
                let observed_tokens = observed_total_tokens(&usage);
                if context_organizer.needs_compression(observed_tokens) {
                    let active = ActiveCompression::start(
                        ctx,
                        context_organizer,
                        observed_tokens,
                        CompressionContinuation::Summary {
                            decision,
                            request_injection_generation,
                        },
                    );
                    return ExecutionPhase::Compressing(CompressingPhase {
                        active,
                        suspended_batch: None,
                    });
                }
                return finish_summary(
                    ctx,
                    state,
                    injections,
                    decision,
                    request_injection_generation,
                );
            }

            if !response.tool_calls.is_empty() {
                tracing::warn!(
                    count = response.tool_calls.len(),
                    protocol = ?ctx.client().protocol(),
                    "summary phase returned tool calls; continuing tool execution"
                );
                // 工具调用说明任务仍可继续。把总结响应转回 ReAct 工具批次，
                // 不经过 finish_summary，因此不增加 continuation_count。
                state.reset_react_phase();
                state.budget.executed_tool_in_phase = true;
                let calls = record_tool_calls(
                    ctx,
                    &pending_msg_id,
                    &response,
                    format!("summary-iteration-{iteration}-continuation"),
                );
                return ExecutionPhase::PreparingTools(ToolBatchState {
                    calls: calls.into_iter().enumerate().collect(),
                    ready_tools: Vec::new(),
                    prepared_keys: HashSet::new(),
                    invalid_tool_calls: response.invalid_tool_calls,
                    response_usage: response.usage,
                    request_injection_generation,
                    needs_failure_recovery: false,
                });
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

            let observed_tokens = observed_total_tokens(&usage);
            if context_organizer.needs_compression(observed_tokens) {
                let active = ActiveCompression::start(
                    ctx,
                    context_organizer,
                    observed_tokens,
                    CompressionContinuation::Summary {
                        decision,
                        request_injection_generation,
                    },
                );
                ExecutionPhase::Compressing(CompressingPhase {
                    active,
                    suspended_batch: None,
                })
            } else {
                finish_summary(
                    ctx,
                    state,
                    injections,
                    decision,
                    request_injection_generation,
                )
            }
        }
        LlmPurpose::ForceFinal {
            request_injection_generation,
            summary_error,
        } => {
            let force_result = match response_result {
                Ok(response) => {
                    state.accumulated_usage.accumulate(&response.usage);
                    commit_summary_message(ctx, &pending_msg_id, &response, "force_final_response")
                }
                Err(error) => {
                    let message = error.to_string();
                    persist_error(ctx, format!("force_final_response 失败：{message}"));
                    Err(message)
                }
            };
            let received_new_injection = injections.generation() > request_injection_generation;
            injections.commit(ctx);
            if received_new_injection {
                state.reset_react_phase();
                return ExecutionPhase::NeedModel;
            }
            match force_result {
                Ok(()) => ExecutionPhase::PendingFinish(TurnExecutionResult::success(
                    state.accumulated_usage.clone(),
                )),
                Err(message) => {
                    let message = summary_error.map_or(message.clone(), |summary_error| {
                        format!("总结阶段失败：{summary_error}；强制最终回复失败：{message}")
                    });
                    ExecutionPhase::PendingFinish(TurnExecutionResult::failed(
                        state.accumulated_usage.clone(),
                        message,
                    ))
                }
            }
        }
    }
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
    use crate::session::{Message, MessageRole, MessageToolCall, Session};
    use crate::tool::ToolResult;
    use crate::tool_override::{
        MentionCandidateProvider, PromptSectionProvider, ToolOverrideHandler, ToolSpecProvider,
    };
    use crate::turn_context::TurnContext;
    use std::collections::HashMap;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tiangong_llm::{ModelEndpoint, ProviderProtocol};
    use tiangong_types::{StreamEvent, TokenUsage, stream::ContextCompressAction};
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
            "created": 0,
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
            "created": 0,
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
            "created": 0,
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

    async fn mount_completion(
        server: &MockServer,
        content: &str,
        finish_reason: &str,
        prompt_tokens: u64,
        completion_tokens: u64,
        delay: Option<Duration>,
    ) {
        let body = serde_json::json!({
            "id": "chatcmpl-compression",
            "object": "chat.completion",
            "model": "test-model",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": content},
                "finish_reason": finish_reason,
            }],
            "usage": {
                "prompt_tokens": prompt_tokens,
                "completion_tokens": completion_tokens,
                "total_tokens": prompt_tokens + completion_tokens,
            },
        });
        let mut response = ResponseTemplate::new(200).set_body_json(body);
        if let Some(delay) = delay {
            response = response.set_delay(delay);
        }
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(response)
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

    struct PausedTool {
        started: Arc<Notify>,
        release: Arc<Notify>,
    }

    impl ToolOverrideHandler for PausedTool {
        fn handle(
            &self,
            _call: &ToolCall,
            _session: &mut Session,
            _actor_id: &str,
        ) -> Pin<Box<dyn Future<Output = Option<ToolResult>> + Send>> {
            let started = self.started.clone();
            let release = self.release.clone();
            Box::pin(async move {
                started.notify_one();
                release.notified().await;
                Some(ToolResult {
                    ok: true,
                    summary: "测试工具已完成".to_string(),
                    stdout: "done".to_string(),
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
    impl MentionCandidateProvider for TrustTrackingPlugin {}

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
    fn endpoint_with_protocol(server: &MockServer, protocol: ProviderProtocol) -> ModelEndpoint {
        ModelEndpoint {
            base_url: server.uri(),
            api_key: "test-key".to_string(),
            model: "test-model".to_string(),
            protocol,
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
        storage_root: std::path::PathBuf,
    }

    impl TestHarness {
        /// `extra_overrides` / `tools` 用于工具调用路径测试。
        fn new(
            server: &MockServer,
            tools: Vec<ToolSpec>,
            tool_overrides: HashMap<String, Arc<dyn ToolOverrideHandler>>,
        ) -> Self {
            Self::new_with_protocol(
                server,
                ProviderProtocol::OpenAiChatCompletions,
                tools,
                tool_overrides,
                Vec::new(),
            )
        }

        /// 额外注入插件（用于生命周期计数等需要插件观察的测试）。
        fn new_with_plugins(
            server: &MockServer,
            tools: Vec<ToolSpec>,
            tool_overrides: HashMap<String, Arc<dyn ToolOverrideHandler>>,
            plugins: Vec<Arc<dyn Plugin>>,
        ) -> Self {
            Self::new_with_protocol(
                server,
                ProviderProtocol::OpenAiChatCompletions,
                tools,
                tool_overrides,
                plugins,
            )
        }

        fn new_with_protocol(
            server: &MockServer,
            protocol: ProviderProtocol,
            tools: Vec<ToolSpec>,
            tool_overrides: HashMap<String, Arc<dyn ToolOverrideHandler>>,
            plugins: Vec<Arc<dyn Plugin>>,
        ) -> Self {
            let root = tempfile::tempdir().expect("创建临时目录失败");
            let mut session = Session::new("test-session".to_string());
            session.bind_storage_root(root.path());
            session.append_message(MessageRole::User, "你好");
            session.rebuild_system_prompt(&SystemPromptConfig::from_plugin_sections(Vec::new()));
            // 暴露 storage_root 供 turn 级测试磁盘重载验证。
            let storage_root = root.path().to_path_buf();
            // 让 tempdir 存活到 turn 结束(用 `leak` 避免 Rust 借用检查器抱怨;
            // 测试进程结束即回收)。
            std::mem::forget(root);

            let agent_config = AgentConfig {
                reasoning_effort: "none".to_string(),
                ..Default::default()
            };
            let client = SingleProviderClient::new(endpoint_with_protocol(server, protocol));
            let (stream_tx, stream_rx) = std::sync::mpsc::channel::<StreamEvent>();
            let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<Command>();

            let ctx = TurnContext::builder()
                .client(client)
                .session(session)
                .stream_tx(stream_tx)
                .plugins(plugins)
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
                storage_root,
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn compression_persists_model_visible_resume_after_system_prompt() {
        let server = MockServer::start().await;
        mount_sse(
            &server,
            vec![text_delta_chunk("最终回答。"), usage_chunk(185_900, 5)],
        )
        .await;
        mount_completion(
            &server,
            "[[CURRENT_TASK]]\n当前任务已完成\n[[SUMMARY]]\n历史摘要",
            "stop",
            100,
            20,
            None,
        )
        .await;

        let mut harness = TestHarness::new(&server, Vec::new(), HashMap::new());
        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;

        assert!(matches!(result.outcome, TurnExecutionOutcome::Success));
        assert_eq!(result.usage.total_tokens, 186_025);
        assert_eq!(
            harness.ctx.session.context_summary.as_deref(),
            Some("历史摘要")
        );
        let resume = &harness.ctx.session.messages[harness.ctx.session.summary_up_to];
        assert_eq!(resume.role, MessageRole::User);
        assert_eq!(resume.phase, crate::session::MessagePhase::CompressedResume);
        assert!(!resume.model_excluded);
        assert!(matches!(
            &resume.content[0],
            crate::session::ContentBlock::ModelInstruction { text }
                if text.contains("当前任务已完成")
        ));

        let context = harness.ctx.session.context();
        assert_eq!(context[0].role, MessageRole::System);
        assert_eq!(context[1].id, resume.id);
        assert_eq!(
            context[1].phase,
            crate::session::MessagePhase::CompressedResume
        );

        let persisted = Session::load_from_storage(
            harness
                .ctx
                .session
                .bound_storage_root()
                .expect("测试会话应绑定存储目录"),
            &harness.ctx.session.id,
        )
        .expect("压缩结果应持久化");
        assert_eq!(
            persisted.messages[persisted.summary_up_to].phase,
            crate::session::MessagePhase::CompressedResume
        );

        harness
            .ctx
            .session
            .append_message(MessageRole::User, "继续提出新问题");
        let context = harness.ctx.session.context();
        assert_eq!(
            context[1].phase,
            crate::session::MessagePhase::CompressedResume
        );
        assert_eq!(context[2].text_content(), "继续提出新问题");

        let requests = server.received_requests().await.unwrap();
        let compression_body: serde_json::Value =
            serde_json::from_slice(&requests[1].body).unwrap();
        assert_eq!(compression_body["max_tokens"], serde_json::json!(9_999));
        assert!(
            compression_body["messages"]
                .to_string()
                .contains("不得超过 9999 tokens")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn forced_compression_folds_older_history_and_keeps_latest_tool_batch() {
        let server = MockServer::start().await;
        mount_request_error(&server, "context_window_exceeded").await;
        mount_request_error(&server, "context_window_exceeded").await;
        mount_completion(
            &server,
            "[[CURRENT_TASK]]\n继续处理最近工具结果\n[[SUMMARY]]\n较早历史摘要",
            "stop",
            100,
            20,
            None,
        )
        .await;
        mount_sse(
            &server,
            vec![text_delta_chunk("最终回答。"), usage_chunk(10, 5)],
        )
        .await;

        let mut harness = TestHarness::new(&server, Vec::new(), HashMap::new());
        harness
            .ctx
            .session
            .append_message(MessageRole::Assistant, "较早回答");
        let mut assistant = Message::new(MessageRole::Assistant, "");
        assistant.tool_calls = vec![MessageToolCall {
            id: "latest-call".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "latest.txt"}),
        }];
        harness.ctx.session.messages.push(assistant);
        harness.ctx.session.messages.push(Message::tool_result(
            "latest-call",
            "read_file",
            "recent-tool-output",
            false,
        ));

        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;

        assert!(
            matches!(result.outcome, TurnExecutionOutcome::Success),
            "强制压缩后应重试成功，实际结果：{:?}",
            result.outcome
        );
        assert_eq!(result.usage.total_tokens, 135);
        assert_eq!(harness.ctx.session.summary_up_to, 2);
        let context = harness.ctx.session.context();
        assert_eq!(context[0].role, MessageRole::System);
        assert_eq!(
            context[1].phase,
            crate::session::MessagePhase::CompressedResume
        );
        assert!(context.iter().any(|message| {
            message.tool_call_id.as_deref() == Some("latest-call")
                && message.text_content() == "recent-tool-output"
        }));

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 4);
        let first_body = String::from_utf8_lossy(&requests[0].body);
        let compression_body = String::from_utf8_lossy(&requests[2].body);
        let retry_body = String::from_utf8_lossy(&requests[3].body);
        assert!(first_body.contains("recent-tool-output"));
        assert!(!compression_body.contains("recent-tool-output"));
        assert!(compression_body.contains("较早回答"));
        assert!(retry_body.contains("recent-tool-output"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn truncated_compression_does_not_advance_summary_boundary() {
        let server = MockServer::start().await;
        mount_sse(
            &server,
            vec![text_delta_chunk("最终回答。"), usage_chunk(185_900, 5)],
        )
        .await;
        mount_completion(
            &server,
            "[[CURRENT_TASK]]\n当前任务已完成\n[[SUMMARY]]\n截断摘要",
            "length",
            100,
            20,
            None,
        )
        .await;

        let mut harness = TestHarness::new(&server, Vec::new(), HashMap::new());
        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;

        assert!(matches!(result.outcome, TurnExecutionOutcome::Success));
        assert_eq!(result.usage.total_tokens, 186_025);
        assert_eq!(harness.ctx.session.summary_up_to, 0);
        assert!(harness.ctx.session.context_summary.is_none());
        assert!(harness.stream_rx.try_iter().any(|event| matches!(
            event,
            StreamEvent::ContextCompressed {
                action: ContextCompressAction::Failed,
                ..
            }
        )));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn persistence_failure_keeps_original_compression_state() {
        let server = MockServer::start().await;
        mount_sse(
            &server,
            vec![text_delta_chunk("最终回答。"), usage_chunk(185_900, 5)],
        )
        .await;
        mount_completion(
            &server,
            "[[CURRENT_TASK]]\n当前任务已完成\n[[SUMMARY]]\n历史摘要",
            "stop",
            100,
            20,
            None,
        )
        .await;

        let invalid_root = tempfile::tempdir().unwrap();
        let blocking_file = invalid_root.path().join("not-a-directory");
        std::fs::write(&blocking_file, "blocked").unwrap();
        let mut harness = TestHarness::new(&server, Vec::new(), HashMap::new());
        harness.ctx.session.bind_storage_root(blocking_file);

        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;
        let events = harness.stream_rx.try_iter().collect::<Vec<_>>();

        assert!(matches!(result.outcome, TurnExecutionOutcome::Success));
        assert_eq!(harness.ctx.session.summary_up_to, 0);
        assert!(harness.ctx.session.context_summary.is_none());
        assert!(events.iter().any(|event| matches!(
            event,
            StreamEvent::ContextCompressed {
                action: ContextCompressAction::Failed,
                ..
            }
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            StreamEvent::ContextCompressed {
                action: ContextCompressAction::Auto,
                ..
            }
        )));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_interrupts_active_context_compression() {
        let server = MockServer::start().await;
        mount_sse(
            &server,
            vec![text_delta_chunk("最终回答。"), usage_chunk(185_900, 5)],
        )
        .await;
        mount_completion(
            &server,
            "[[CURRENT_TASK]]\n当前任务已完成\n[[SUMMARY]]\n历史摘要",
            "stop",
            100,
            20,
            Some(Duration::from_secs(5)),
        )
        .await;

        let harness = TestHarness::new(&server, Vec::new(), HashMap::new());
        let TestHarness {
            mut ctx,
            stream_rx,
            cmd_tx,
            mut cmd_rx,
            ..
        } = harness;
        let cancel_task = tokio::task::spawn_blocking(move || {
            loop {
                let event = stream_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("等待压缩开始事件超时");
                if matches!(event, StreamEvent::ContextCompressing { .. }) {
                    cmd_tx.send(Command::Cancel).unwrap();
                    break stream_rx;
                }
            }
        });

        let result =
            tokio::time::timeout(Duration::from_secs(2), execute_turn(&mut ctx, &mut cmd_rx))
                .await
                .expect("取消压缩后 turn 应及时结束");
        let stream_rx = cancel_task.await.unwrap();

        assert!(matches!(result.outcome, TurnExecutionOutcome::Cancelled));
        assert_eq!(ctx.session.summary_up_to, 0);
        assert!(stream_rx.try_iter().any(|event| matches!(
            event,
            StreamEvent::ContextCompressed {
                action: ContextCompressAction::Cancelled,
                ..
            }
        )));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_interrupts_manual_context_compression() {
        let server = MockServer::start().await;
        mount_completion(
            &server,
            "[[SUMMARY]]\n历史摘要",
            "stop",
            100,
            20,
            Some(Duration::from_secs(5)),
        )
        .await;

        let harness = TestHarness::new(&server, Vec::new(), HashMap::new());
        let TestHarness {
            ctx,
            stream_rx,
            cmd_tx,
            cmd_rx,
            ..
        } = harness;
        let cancel_task = tokio::task::spawn_blocking(move || {
            loop {
                let event = stream_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("等待手动压缩开始事件超时");
                if matches!(event, StreamEvent::ContextCompressing { .. }) {
                    cmd_tx.send(Command::Cancel).unwrap();
                    break stream_rx;
                }
            }
        });

        tokio::time::timeout(
            Duration::from_secs(2),
            crate::react::compression::run_manual_context_compression(ctx, cmd_rx),
        )
        .await
        .expect("取消手动压缩后任务应及时结束");
        let stream_rx = cancel_task.await.unwrap();

        assert!(stream_rx.try_iter().any(|event| matches!(
            event,
            StreamEvent::ContextCompressed {
                action: ContextCompressAction::Cancelled,
                ..
            }
        )));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn manual_context_compression_preserves_runtime_settings() {
        let server = MockServer::start().await;
        mount_completion(
            &server,
            "[[SUMMARY]]\n历史摘要",
            "stop",
            100,
            20,
            Some(Duration::from_millis(200)),
        )
        .await;

        let harness = TestHarness::new(&server, Vec::new(), HashMap::new());
        let TestHarness {
            ctx,
            stream_rx,
            cmd_tx,
            cmd_rx,
            ..
        } = harness;
        let storage_root = ctx
            .session
            .bound_storage_root()
            .expect("测试 Session 必须绑定存储目录")
            .to_path_buf();
        let session_id = ctx.session.id.clone();
        let _cmd_tx_guard = cmd_tx.clone();
        let update_task = tokio::task::spawn_blocking(move || {
            loop {
                let event = stream_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("等待手动压缩开始事件超时");
                if matches!(event, StreamEvent::ContextCompressing { .. }) {
                    cmd_tx
                        .send(Command::SetReasoningEffort("max".to_string()))
                        .unwrap();
                    cmd_tx
                        .send(Command::SetTrustMode(TrustMode::Supervised))
                        .unwrap();
                    break;
                }
            }
        });

        tokio::time::timeout(
            Duration::from_secs(2),
            crate::react::compression::run_manual_context_compression(ctx, cmd_rx),
        )
        .await
        .expect("手动压缩应及时完成");
        update_task.await.unwrap();

        let persisted =
            Session::load_from_storage(&storage_root, &session_id).expect("手动压缩结果应持久化");
        assert_eq!(persisted.reasoning_effort.as_deref(), Some("max"));
        assert_eq!(persisted.trust_mode, TrustMode::Supervised);
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
            let _ = cmd_tx.send(Command::SetReasoningEffort("max".to_string()));
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
        assert_eq!(harness.ctx.agent_config.reasoning_effort, "max");
        assert_eq!(harness.ctx.session.reasoning_effort.as_deref(), Some("max"));
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

    /// ALR-111 用量权威：多轮模型请求的用量必须累计到终态结果，重构后仍须保证
    /// 最终终态和 Session 使用封口前最新累计用量。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn accumulated_usage_is_aggregated_across_requests() {
        let server = MockServer::start().await;
        let invocations = Arc::new(Mutex::new(Vec::new()));
        let mut overrides: HashMap<String, Arc<dyn ToolOverrideHandler>> = HashMap::new();
        overrides.insert(
            "echo".to_string(),
            Arc::new(EchoTool {
                invocations: invocations.clone(),
            }),
        );
        // 1) 工具调用；2) 文本以问号结尾 → 进入 Summary；3) Summary 完成。
        mount_sse(
            &server,
            vec![tool_call_chunk("call_1", "echo", "{}"), usage_chunk(15, 3)],
        )
        .await;
        mount_sse(
            &server,
            vec![text_delta_chunk("结果还需要补充吗?"), usage_chunk(25, 5)],
        )
        .await;
        mount_sse(
            &server,
            vec![text_delta_chunk("已完成。"), usage_chunk(30, 4)],
        )
        .await;

        let mut harness = TestHarness::new(&server, vec![tool_spec("echo")], overrides);
        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;

        assert!(
            matches!(result.outcome, TurnExecutionOutcome::Success),
            "工具+总结链路应返回 Success，实际: {:?}",
            result.outcome
        );
        // 三轮请求用量累计：(15+3) + (25+5) + (30+4) = 82
        assert_eq!(
            result.usage.total_tokens, 82,
            "终态用量应为各轮模型请求用量之和（ALR-111）"
        );
        harness.drain_stream();
    }

    /// ALR-302 事件契约：工具执行必须先发 ToolStart，完成后发 ToolResult，
    /// 重构后事件顺序需保持。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tool_execution_emits_start_before_result_event() {
        let server = MockServer::start().await;
        let invocations = Arc::new(Mutex::new(Vec::new()));
        let mut overrides: HashMap<String, Arc<dyn ToolOverrideHandler>> = HashMap::new();
        overrides.insert(
            "echo".to_string(),
            Arc::new(EchoTool {
                invocations: invocations.clone(),
            }),
        );
        // 工具调用 → 文本以问号结尾 → Summary 完成。
        mount_sse(
            &server,
            vec![tool_call_chunk("call_1", "echo", "{}"), usage_chunk(15, 3)],
        )
        .await;
        mount_sse(
            &server,
            vec![text_delta_chunk("结果还需要补充吗?"), usage_chunk(25, 5)],
        )
        .await;
        mount_sse(
            &server,
            vec![text_delta_chunk("已完成。"), usage_chunk(30, 4)],
        )
        .await;

        let mut harness = TestHarness::new(&server, vec![tool_spec("echo")], overrides);
        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;
        assert!(matches!(result.outcome, TurnExecutionOutcome::Success));

        let events: Vec<StreamEvent> = harness.stream_rx.try_iter().collect();
        let start_idx = events.iter().position(
            |e| matches!(e, StreamEvent::ToolStart { name, .. } if name.as_str() == "echo"),
        );
        let result_idx = events.iter().position(
            |e| matches!(e, StreamEvent::ToolResult { name, .. } if name.as_str() == "echo"),
        );
        assert!(start_idx.is_some(), "应发送 echo 的 ToolStart 事件");
        assert!(result_idx.is_some(), "应发送 echo 的 ToolResult 事件");
        assert!(
            start_idx < result_idx,
            "ToolStart 必须在 ToolResult 之前（ALR-302 事件契约）"
        );
    }

    /// ALR-107/109：run_turn 只发一次 Done，并把 turn_status 写入最新用户消息；
    /// 重载磁盘 session 后锚点一致。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_turn_emits_single_done_and_anchors_status_to_latest_user_message() {
        use super::super::turn::run_turn;

        let server = MockServer::start().await;
        mount_sse(
            &server,
            vec![text_delta_chunk("你好,我是测试助手。"), usage_chunk(10, 5)],
        )
        .await;
        let harness = TestHarness::new(&server, Vec::new(), HashMap::new());
        // 标题为 "test-session"（非默认），spawn_title_generation 跳过。
        let storage_root = harness.storage_root.clone();
        let session_id = harness.ctx.session.id.clone();
        let (_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<Command>();
        let stream_rx = harness.stream_rx;
        run_turn(harness.ctx, cmd_rx).await;

        // ALR-109 唯一终态：恰好一个 Done。
        let events: Vec<StreamEvent> = stream_rx.try_iter().collect();
        let done = events
            .iter()
            .filter(|e| matches!(e, StreamEvent::Done { .. }))
            .count();
        assert_eq!(
            done, 1,
            "一个物理 turn 只发一次 Done（ALR-109），实际: {done}"
        );

        // ALR-107 最新消息锚点：重载磁盘 session，最新用户消息应有 turn_status。
        let reloaded =
            Session::load_from_storage(&storage_root, &session_id).expect("重载 session");
        let latest_has_status = reloaded
            .messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::User)
            .is_some_and(|m| m.turn_status.is_some());
        assert!(
            latest_has_status,
            "最新用户消息应写入 turn_status（ALR-107）"
        );
    }

    /// 计数 on_turn_started / on_turn_finished 调用次数的插件，用于验证生命周期唯一性。
    struct LifecycleCountingPlugin {
        started: Arc<AtomicU32>,
        finished: Arc<AtomicU32>,
    }

    impl ToolOverrideHandler for LifecycleCountingPlugin {}
    impl ToolSpecProvider for LifecycleCountingPlugin {}
    impl PromptSectionProvider for LifecycleCountingPlugin {}
    impl MentionCandidateProvider for LifecycleCountingPlugin {}

    impl Plugin for LifecycleCountingPlugin {
        fn id(&self) -> &str {
            "lifecycle-counter"
        }
        fn on_turn_started(&self, _: &mut Session, _: usize) {
            self.started.fetch_add(1, Ordering::SeqCst);
        }
        fn on_turn_finished(&self, _: &mut Session, _: usize) {
            self.finished.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// ALR-108：一个物理 turn 只调用一次 on_turn_started 和一次 on_turn_finished。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_turn_invokes_lifecycle_hooks_exactly_once() {
        use super::super::turn::run_turn;

        let server = MockServer::start().await;
        mount_sse(
            &server,
            vec![text_delta_chunk("你好,我是测试助手。"), usage_chunk(10, 5)],
        )
        .await;
        let started = Arc::new(AtomicU32::new(0));
        let finished = Arc::new(AtomicU32::new(0));
        let plugin = Arc::new(LifecycleCountingPlugin {
            started: started.clone(),
            finished: finished.clone(),
        });
        let harness =
            TestHarness::new_with_plugins(&server, Vec::new(), HashMap::new(), vec![plugin]);
        let (_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<Command>();
        run_turn(harness.ctx, cmd_rx).await;
        assert_eq!(
            started.load(Ordering::SeqCst),
            1,
            "on_turn_started 应只调用一次（ALR-108）"
        );
        assert_eq!(
            finished.load(Ordering::SeqCst),
            1,
            "on_turn_finished 应只调用一次（ALR-108）"
        );
    }

    /// ALR-101/110：运行中注入用户消息——中断工具等待（协议闭合）、保存消息、
    /// 确认接收，并从新意图重启直至成功。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn inject_user_message_interrupts_tools_and_restarts() {
        let server = MockServer::start().await;
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let mut overrides: HashMap<String, Arc<dyn ToolOverrideHandler>> = HashMap::new();
        overrides.insert(
            "paused_probe".to_string(),
            Arc::new(PausedTool {
                started: started.clone(),
                release: release.clone(),
            }),
        );
        // 1) 首轮：工具调用（PausedTool 阻塞，制造"工具等待中"）。
        mount_sse(
            &server,
            vec![
                tool_call_chunk("call_1", "paused_probe", "{}"),
                usage_chunk(15, 3),
            ],
        )
        .await;
        // 2) 注入后新意图的请求：直接文本回答 → Success。
        mount_sse(
            &server,
            vec![
                text_delta_chunk("好的，按新要求完成了。"),
                usage_chunk(20, 4),
            ],
        )
        .await;

        let harness = TestHarness::new(&server, vec![tool_spec("paused_probe")], overrides);
        let TestHarness {
            mut ctx,
            stream_rx,
            cmd_tx,
            mut cmd_rx,
            ..
        } = harness;
        // 注入由独立任务发送：等工具进入运行后投递引导消息。
        let inject_tx = cmd_tx.clone();
        let started_wait = started.clone();
        tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(2), started_wait.notified())
                .await
                .expect("工具应已启动");
            inject_tx
                .send(Command::InjectUserMessage {
                    message_id: "injected-1".to_string(),
                    content: vec![tiangong_types::ContentBlock::text("改成先做另一件事")],
                })
                .unwrap();
        });
        let result = execute_turn(&mut ctx, &mut cmd_rx).await;
        assert!(
            matches!(result.outcome, TurnExecutionOutcome::Success),
            "新意图执行应成功，实际: {:?}",
            result.outcome
        );
        // 消息已进入 session（校验+事务保存）。
        assert!(
            ctx.session
                .messages
                .iter()
                .any(|m| m.id == "injected-1" && m.role == MessageRole::User),
            "注入的消息应保存进 session"
        );
        // 保存成功后才发 UserMessage 确认（ALR-202 的同轮部分）。
        assert!(
            stream_rx.try_iter().any(|e| matches!(e,
                StreamEvent::UserMessage { message_id, .. } if message_id == "injected-1")),
            "保存成功后应发送 UserMessage 确认"
        );
        // 被中断的工具调用协议已闭合：call_1 有对应的失败结果消息。
        let call_closed = ctx
            .session
            .messages
            .iter()
            .any(|m| m.tool_call_id.as_deref() == Some("call_1") && m.tool_result_is_error);
        assert!(call_closed, "被中断的工具调用应有失败结果（ALR-110）");
    }

    /// ALR-104：Summary 进行中被引导消息打断——部分输出降级为 React 过程消息，
    /// 不得保持最终 Summary 身份。
    ///
    /// 说明：集成层面"Summary 流式中途被打断"无法用 wiremock 确定性构造（SSE body
    /// 完整发送即结束，无中途停顿点；整体延迟则注入先于任何 chunk、部分输出为空）。
    /// 因此直接单测降级语义的核心：被中断的 Summary 部分输出以 React 过程消息落盘，
    /// 不保持最终 Summary 身份。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn interrupted_llm_summary_output_persists_as_react_phase() {
        use super::super::phase::LlmPurpose;
        use super::persist_interrupted_llm_output;

        let server = MockServer::start().await;
        let mut harness = TestHarness::new(&server, Vec::new(), HashMap::new());
        let summary_purpose = LlmPurpose::Summary {
            iteration: 1,
            request_injection_generation: 0,
        };
        persist_interrupted_llm_output(
            &mut harness.ctx,
            &summary_purpose,
            "summary-partial-1",
            "这是被打断的半截总结",
            "",
        );
        let message = harness
            .ctx
            .session
            .messages
            .iter()
            .find(|m| m.id == "summary-partial-1")
            .expect("部分总结应以过程消息落盘");
        assert_eq!(
            message.phase,
            crate::session::MessagePhase::React,
            "被中断的 Summary 部分输出必须降级为 React（ALR-104），实际: {:?}",
            message.phase
        );
        assert!(message.text_content().contains("半截总结"));

        // ForceFinal 被中断：无可靠完整内容，不落盘。
        let force_purpose = LlmPurpose::ForceFinal {
            request_injection_generation: 0,
            summary_error: None,
        };
        persist_interrupted_llm_output(
            &mut harness.ctx,
            &force_purpose,
            "force-partial-1",
            "半截强制回复",
            "",
        );
        assert!(
            !harness
                .ctx
                .session
                .messages
                .iter()
                .any(|m| m.id == "force-partial-1"),
            "被中断的 ForceFinal 输出不应提交"
        );
    }

    /// ALR-107（多消息）：注入引导消息后，最终 turn_status 写入最新（注入的）
    /// 用户消息，原始消息不被覆盖；磁盘重载后一致。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn final_status_anchors_to_injected_latest_user_message() {
        use super::super::turn::run_turn;

        let server = MockServer::start().await;
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let mut overrides: HashMap<String, Arc<dyn ToolOverrideHandler>> = HashMap::new();
        overrides.insert(
            "paused_probe".to_string(),
            Arc::new(PausedTool {
                started: started.clone(),
                release: release.clone(),
            }),
        );
        mount_sse(
            &server,
            vec![
                tool_call_chunk("call_1", "paused_probe", "{}"),
                usage_chunk(15, 3),
            ],
        )
        .await;
        mount_sse(
            &server,
            vec![text_delta_chunk("按新要求完成。"), usage_chunk(20, 4)],
        )
        .await;
        // 新意图首轮后 request_round>0，文本回答进入完成度检查 → Summary 完成。
        mount_sse(
            &server,
            vec![text_delta_chunk("已完成。"), usage_chunk(25, 3)],
        )
        .await;

        let harness = TestHarness::new(&server, vec![tool_spec("paused_probe")], overrides);
        let TestHarness {
            ctx,
            stream_rx,
            cmd_tx: _,
            storage_root,
            ..
        } = harness;
        let session_id = ctx.session.id.clone();
        // 注入走独立通道任务：等工具启动后把命令投给 run_turn 持有的接收端。
        // 生产中发送端由 TurnTask 注册表持有；测试克隆一份保持通道开启，否则任务
        // 结束后通道关闭会被 execute_turn 当作取消。
        let started_wait = started.clone();
        let (inject_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<Command>();
        let _keep_channel_open = inject_tx.clone();
        let inject_task = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(2), started_wait.notified())
                .await
                .expect("工具应已启动");
            inject_tx
                .send(Command::InjectUserMessage {
                    message_id: "injected-anchor".to_string(),
                    content: vec![tiangong_types::ContentBlock::text("请改用另一方案")],
                })
                .unwrap();
        });
        run_turn(ctx, cmd_rx).await;
        let _ = inject_task.await;

        // 磁盘重载验证：最新用户消息（注入的）有 turn_status，原始消息无。
        let reloaded =
            Session::load_from_storage(&storage_root, &session_id).expect("重载 session");
        let latest = reloaded
            .messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::User)
            .expect("应有用户消息");
        assert_eq!(latest.id, "injected-anchor", "最新用户消息应为注入的消息");
        assert!(
            latest.turn_status.is_some(),
            "最终状态应写入最新（注入的）用户消息（ALR-107）"
        );
        let first = reloaded
            .messages
            .iter()
            .find(|m| m.role == MessageRole::User)
            .expect("应有原始用户消息");
        assert_eq!(first.text_content(), "你好", "原始用户消息应保持");
        assert!(
            first.turn_status.is_none(),
            "原始用户消息不应被写入最终状态（ALR-107）"
        );
        // 唯一终态保持（ALR-109）。
        let terminal: Vec<String> = stream_rx
            .try_iter()
            .filter_map(|e| match e {
                StreamEvent::Done { .. } => Some("done".to_string()),
                StreamEvent::Error { message } => Some(format!("error: {message}")),
                _ => None,
            })
            .collect();
        assert_eq!(
            terminal,
            vec!["done".to_string()],
            "一个物理 turn 只发一次 Done（ALR-109），实际终态: {terminal:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reasoning_effort_update_applies_to_next_model_request() {
        let server = MockServer::start().await;
        mount_sse(
            &server,
            vec![
                tool_call_chunk("call_1", "paused_probe", "{}"),
                usage_chunk(15, 3),
            ],
        )
        .await;
        mount_sse(
            &server,
            vec![text_delta_chunk("好的，已完成。"), usage_chunk(20, 4)],
        )
        .await;

        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let mut overrides: HashMap<String, Arc<dyn ToolOverrideHandler>> = HashMap::new();
        overrides.insert(
            "paused_probe".to_string(),
            Arc::new(PausedTool {
                started: started.clone(),
                release: release.clone(),
            }),
        );
        let mut harness = TestHarness::new(&server, vec![tool_spec("paused_probe")], overrides);
        let cmd_tx = harness.cmd_tx.clone();
        let update_task = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(2), started.notified())
                .await
                .expect("工具应在期限内开始执行");
            cmd_tx
                .send(Command::SetReasoningEffort("max".to_string()))
                .expect("运行中的 turn 应接收思考强度更新");
            release.notify_one();
        });

        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;
        update_task.await.unwrap();

        assert!(matches!(result.outcome, TurnExecutionOutcome::Success));
        assert_eq!(harness.ctx.agent_config.reasoning_effort, "max");
        assert_eq!(harness.ctx.session.reasoning_effort.as_deref(), Some("max"));

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        let first_body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        let second_body: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
        assert!(first_body.get("reasoning_effort").is_none());
        assert_eq!(first_body["thinking"]["type"], "disabled");
        assert_eq!(second_body["reasoning_effort"], "max");
        assert_eq!(second_body["thinking"]["type"], "enabled");
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
            ..
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
            ..
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
            ..
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
    async fn all_invalid_tool_calls_are_filtered_then_regenerated() {
        let server = MockServer::start().await;
        mount_sse(
            &server,
            vec![
                tool_call_chunk("call_invalid", "echo", "{}"),
                usage_chunk(9, 2),
            ],
        )
        .await;
        mount_sse(
            &server,
            vec![
                tool_call_chunk("call_valid", "echo", r#"{"message":"schema 已修正"}"#),
                usage_chunk(11, 3),
            ],
        )
        .await;
        mount_sse(
            &server,
            vec![text_delta_chunk("已完成。"), usage_chunk(13, 4)],
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
        let tool = ToolSpec {
            name: "echo".to_string(),
            description: "回显消息".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"message": {"type": "string"}},
                "required": ["message"],
                "additionalProperties": false
            }),
        };
        let mut harness = TestHarness::new(&server, vec![tool], overrides);

        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;

        assert!(matches!(result.outcome, TurnExecutionOutcome::Success));
        assert_eq!(result.usage.total_tokens, 42);
        {
            let calls = invocations.lock().unwrap();
            assert_eq!(calls.len(), 1, "被剔除的调用不应执行");
            assert_eq!(calls[0].id, "call_valid");
        }

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 3);
        let second_body: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
        let second_body_text = second_body.to_string();
        assert!(second_body_text.contains("invalid_tool_calls"));
        assert!(second_body_text.contains("required"));
        assert!(harness.ctx.session.messages.iter().all(|message| {
            !message
                .tool_calls
                .iter()
                .any(|call| call.id == "call_invalid")
        }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn invalid_summary_tool_calls_trigger_another_bounded_iteration() {
        let server = MockServer::start().await;
        mount_sse(
            &server,
            vec![
                tool_call_chunk("call_invalid_summary", "echo", "{}"),
                usage_chunk(9, 2),
            ],
        )
        .await;
        mount_sse(
            &server,
            vec![text_delta_chunk("[DONE]\n已完成。"), usage_chunk(11, 3)],
        )
        .await;

        let tool = ToolSpec {
            name: "echo".to_string(),
            description: "回显消息".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"message": {"type": "string"}},
                "required": ["message"],
                "additionalProperties": false
            }),
        };
        let mut harness = TestHarness::new(&server, vec![tool], HashMap::new());
        harness.ctx.max_tool_rounds = 0;

        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;

        assert!(matches!(result.outcome, TurnExecutionOutcome::Success));
        assert_eq!(result.usage.total_tokens, 25);
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        let second_body: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
        let second_body_text = second_body.to_string();
        assert!(second_body_text.contains("invalid_tool_calls"));
        assert!(second_body_text.contains("schema 位置=#/required"));
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
    async fn summary_tool_calls_continue_without_consuming_summary_iteration() {
        let server = MockServer::start().await;
        mount_sse(
            &server,
            vec![text_delta_chunk("[调用工具:测试]"), usage_chunk(10, 1)],
        )
        .await;
        mount_sse(
            &server,
            vec![
                text_delta_chunk(
                    "先跑验证：\n\n<｜｜DSML｜｜tool_calls>\n<｜｜DSML｜｜invoke name=\"verify\">\n</｜｜DSML｜｜invoke>\n</｜｜DSML｜｜tool_calls>",
                ),
                usage_chunk(12, 2),
            ],
        )
        .await;
        mount_sse(
            &server,
            vec![text_delta_chunk("[调用工具:仍需总结]"), usage_chunk(20, 3)],
        )
        .await;
        mount_sse(
            &server,
            vec![text_delta_chunk("[DONE]\n任务完成"), usage_chunk(24, 4)],
        )
        .await;

        let invocations = Arc::new(Mutex::new(Vec::new()));
        let mut overrides: HashMap<String, Arc<dyn ToolOverrideHandler>> = HashMap::new();
        overrides.insert(
            "verify".to_string(),
            Arc::new(EchoTool {
                invocations: invocations.clone(),
            }),
        );
        let mut harness = TestHarness::new_with_protocol(
            &server,
            ProviderProtocol::DeepSeek,
            vec![tool_spec("verify")],
            overrides,
            Vec::new(),
        );
        // 第一轮 ReAct 已到上限；总结工具调用必须重置阶段，才能继续处理工具结果。
        harness.ctx.max_tool_rounds = 1;
        harness.ctx.max_outer_iterations = 1;

        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;

        assert!(
            matches!(result.outcome, TurnExecutionOutcome::Success),
            "DeepSeek 总结工具续作结果: {:?}",
            result.outcome
        );
        assert_eq!(invocations.lock().unwrap().len(), 1);
        let summary_iterations = harness
            .stream_rx
            .try_iter()
            .filter_map(|event| match event {
                StreamEvent::PhaseChanged { phase, iteration } if phase == "summary" => {
                    Some(iteration)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(summary_iterations, vec![1, 1]);
        assert!(harness.ctx.session.messages.iter().any(|message| {
            message.role == MessageRole::Tool
                && message.tool_name.as_deref() == Some("verify")
                && message.text_content().contains("done")
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
            ..
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
    async fn forces_final_response_when_continuation_limit_is_reached() {
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
    async fn force_final_does_not_commit_empty_reply_from_invalid_tool_call() {
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
            vec![
                tool_call_chunk("call_invalid_final", "echo", "{}"),
                usage_chunk(8, 3),
            ],
        )
        .await;

        let tool = ToolSpec {
            name: "echo".to_string(),
            description: "回显消息".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"message": {"type": "string"}},
                "required": ["message"],
                "additionalProperties": false
            }),
        };
        let mut harness = TestHarness::new(&server, vec![tool], HashMap::new());
        harness.ctx.max_outer_iterations = 1;

        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;

        assert_eq!(result.usage.total_tokens, 26);
        match result.outcome {
            TurnExecutionOutcome::Failed(message) => {
                assert!(message.contains("call_invalid_final"));
                assert!(message.contains("schema 位置=#/required"));
            }
            outcome => panic!("异常最终工具调用不应被当作成功：{outcome:?}"),
        }
        assert!(harness.ctx.session.messages.iter().all(|message| {
            message.id != "call_invalid_final"
                && !(message.phase == crate::session::MessagePhase::Summary
                    && message.text_content().trim().is_empty())
        }));
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
