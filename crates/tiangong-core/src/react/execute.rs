//! 单轮 Agent Loop 的执行过程。
//!
//! 本模块只负责从已构建的 TurnContext 执行模型请求、工具调用与总结阶段；
//! turn 的插件生命周期、状态提交和最终持久化由 react/turn.rs 负责。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use tokio::sync::mpsc as tokio_mpsc;

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
    build_thinking_config, emit_token_usage, persist_error, select_client_for_request,
};
use crate::react::message::*;
use crate::runtime::LlmOutputRecord;
use crate::session::{Message, MessagePhase, MessageRole};
use crate::stream_throttle::{StreamTextKind, ThrottledStreamSink};
use crate::tool::ToolResult;
use crate::turn_context::TurnContext;
use tiangong_types::{DeferredToolInjection, StreamEvent, StreamToolCall};

use super::command::{CommandEffect, Deferred, handle_command};
use super::compression::observed_total_tokens;
use super::outcome::TurnExecutionResult;
use super::phase::{ActiveLlm, ExecutionBudget, ExecutionPhase, LlmPurpose, ToolBatchState};
use super::request;
use super::tools;

#[derive(Default)]
pub(super) struct ToolCallHistory {
    successful_keys: HashSet<String>,
    failed_calls: HashMap<String, String>,
    failed_names: HashSet<String>,
}

impl ToolCallHistory {
    pub(super) fn clear(&mut self) {
        self.successful_keys.clear();
        self.failed_calls.clear();
        self.failed_names.clear();
    }
}

pub(super) struct ToolInjectionBuffer {
    session_deferred: Vec<DeferredToolInjection>,
    pending: VecDeque<DeferredToolInjection>,
    generation: u64,
}

impl ToolInjectionBuffer {
    pub(super) fn new(ctx: &TurnContext) -> Self {
        Self {
            session_deferred: ctx.session.deferred_tool_injections.clone(),
            pending: VecDeque::new(),
            generation: 0,
        }
    }

    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    /// 接收即向宿主发布待处理快照；真正写入 Session 仍等待当前子过程释放 ctx。
    pub(super) fn receive(
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
    pub(super) fn commit(&mut self, ctx: &mut TurnContext) {
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
pub(super) fn set_runtime_trust_mode(
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

pub(super) fn append_invalid_tool_calls_context(
    ctx: &mut TurnContext,
    invalid_calls: &[InvalidToolCall],
) {
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

pub(super) enum ToolPreflightOutcome {
    Execute {
        args_summary: String,
        dedupe_key: String,
    },
    Skip {
        needs_recovery: bool,
    },
}

pub(super) fn prepare_tool_call(
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

pub(super) fn record_parallel_duplicate_tool_call(ctx: &mut TurnContext, call: &ToolCall) {
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

pub(super) fn record_rejected_tool_call(
    ctx: &mut TurnContext,
    call: &ToolCall,
    args_summary: &str,
) {
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

pub(super) struct CompletedToolCall<'a> {
    pub(super) call: &'a ToolCall,
    pub(super) args_summary: &'a str,
    pub(super) dedupe_key: String,
    pub(super) result: &'a ToolResult,
    pub(super) duration_ms: u64,
}

pub(super) fn record_completed_tool_call(
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

pub(super) fn append_failure_recovery_prompt(
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
/// 3.1）+ 预算 + 累计用量 + 工具去重记录 + 工具义务契约。阶段持有的活动资源
/// （模型请求/工具任务/审批/压缩）都在 phase 变体内，不再有并列活动 `Option`
/// （ALR-001）。
pub(super) struct AgentLoopState {
    pub(super) phase: Option<ExecutionPhase>,
    pub(super) budget: ExecutionBudget,
    pub(super) accumulated_usage: TokenUsage,
    pub(super) tool_history: ToolCallHistory,
    /// 最近一次模型响应/工具批次的观测 token 总量（请求前压力检查的信号）。
    pub(super) last_observed_tokens: usize,
    /// 待处理的上下文溢出恢复（请求错误策略在下次请求前消费，ALR-304）。
    pub(super) pending_context_recovery: Option<String>,
    /// 候选最终答复的消息 ID：进入封口时记录，**真正提交时**才标记
    /// Summary 相位——封口窗口被引导/注入撤销时回收，不提前定格。
    pub(super) pending_summary_msg_id: Option<String>,
}

impl AgentLoopState {
    fn new(ctx: &TurnContext) -> Self {
        Self {
            phase: Some(ExecutionPhase::NeedModel),
            budget: ExecutionBudget::default(),
            accumulated_usage: TokenUsage::default(),
            tool_history: ToolCallHistory::default(),
            last_observed_tokens: ctx.session.current_tokens,
            pending_context_recovery: None,
            pending_summary_msg_id: None,
        }
    }

    /// 取出当前阶段（ALR-205：取出后所有退出路径必须 install 新阶段或形成终态）。
    pub(super) fn take_phase(&mut self) -> ExecutionPhase {
        self.phase.take().expect("take_phase 时阶段必须存在")
    }

    /// 安装新阶段。install 前 phase 必须为 None（已 take），避免双阶段并存。
    pub(super) fn install_phase(&mut self, phase: ExecutionPhase) {
        debug_assert!(
            self.phase.is_none(),
            "install_phase 前必须先 take，避免双阶段并存"
        );
        self.phase = Some(phase);
    }

    /// 新用户意图（steer 注入）：清工具去重记录。
    pub(super) fn reset_tool_history(&mut self) {
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

pub(super) fn persist_streamed_react_message(
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

/// ReAct 文本回复的后续去向。
enum ReactTextDisposition {
    /// 有效最终答复：提交（PendingFinish）。
    Complete,
    /// 无效输出（空回复/合成占位符）：明确失败。
    InvalidOutput,
}

fn handle_react_text_response(
    ctx: &mut TurnContext,
    pending_msg_id: &str,
    response: &ModelFunctionResponse,
) -> ReactTextDisposition {
    // 合成占位符不是有效输出，不保存为消息。
    if is_synthetic_tool_call_placeholder(&response.text) {
        return ReactTextDisposition::InvalidOutput;
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
            stage: "react-round".to_string(),
            content: response.text.clone(),
            reasoning_content: response.reasoning_content.clone(),
            tool_calls: Vec::new(),
            usage: response.usage.clone(),
        }),
        response.reasoning_content.clone(),
    );
    ctx.session.persist_to_disk();

    // 附件内容不进入模型请求（模型看不见），自然会调用读取插件处理——
    // 工具使用由模型基于上下文自主决策（Agent 的根基），Loop 不做义务门控。
    if response.text.trim().is_empty() {
        return ReactTextDisposition::InvalidOutput;
    }
    ReactTextDisposition::Complete
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
            // 记录候选答复 ID；Summary 相位在 PendingFinish 真正提交时标记
            //（封口窗口被继续命令撤销时回收，候选不得提前定格为最终答复）。
            state.pending_summary_msg_id = Some(pending_msg_id.clone());
            emit_session_message_upsert(ctx, &pending_msg_id);
            ctx.session.persist_to_disk();
            ExecutionPhase::PendingFinish(TurnExecutionResult::success(
                state.accumulated_usage.clone(),
            ))
        }
        ReactTextDisposition::InvalidOutput => {
            // 模型未产生有效输出：明确失败，不把空回复发布为最终答复。
            let reason = "模型未产生有效回复".to_string();
            persist_error(ctx, &reason);
            ExecutionPhase::PendingFinish(TurnExecutionResult::failed(
                state.accumulated_usage.clone(),
                reason,
            ))
        }
    }
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
    let mut trust_mode = ctx.trust_mode;
    let mut injections = ToolInjectionBuffer::new(ctx);
    let mut state = AgentLoopState::new(ctx);
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
            match handle_command(
                cmd_rx,
                deferred_command,
                ctx,
                &mut state,
                &mut injections,
                &mut trust_mode,
                &plugins,
                &stream_tx,
                context_limit,
            )
            .await
            {
                CommandEffect::KeepCurrent => {}
                CommandEffect::ToPhase(phase) => state.install_phase(phase),
                CommandEffect::Terminate(result) => break 'agent_loop result,
            }
            continue 'agent_loop;
        }

        // ── 阶段驱动（take → drive → install）──
        let phase = state.take_phase();
        match phase {
            // ── Ready：需要下一次 ReAct 模型请求 ──
            ExecutionPhase::NeedModel => {
                if state.budget.react_rounds_in_phase >= ctx.max_tool_rounds {
                    // 安全限制（ALR-305）：连续工具调用轮次达到上限时明确失败，
                    // 不强迫模型生成一个看起来完成的回答。
                    let message = format!(
                        "连续工具执行轮次达到安全上限（{}），本轮已终止",
                        ctx.max_tool_rounds
                    );
                    persist_error(ctx, &message);
                    state.install_phase(ExecutionPhase::PendingFinish(
                        TurnExecutionResult::failed(state.accumulated_usage.clone(), message),
                    ));
                    continue 'agent_loop;
                }
                // ── 请求前策略（ALR-303/304）──
                // 上下文溢出恢复优先于常规压力压缩；两者都在请求前内联完成，
                // 压缩不再形成 Agent 顶层阶段。压缩期间到达的命令交还驱动处理。
                let recovery = state.pending_context_recovery.take();
                let preparation = match recovery {
                    Some(error_message) => {
                        tracing::warn!(session_id = %ctx.session.id, "检测到上下文超限，尝试强制压缩");
                        match request::recover_context_overflow(
                            ctx,
                            &mut state.accumulated_usage,
                            &context_organizer,
                            cmd_rx,
                        )
                        .await
                        {
                            request::ContextRecovery::Retriable => None,
                            request::ContextRecovery::Exhausted => {
                                persist_error(ctx, format!("ReAct 循环请求失败：{error_message}"));
                                state.install_phase(ExecutionPhase::PendingFinish(
                                    TurnExecutionResult::failed(
                                        state.accumulated_usage.clone(),
                                        error_message,
                                    ),
                                ));
                                continue 'agent_loop;
                            }
                            request::ContextRecovery::Interrupted(deferred_command) => {
                                Some(deferred_command)
                            }
                        }
                    }
                    None => {
                        match request::prepare_before_request(
                            ctx,
                            &mut state.accumulated_usage,
                            &context_organizer,
                            state.last_observed_tokens,
                            cmd_rx,
                        )
                        .await
                        {
                            request::RequestPreparation::Ready => None,
                            request::RequestPreparation::Interrupted(deferred_command) => {
                                Some(deferred_command)
                            }
                        }
                    }
                };
                if let Some(deferred_command) = preparation {
                    // 归还阶段：命令处理器（如 interrupt_active_work）会 take 当前
                    // 阶段，KeepCurrent 语义假设阶段仍在驱动手中。
                    state.install_phase(ExecutionPhase::NeedModel);
                    deferred = Some(deferred_command);
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

            // ── Ready：暂定完成，终态封口后提交 ──
            // 阶段变体只含结果（结构上保证不持有主循环活动资源，不变量 4）。
            // 封口（ALR-201）：原子 Accepting → Sealing，此后新命令不再入当前
            // 队列（send_command 返回 false，deliver 把用户消息排入下一轮）；
            // 封口前已入队的命令在此排空处理：决定性命令（取消/关闭/保存失败）
            // 直接形成终态；继续命令（引导/工具注入）恢复 Accepting 并重启；
            // 无决定性命令 → Committing，刷新为最新累计用量后提交（ALR-111）。
            ExecutionPhase::PendingFinish(result) => {
                // 命令处理（取消/引导/审批分支）会 take 当前阶段：先装回自身，
                // 否则排空窗口到达的决定性命令在 take_phase 断言上崩溃。
                state.install_phase(ExecutionPhase::PendingFinish(result));
                // 测试同步点（仅测试构建）：封口前屏障——候选答复已生成、
                // 提交尚未开始的精确窗口，供集成测试投递引导/交接消息。
                #[cfg(test)]
                crate::core::test_support::seal_barrier(&ctx.session.id, "seal", cmd_rx).await;
                crate::react::inbox::begin_seal(&ctx.session.id);
                loop {
                    let next = match cmd_rx.try_recv() {
                        Ok(command) => Some(Deferred::Command(command)),
                        Err(tokio_mpsc::error::TryRecvError::Empty) => None,
                        Err(tokio_mpsc::error::TryRecvError::Disconnected) => {
                            Some(Deferred::Closed)
                        }
                    };
                    let Some(deferred_command) = next else { break };
                    match handle_command(
                        cmd_rx,
                        deferred_command,
                        ctx,
                        &mut state,
                        &mut injections,
                        &mut trust_mode,
                        &plugins,
                        &stream_tx,
                        context_limit,
                    )
                    .await
                    {
                        CommandEffect::KeepCurrent => {}
                        CommandEffect::ToPhase(phase) => {
                            // 继续命令：恢复接收，回到主循环驱动新阶段。
                            crate::react::inbox::reopen(&ctx.session.id);
                            state.install_phase(phase);
                            continue 'agent_loop;
                        }
                        CommandEffect::Terminate(terminal) => {
                            crate::react::inbox::commit_ingress(&ctx.session.id);
                            break 'agent_loop terminal;
                        }
                    }
                }
                // 候选完成检查（design.md 4.2.4）：封口竞态到达的 `next_step`
                //（inject 在 Sealing 期间被拒后落入 Inbox）原子领取——有积压
                // 则撤销暂定结果，注入生效后继续处理，不得滞留或丢失。
                let sealed_steps = crate::react::inbox::take_pending_steps(&ctx.session.id);
                if !sealed_steps.is_empty() {
                    crate::react::inbox::reopen(&ctx.session.id);
                    for command in sealed_steps {
                        crate::react::inbox::send_command(&ctx.session.id, command);
                    }
                    // 撤销提交：装回 NeedModel 前清出占位的 PendingFinish。
                    state.take_phase();
                    state.install_phase(ExecutionPhase::NeedModel);
                    continue 'agent_loop;
                }
                // 排空完毕无继续命令：取回占位的暂定结果，正式提交。
                let ExecutionPhase::PendingFinish(pending_result) = state.take_phase() else {
                    unreachable!("排空循环保持 PendingFinish 在位");
                };
                // 提交时刻标记候选答复为最终答复（此前撤销路径不会到达这里）。
                if let Some(msg_id) = state.pending_summary_msg_id.take() {
                    if let Some(message) = ctx
                        .session
                        .messages
                        .iter_mut()
                        .find(|message| message.id == msg_id)
                    {
                        message.phase = MessagePhase::Summary;
                    }
                    emit_session_message_upsert(ctx, &msg_id);
                }
                crate::react::inbox::commit_ingress(&ctx.session.id);
                // 测试同步点（仅测试构建）：提交已确定（Committing），此后
                // 到达的用户消息只能占用待执行单槽——供关闭/交接/Busy 测试。
                #[cfg(test)]
                crate::core::test_support::seal_barrier(&ctx.session.id, "commit", cmd_rx).await;
                let mut pending_result = pending_result;
                pending_result.usage = state.accumulated_usage.clone();
                tracing::debug!(
                    session_id = %ctx.session.id,
                    event = "ingress_committed",
                    detail = state.budget.debug_summary(),
                    "终态封口完成，提交暂定结果（ALR-201/301）"
                );
                break 'agent_loop pending_result;
            }

            // ── Waiting：模型请求（流式驱动，命令与 chunk 双路 select）──
            ExecutionPhase::WaitingModel(mut active) => {
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
                            &stream_tx,
                            purpose,
                            pending_msg_id,
                            streamed_text,
                            streamed_reasoning,
                            response_result,
                        );
                        let next_phase = match next {
                            NextStep::Phase(phase) => phase,
                            NextStep::ExecuteTools(batch) => {
                                match tools::execute_tool_batch(
                                    ctx,
                                    &mut state,
                                    &mut injections,
                                    &mut trust_mode,
                                    &plugins,
                                    &stream_tx,
                                    context_limit,
                                    cmd_rx,
                                    batch,
                                )
                                .await
                                {
                                    tools::ToolBatchOutcome::Completed => {
                                        ExecutionPhase::NeedModel
                                    }
                                    tools::ToolBatchOutcome::Interrupted(deferred_command) => {
                                        // 归还阶段后交还统一命令处理（命令处理器会
                                        // take 当前阶段）。
                                        state.install_phase(ExecutionPhase::NeedModel);
                                        deferred = Some(deferred_command);
                                        continue 'agent_loop;
                                    }
                                }
                            }
                        };
                        state.install_phase(next_phase);
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
    }
}

/// 模型请求完成后的去向：下一阶段，或整批工具调用（由调用方 await
/// 工具流水线，ALR-301）。
enum NextStep {
    Phase(ExecutionPhase),
    ExecuteTools(ToolBatchState),
}

/// 模型请求完成后的统一处理：归一化响应并产出下一去向。
///
/// 这是旧 select「chunk 通道关闭」分支的主体：错误分流（上下文超限 → 压缩重试）、
/// 工具调用 → 工具流水线、文本回复 → 候选完成门控（无义务完成 / 有义务修复）。
#[allow(clippy::too_many_arguments)]
fn complete_llm_request(
    ctx: &mut TurnContext,
    state: &mut AgentLoopState,
    injections: &mut ToolInjectionBuffer,
    stream_tx: &std::sync::mpsc::Sender<StreamEvent>,
    purpose: LlmPurpose,
    pending_msg_id: String,
    streamed_text: String,
    streamed_reasoning: String,
    response_result: anyhow::Result<ModelFunctionResponse>,
) -> NextStep {
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
                        // 上下文溢出：交由下一次请求前的恢复策略强制压缩（ALR-304）。
                        state.pending_context_recovery = Some(error_message);
                        return NextStep::Phase(ExecutionPhase::NeedModel);
                    }
                    injections.commit(ctx);
                    persist_error(ctx, format!("ReAct 循环请求失败：{error_message}"));
                    return NextStep::Phase(ExecutionPhase::PendingFinish(
                        TurnExecutionResult::failed(state.accumulated_usage.clone(), error_message),
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
            // 记录观测压力，交给下一次请求前策略统一判断（ALR-303）；
            // 同步到 session，跨 turn 的请求前策略也能读取该信号。
            let observed_tokens = observed_total_tokens(&response.usage);
            state.last_observed_tokens = observed_tokens;
            ctx.session.current_tokens = observed_tokens;

            if response.tool_calls.is_empty() && !response.invalid_tool_calls.is_empty() {
                append_invalid_tool_calls_context(ctx, &response.invalid_tool_calls);
                NextStep::Phase(ExecutionPhase::NeedModel)
            } else if response.tool_calls.is_empty() {
                let disposition = handle_react_text_response(ctx, &pending_msg_id, &response);
                NextStep::Phase(finish_react_text(
                    ctx,
                    state,
                    injections,
                    pending_msg_id,
                    disposition,
                    request_injection_generation,
                ))
            } else {
                let calls = record_tool_calls(
                    ctx,
                    &pending_msg_id,
                    &response,
                    format!("react-round-{}", state.budget.request_round),
                );
                NextStep::ExecuteTools(ToolBatchState {
                    calls: calls.into_iter().enumerate().collect(),
                    ready_tools: Vec::new(),
                    prepared_keys: HashSet::new(),
                    invalid_tool_calls: response.invalid_tool_calls,
                    response_usage: response.usage,
                    needs_failure_recovery: false,
                })
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

    /// 请求前压缩保留模型可见的续接消息：上一 turn 观测压力超阈值，下一 turn
    /// 在发起模型请求前压缩（ALR-303），摘要与 resume 持久化到磁盘。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn compression_persists_summary_and_keeps_recent_interaction() {
        let server = MockServer::start().await;
        // 第一段：大用量文本完成，建立跨 turn 的压力信号。
        mount_sse(
            &server,
            vec![text_delta_chunk("最终回答。"), usage_chunk(185_900, 5)],
        )
        .await;
        // 第二段请求前压缩；压缩后的模型请求。
        mount_completion(
            &server,
            "[[CURRENT_TASK]]\n当前任务已完成\n[[SUMMARY]]\n历史摘要",
            "stop",
            100,
            20,
            None,
        )
        .await;
        mount_sse(
            &server,
            vec![text_delta_chunk("继续回答。"), usage_chunk(30, 5)],
        )
        .await;

        let mut harness = TestHarness::new(&server, Vec::new(), HashMap::new());
        let first = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;
        assert!(matches!(first.outcome, TurnExecutionOutcome::Success));
        assert_eq!(first.usage.total_tokens, 185_905);

        // 新 turn：请求前压力检查触发压缩（新增用户消息成为续接的当前任务）。
        harness
            .ctx
            .session
            .append_message(MessageRole::User, "继续提出新问题");
        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;

        assert!(matches!(result.outcome, TurnExecutionOutcome::Success));
        assert_eq!(result.usage.total_tokens, 155);
        assert_eq!(
            harness.ctx.session.context_summary.as_deref(),
            Some("历史摘要")
        );
        // 压缩只是 session 的一次调整：最近交互保留（锚点用户消息承载当前任务），
        // 不注入合成续接消息。
        let context = harness.ctx.session.context();
        assert_eq!(context[0].role, MessageRole::System);
        assert_eq!(context[1].text_content(), "继续提出新问题");
        assert!(
            harness
                .ctx
                .session
                .messages
                .iter()
                .all(|message| message.phase != crate::session::MessagePhase::CompressedResume),
            "压缩不注入合成续接消息"
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
            persisted.context_summary.as_deref(),
            Some("历史摘要"),
            "摘要边界应持久化"
        );

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
        harness
            .ctx
            .session
            .append_message(MessageRole::User, "处理 latest.txt");
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
        assert_eq!(harness.ctx.session.summary_up_to, 3);
        let context = harness.ctx.session.context();
        assert_eq!(context[0].role, MessageRole::System);
        // 锚点被折叠：注入 LLM 可见、用户不可见的锚点消息（用户请求原文），
        // 随后是完整保留的最近工具批次。
        assert_eq!(context[1].role, MessageRole::User);
        assert_eq!(
            context[1].phase,
            crate::session::MessagePhase::CompressedResume
        );
        assert!(matches!(
            &context[1].content[0],
            crate::session::ContentBlock::ModelInstruction { text }
                if text.contains("处理 latest.txt")
        ));
        assert_eq!(context[2].role, MessageRole::Assistant);
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

    /// 截断的压缩结果不得推进摘要边界：finish_reason=length 视为失败，
    /// session 保持原状（请求前压缩路径，ALR-303）。
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
        mount_sse(
            &server,
            vec![text_delta_chunk("继续回答。"), usage_chunk(30, 5)],
        )
        .await;

        let mut harness = TestHarness::new(&server, Vec::new(), HashMap::new());
        let first = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;
        assert!(matches!(first.outcome, TurnExecutionOutcome::Success));
        harness
            .ctx
            .session
            .append_message(MessageRole::User, "继续提问");
        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;

        assert!(matches!(result.outcome, TurnExecutionOutcome::Success));
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

    /// 压缩结果持久化失败时保持原压缩状态：不发 Auto 成功事件，仅 Failed 事件
    ///（请求前压缩路径，ALR-303）。
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
        mount_sse(
            &server,
            vec![text_delta_chunk("继续回答。"), usage_chunk(30, 5)],
        )
        .await;

        let invalid_root = tempfile::tempdir().unwrap();
        let blocking_file = invalid_root.path().join("not-a-directory");
        std::fs::write(&blocking_file, "blocked").unwrap();
        let mut harness = TestHarness::new(&server, Vec::new(), HashMap::new());
        harness.ctx.session.bind_storage_root(blocking_file);

        let _ = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;
        harness
            .ctx
            .session
            .append_message(MessageRole::User, "继续提问");
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

    /// 请求前压缩期间取消：压缩被中止（Cancelled 事件），turn 以取消终态结束
    /// 且不应用任何压缩结果（ALR-303/306）。
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

        let mut harness = TestHarness::new(&server, Vec::new(), HashMap::new());
        let _ = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;
        harness.drain_stream();
        harness
            .ctx
            .session
            .append_message(MessageRole::User, "继续提问");

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

        let defer_never =
            |command: crate::core::command::Command| -> Result<(), crate::core::command::Command> {
                Err(command)
            };
        tokio::time::timeout(
            Duration::from_secs(2),
            crate::react::compression::run_manual_context_compression(ctx, cmd_rx, &defer_never),
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

        let defer_never =
            |command: crate::core::command::Command| -> Result<(), crate::core::command::Command> {
                Err(command)
            };
        tokio::time::timeout(
            Duration::from_secs(2),
            crate::react::compression::run_manual_context_compression(ctx, cmd_rx, &defer_never),
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

    /// 工具调用路径:模型先调用工具 → 工具执行 → 模型给出文本回复 → 候选完成
    /// 门控（无工具义务）通过 → `Success`。
    ///
    /// 覆盖最小模型—工具循环的完整链路（任务 15：不再进入总结阶段）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn runs_tool_then_completes() {
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
        // 2) 工具执行后:文本回复(无工具义务 → 直接完成,问号结尾不影响)。
        mount_sse(
            &server,
            vec![
                text_delta_chunk("结果还需要我做什么吗?"),
                usage_chunk(25, 5),
            ],
        )
        .await;

        let mut harness = TestHarness::new(&server, tools, overrides);
        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;

        assert!(
            matches!(result.outcome, TurnExecutionOutcome::Success),
            "工具+文本回复链路应返回 Success,实际: {:?}",
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
        // 1) 工具调用；2) 文本回复（无工具义务 → 直接完成，不再有总结请求）。
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

        let mut harness = TestHarness::new(&server, vec![tool_spec("echo")], overrides);
        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;

        assert!(
            matches!(result.outcome, TurnExecutionOutcome::Success),
            "工具+文本链路应返回 Success，实际: {:?}",
            result.outcome
        );
        // 两轮请求用量累计：(15+3) + (25+5) = 48
        assert_eq!(
            result.usage.total_tokens, 48,
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

    /// 计数 on_cancel 调用次数的插件（ALR-103 语义验证）。
    struct CancelCountingPlugin {
        cancelled: Arc<AtomicU32>,
    }

    impl ToolOverrideHandler for CancelCountingPlugin {}
    impl ToolSpecProvider for CancelCountingPlugin {}
    impl PromptSectionProvider for CancelCountingPlugin {}
    impl MentionCandidateProvider for CancelCountingPlugin {}

    impl Plugin for CancelCountingPlugin {
        fn id(&self) -> &str {
            "cancel-counter"
        }
        fn on_cancel<'a>(
            &'a self,
            _session: &mut Session,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
            let cancelled = self.cancelled.clone();
            Box::pin(async move {
                cancelled.fetch_add(1, Ordering::SeqCst);
            })
        }
    }

    /// ALR-103：普通引导消息不取消插件后台任务（on_cancel 不被调用）；显式取消
    /// 整个 turn 时 on_cancel 恰好调用一次。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn inject_does_not_cancel_plugins_but_explicit_cancel_does() {
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
        // 两轮工具调用：第一轮被注入打断（新意图再次进入工具等待），随后显式取消。
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
            vec![
                tool_call_chunk("call_2", "paused_probe", "{}"),
                usage_chunk(20, 4),
            ],
        )
        .await;

        let cancelled = Arc::new(AtomicU32::new(0));
        let plugin = Arc::new(CancelCountingPlugin {
            cancelled: cancelled.clone(),
        });
        let harness = TestHarness::new_with_plugins(
            &server,
            vec![tool_spec("paused_probe")],
            overrides,
            vec![plugin],
        );
        let TestHarness { ctx, stream_rx, .. } = harness;
        // on_cancel 由 run_turn 在终态判定后调用，须走 run_turn 层验证。
        // 顺序驱动：等工具启动 → 注入 → 等第二次工具启动 → 显式取消。
        let started_wait = started.clone();
        let cancelled_probe = cancelled.clone();
        let (tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<Command>();
        let _keep_open = tx.clone();
        let cmd_task = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(2), started_wait.notified())
                .await
                .expect("第一个工具应已启动");
            tx.send(Command::InjectUserMessage {
                message_id: "injected-cancel-probe".to_string(),
                content: vec![tiangong_types::ContentBlock::text("换个方向")],
            })
            .unwrap();
            // 注入后引导阶段不触发 on_cancel（此刻计数必须为 0）。
            tokio::time::timeout(Duration::from_secs(2), started_wait.notified())
                .await
                .expect("新意图的工具应已启动");
            assert_eq!(
                cancelled_probe.load(Ordering::SeqCst),
                0,
                "引导消息不应触发 on_cancel（ALR-103）"
            );
            tx.send(Command::Cancel).unwrap();
        });
        run_turn(ctx, cmd_rx).await;
        let _ = cmd_task.await;
        assert_eq!(
            cancelled.load(Ordering::SeqCst),
            1,
            "显式取消应恰好触发一次 on_cancel（ALR-103）"
        );
        // 取消终态 + 注入的消息已保存（引导路径生效）。
        let terminal: Vec<String> = stream_rx
            .try_iter()
            .filter_map(|e| match e {
                StreamEvent::Error { message } => Some(message),
                _ => None,
            })
            .collect();
        assert!(
            terminal.iter().any(|m| m.contains("已取消")),
            "应发布取消终态，实际: {terminal:?}"
        );
    }

    /// 并行工具批次：单个响应返回两个工具调用（不同参数，避免同批去重），
    /// 两者都执行并产出结果，协议完整后进入完成度检查（不变量 3：任务↔记录对应）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn parallel_tool_batch_executes_both_and_closes_protocol() {
        let server = MockServer::start().await;
        let invocations = Arc::new(Mutex::new(Vec::new()));
        let mut overrides: HashMap<String, Arc<dyn ToolOverrideHandler>> = HashMap::new();
        overrides.insert(
            "echo".to_string(),
            Arc::new(EchoTool {
                invocations: invocations.clone(),
            }),
        );
        // 1) 单响应两个工具调用（参数不同，避免同批去重跳过）。
        mount_sse(
            &server,
            vec![
                tool_call_chunk("call_a", "echo", r#"{"a":1}"#),
                tool_call_chunk("call_b", "echo", r#"{"b":2}"#),
                usage_chunk(15, 3),
            ],
        )
        .await;
        // 2) 工具后文本以问号结尾 → 进入 Summary；3) Summary 完成。
        mount_sse(
            &server,
            vec![text_delta_chunk("两个都完成了吗?"), usage_chunk(25, 5)],
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
            "并行批次完整链路应 Success，实际: {:?}",
            result.outcome
        );
        assert_eq!(invocations.lock().unwrap().len(), 2, "两个并行工具都应执行");
        for call_id in ["call_a", "call_b"] {
            let has_result = harness
                .ctx
                .session
                .messages
                .iter()
                .any(|m| m.tool_call_id.as_deref() == Some(call_id));
            assert!(has_result, "{call_id} 应有对应工具结果（协议闭合）");
        }
        harness.drain_stream();
    }

    /// ALR-101（压缩分支）：压缩进行中收到引导消息——取消压缩、保存新消息、
    /// 从新意图重启；被取消压缩的迟到结果不得应用（context_summary 保持为空）。
    /// 请求前压缩期间注入用户消息：压缩被中止、迟到结果不应用，新意图重启后
    /// 正常完成（ALR-102/303）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn inject_during_compression_cancels_and_restarts_without_applying_summary() {
        let server = MockServer::start().await;
        // 1) 第一段：大用量文本完成（建立压力信号）。
        mount_sse(
            &server,
            vec![text_delta_chunk("部分回答。"), usage_chunk(185_900, 5)],
        )
        .await;
        // 2) 第二段请求前压缩（响应延迟制造注入窗口）。
        mount_completion(
            &server,
            "[[CURRENT_TASK]]\n旧任务\n[[SUMMARY]]\n旧摘要（不应被应用）",
            "stop",
            100,
            20,
            Some(Duration::from_millis(400)),
        )
        .await;
        // 3) 注入重启后：再次请求前压缩（无可用压缩响应，失败不应用）。
        mount_sse(
            &server,
            vec![text_delta_chunk("按新要求完成。"), usage_chunk(20, 4)],
        )
        .await;
        // 4) 最终模型请求。
        mount_sse(
            &server,
            vec![text_delta_chunk("已完成。"), usage_chunk(25, 3)],
        )
        .await;

        let mut harness = TestHarness::new(&server, Vec::new(), HashMap::new());
        let _ = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;
        harness.drain_stream();
        harness
            .ctx
            .session
            .append_message(MessageRole::User, "继续任务");

        let TestHarness {
            mut ctx,
            stream_rx: _,
            cmd_tx,
            mut cmd_rx,
            ..
        } = harness;
        let turn = tokio::spawn(async move {
            let result = execute_turn(&mut ctx, &mut cmd_rx).await;
            (result, ctx)
        });
        // 第一段已发出 1 个请求；等待第二段的压缩请求（第 2 个）后注入。
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while server.received_requests().await.map_or(0, |r| r.len()) < 2 {
            if tokio::time::Instant::now() >= deadline {
                panic!("压缩请求未在期限内发出");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        cmd_tx
            .send(Command::InjectUserMessage {
                message_id: "injected-during-compression".to_string(),
                content: vec![tiangong_types::ContentBlock::text("换个方向，不用压缩了")],
            })
            .unwrap();

        let (result, ctx) = turn.await.expect("turn task 不应 panic");
        assert!(
            matches!(result.outcome, TurnExecutionOutcome::Success),
            "新意图执行应成功，实际: {:?}",
            result.outcome
        );
        assert!(
            ctx.session
                .messages
                .iter()
                .any(|m| m.id == "injected-during-compression" && m.role == MessageRole::User),
            "引导消息应保存进 session"
        );
        assert!(
            ctx.session.context_summary.is_none(),
            "被中断压缩的迟到结果不得应用（context_summary 应为空）"
        );
    }

    /// ALR-203 连续命令顺序：引导消息与取消接连到达时按序处理——注入先保存
    /// 消息并重启，随后的取消形成最终取消终态（不被重启意图覆盖）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn consecutive_inject_then_cancel_terminates_in_order() {
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

        let harness = TestHarness::new(&server, vec![tool_spec("paused_probe")], overrides);
        let TestHarness {
            mut ctx,
            stream_rx: _,
            cmd_tx,
            mut cmd_rx,
            ..
        } = harness;
        let tx = cmd_tx.clone();
        let started_wait = started.clone();
        let cmd_task = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(2), started_wait.notified())
                .await
                .expect("工具应已启动");
            // 两条命令背靠背投递：注入在前、取消在后。
            tx.send(Command::InjectUserMessage {
                message_id: "injected-then-cancel".to_string(),
                content: vec![tiangong_types::ContentBlock::text("先换个方向")],
            })
            .unwrap();
            tx.send(Command::Cancel).unwrap();
        });
        let result = execute_turn(&mut ctx, &mut cmd_rx).await;
        let _ = cmd_task.await;
        assert!(
            matches!(result.outcome, TurnExecutionOutcome::Cancelled),
            "连续注入后取消应形成取消终态，实际: {:?}",
            result.outcome
        );
        assert!(
            ctx.session
                .messages
                .iter()
                .any(|m| m.id == "injected-then-cancel" && m.role == MessageRole::User),
            "注入的消息应已保存（先于取消处理）"
        );
    }

    /// 压力场景（任务 09）：连续两条引导消息——都保存成功、按序重启、
    /// 最终一次成功终态；锚点为最后一条注入消息。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn consecutive_injects_are_all_saved_and_restart_in_order() {
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
        // 两次工具调用（两次打断窗口）+ 新意图文本 + Summary。
        for i in 1..=2 {
            let _ = i;
            mount_sse(
                &server,
                vec![
                    tool_call_chunk("call_x", "paused_probe", "{}"),
                    usage_chunk(15, 3),
                ],
            )
            .await;
        }
        mount_sse(
            &server,
            vec![text_delta_chunk("按最新要求完成。"), usage_chunk(20, 4)],
        )
        .await;
        mount_sse(
            &server,
            vec![text_delta_chunk("已完成。"), usage_chunk(25, 3)],
        )
        .await;

        let harness = TestHarness::new(&server, vec![tool_spec("paused_probe")], overrides);
        let TestHarness {
            mut ctx,
            stream_rx: _,
            cmd_tx,
            mut cmd_rx,
            ..
        } = harness;
        let tx = cmd_tx.clone();
        let started_wait = started.clone();
        let cmd_task = tokio::spawn(async move {
            for n in 1..=2u32 {
                tokio::time::timeout(Duration::from_secs(2), started_wait.notified())
                    .await
                    .expect("工具应已启动");
                tx.send(Command::InjectUserMessage {
                    message_id: format!("injected-chain-{n}"),
                    content: vec![tiangong_types::ContentBlock::text(format!("第 {n} 次调整"))],
                })
                .unwrap();
            }
        });
        let result = execute_turn(&mut ctx, &mut cmd_rx).await;
        let _ = cmd_task.await;
        assert!(
            matches!(result.outcome, TurnExecutionOutcome::Success),
            "连续引导后应成功完成，实际: {:?}",
            result.outcome
        );
        for n in 1..=2u32 {
            let id = format!("injected-chain-{n}");
            assert!(
                ctx.session
                    .messages
                    .iter()
                    .any(|m| m.id == id && m.role == MessageRole::User),
                "{id} 应已保存"
            );
        }
        // 锚点：最后一条用户消息是第二次注入。
        let latest = ctx
            .session
            .messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::User)
            .expect("应有用户消息");
        assert_eq!(latest.id, "injected-chain-2", "最新锚点应为最后一条注入");
    }

    /// 压力场景（任务 09）：命令风暴——工具等待中背靠背投递混合命令（标题/用量/
    /// 工具注入/思考强度/流事件/引导/取消），按序处理不 panic，取消形成终态，
    /// 副作用（标题、用量）生效。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn command_storm_is_processed_in_order_without_panicking() {
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

        let harness = TestHarness::new(&server, vec![tool_spec("paused_probe")], overrides);
        let TestHarness {
            mut ctx,
            stream_rx: _,
            cmd_tx,
            mut cmd_rx,
            ..
        } = harness;
        let tx = cmd_tx.clone();
        let started_wait = started.clone();
        let cmd_task = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(2), started_wait.notified())
                .await
                .expect("工具应已启动");
            // 命令风暴：混合非决定性命令 + 引导 + 取消（最后）。
            tx.send(Command::SetTitle {
                title: "风暴标题".to_string(),
                only_if_default: false,
            })
            .unwrap();
            tx.send(Command::ReportUsage {
                usage: TokenUsage {
                    prompt_tokens: 7,
                    completion_tokens: 3,
                    total_tokens: 10,
                    prompt_cache_hit_tokens: None,
                    prompt_cache_miss_tokens: None,
                },
                source: "storm-probe".to_string(),
                emit_event: false,
            })
            .unwrap();
            tx.send(Command::InjectTool {
                tool_name: "storm_data".to_string(),
                payload: serde_json::json!({"k": 1}),
            })
            .unwrap();
            tx.send(Command::SetReasoningEffort("high".to_string()))
                .unwrap();
            tx.send(Command::EmitStreamEvent(Box::new(
                StreamEvent::TitleChanged {
                    title: "不应直接出现的标题".to_string(),
                },
            )))
            .unwrap();
            tx.send(Command::InjectUserMessage {
                message_id: "storm-injected".to_string(),
                content: vec![tiangong_types::ContentBlock::text("风暴中的引导")],
            })
            .unwrap();
            tx.send(Command::Cancel).unwrap();
        });
        let result = execute_turn(&mut ctx, &mut cmd_rx).await;
        let _ = cmd_task.await;
        assert!(
            matches!(result.outcome, TurnExecutionOutcome::Cancelled),
            "风暴以取消收尾应形成取消终态，实际: {:?}",
            result.outcome
        );
        assert_eq!(ctx.session.title, "风暴标题", "标题命令应已生效");
        assert_eq!(
            ctx.session.reasoning_effort.as_deref(),
            Some("high"),
            "思考强度命令应已生效"
        );
        assert!(
            result.usage.total_tokens >= 10,
            "插件用量应累计进终态（ALR-111），实际: {}",
            result.usage.total_tokens
        );
        assert!(
            ctx.session
                .messages
                .iter()
                .any(|m| m.id == "storm-injected" && m.role == MessageRole::User),
            "风暴中的引导消息应已保存"
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
        // 拒绝后模型看到拒绝结果，解释结束（无义务 → 门控通过）。
        mount_sse(
            &server,
            vec![
                text_delta_chunk("已按你的要求取消执行该工具。"),
                usage_chunk(10, 3),
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
}
