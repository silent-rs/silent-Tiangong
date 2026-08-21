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
use crate::react::context::{emit_token_usage, persist_error, select_client_for_request};
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
use super::phase::{
    ActiveLlm, ExecutionBudget, ExecutionPhase, LlmPurpose, StreamTiming, ToolBatchState,
};
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
    reasoning_elapsed_ms: Option<u64>,
    text_elapsed_ms: Option<u64>,
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
        reasoning_elapsed_ms,
        text_elapsed_ms,
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

#[allow(dead_code)]
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
    append_tool_result_message_with_duration(
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
        duration_ms,
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
/// （模型请求/工具任务/压缩）都在 phase 变体内，不再有并列活动 `Option`
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

// 阶段数据类型统一定义在 super::phase。

fn build_react_request(ctx: &TurnContext) -> ModelRequest {
    ModelRequest {
        user_input: String::new(),
        context: ctx.session.context(),
        reasoning_effort: ctx.agent_config.reasoning_effort,
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
        timing: StreamTiming::default(),
    }
}

pub(super) fn persist_streamed_react_message(
    ctx: &mut TurnContext,
    pending_msg_id: &str,
    streamed_text: &str,
    streamed_reasoning: &str,
    reasoning_elapsed_ms: Option<u64>,
    text_elapsed_ms: Option<u64>,
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
        reasoning_elapsed_ms,
        text_elapsed_ms,
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
    reasoning_elapsed_ms: Option<u64>,
    text_elapsed_ms: Option<u64>,
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
        reasoning_elapsed_ms,
        text_elapsed_ms,
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

            // ── Ready：候选完成并交给统一 turn 收尾 ──
            // 阶段变体只含结果，不持有主循环活动资源。唯一 Receiver 不切换、
            // 不封口；提交窗口到达的命令保持在单通道中，由当前 turn 结束后的
            // 常驻 Driver 按 FIFO 继续处理。
            ExecutionPhase::PendingFinish(mut result) => {
                // 候选完成后不再封口或切换命令入口。唯一 Receiver 继续归 Driver/Loop
                // 所有；提交窗口到达的命令按 FIFO 留在通道，当前 turn 收尾后处理。
                if matches!(
                    result.outcome,
                    super::outcome::TurnExecutionOutcome::Success
                ) && let Some(msg_id) = state.pending_summary_msg_id.take()
                    && let Some(message) = ctx
                        .session
                        .messages
                        .iter_mut()
                        .find(|message| message.id == msg_id)
                {
                    message.phase = MessagePhase::Summary;
                    result.finalized_candidate_id = Some(msg_id);
                }
                result.usage = state.accumulated_usage.clone();
                break 'agent_loop result;
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
                            if !chunk.reasoning_content.is_empty() {
                                active.timing.reasoning.record();
                            }
                            if !chunk.content.is_empty() {
                                active.timing.text.record();
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
                            timing,
                            ..
                        } = active;
                        let reasoning_elapsed_ms = timing.reasoning.elapsed_ms();
                        let text_elapsed_ms = timing.text.elapsed_ms();
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
                            reasoning_elapsed_ms,
                            text_elapsed_ms,
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
    ctx.session.reasoning_effort = Some(ctx.agent_config.reasoning_effort);
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
    reasoning_elapsed_ms: Option<u64>,
    text_elapsed_ms: Option<u64>,
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
                reasoning_elapsed_ms,
                text_elapsed_ms,
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
            // 记录观测压力，交给下一次请求前策略统一判断（ALR-303）；
            // 同步到 session，跨 turn 的请求前策略也能读取该信号。
            let observed_tokens = observed_total_tokens(&response.usage);
            state.last_observed_tokens = observed_tokens;
            ctx.session.current_tokens = observed_tokens;

            if response.tool_calls.is_empty() && !response.invalid_tool_calls.is_empty() {
                append_invalid_tool_calls_context(ctx, &response.invalid_tool_calls);
                NextStep::Phase(ExecutionPhase::NeedModel)
            } else if response.tool_calls.is_empty() {
                let disposition = handle_react_text_response(
                    ctx,
                    &pending_msg_id,
                    &response,
                    reasoning_elapsed_ms,
                    text_elapsed_ms,
                );
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
                    reasoning_elapsed_ms,
                    text_elapsed_ms,
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
#[path = "execute_tests.rs"]
mod execute_tests;
