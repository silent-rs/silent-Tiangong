//! 单轮 Agent Loop 的执行过程。
//!
//! 本模块只负责从已构建的 TurnContext 执行模型请求、工具调用与总结阶段；
//! turn 的插件生命周期、状态提交和最终持久化由 react/turn.rs 负责。

use std::collections::{HashMap, HashSet};

use tokio::sync::mpsc as tokio_mpsc;

use crate::context::assembler::filter_background_task_tools;
use crate::core::command::{Command, PendingCommandEffect};
use crate::formatting::{format_llm_output_message, format_tool_trace_message};
use crate::model::{ModelFunctionResponse, ModelRequest, TokenUsage, ToolCall, ToolSpec};
use crate::permission::TrustMode;
use crate::react::context::{
    emit_token_usage, maybe_update_context_summary, persist_error, select_client_for_request,
};
use crate::react::message::*;
use crate::runtime::LlmOutputRecord;
use crate::session::{Message, MessageRole, Session};
use crate::stream_throttle::ThrottledStreamSink;
use crate::tool::ToolResult;
use crate::turn_context::TurnContext;
use tiangong_types::{StreamEvent, StreamToolCall};

use super::cancel::{CancelSignal, abort_and_join, emit_cancel_usage};
use super::helpers::{
    drain_pending_commands_async, looks_like_final_answer, process_buffered_commands,
};
use super::outcome::TurnExecutionResult;
use super::summary::{ForceFinalReason, ForceFinalResult, SummaryPhaseResult};
use super::tool_call::start_tool_call;

/// 单个 turn 内的执行阶段。
///
/// ReAct Loop 与总结阶段分离后的阶段状态机：
/// - `Initial`：外层循环第一次迭代，主模型决定是否需要工具。
/// - `ToolExecution`：工具执行阶段，LLM 只负责调用工具。
/// - `Summary`：总结阶段，主模型判断任务完成度并输出最终回复。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TurnPhase {
    Initial,
    ToolExecution,
    Summary,
}

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

// TurnContext 定义与基础能力方法位于 `crate::turn_context`。本文件只实现单轮
// Agent Loop 的阶段编排与具体执行步骤。

// ===== turn 执行辅助 =====

fn defer_tool_injections(
    ctx: &mut TurnContext,
    injections: impl IntoIterator<Item = (String, serde_json::Value)>,
) {
    for (tool_name, payload) in injections {
        crate::react::message::defer_tool_injection(ctx, tool_name, payload);
    }
}

fn build_thinking_config(
    ctx: &TurnContext,
) -> (
    Option<crate::model::ThinkingConfig>,
    Option<crate::model::ReasoningEffort>,
    bool,
) {
    let effort_str = ctx.agent_config.reasoning_effort.trim().to_lowercase();
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

enum ReactRequestOutcome {
    Completed {
        pending_msg_id: String,
        response_result: anyhow::Result<crate::model::ModelFunctionResponse>,
    },
    Interrupted {
        current_agent_input: String,
        usage: TokenUsage,
    },
    Cancelled {
        usage: TokenUsage,
    },
}

/// 发起一轮流式模型请求，并在等待期间处理可即时响应的运行时命令。
async fn request_react_response(
    ctx: &mut TurnContext,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
    request_tools: Vec<ToolSpec>,
) -> ReactRequestOutcome {
    let (thinking, reasoning_effort, thinking_disabled) = build_thinking_config(ctx);
    let request = ModelRequest {
        session_title: ctx.session.title.clone(),
        // 当前用户消息已经在 deliver 阶段写入 Session，多轮请求不得重复追加。
        user_input: String::new(),
        context: ctx.session.context(),
        thinking,
        reasoning_effort,
        thinking_disabled,
    };
    let pending_msg_id = scru128::new().to_string();
    let sink = ThrottledStreamSink::with_text_kind(
        pending_msg_id.clone(),
        ctx.stream_tx.clone(),
        crate::stream_throttle::StreamTextKind::React,
    );

    let (chunk_tx, mut chunk_rx) =
        tokio_mpsc::unbounded_channel::<crate::model::ModelStreamChunk>();
    let client = select_client_for_request(ctx, &request).clone();
    let mut llm_task = Some(tokio::task::spawn(async move {
        client
            .stream_function_calls_with_tool_choice(request, request_tools, None, chunk_tx)
            .await
    }));
    let mut interrupted_input = None;
    let mut streamed_text = String::new();
    let mut streamed_reasoning = String::new();
    let mut streaming_usage = TokenUsage::default();

    let response_result = loop {
        tokio::select! {
            biased;
            command = cmd_rx.recv() => {
                match command {
                    Some(Command::Shutdown) | Some(Command::Cancel) | None => {
                        break Err(anyhow::Error::new(CancelSignal::Abort));
                    }
                    Some(Command::Message {
                        prepared,
                        message_id,
                    }) => {
                        sink.flush();
                        let message_count_before_interruption = ctx.session.messages.len();
                        if !streamed_text.trim().is_empty()
                            || !streamed_reasoning.trim().is_empty()
                        {
                            upsert_assistant_text_message(
                                &mut ctx.session,
                                &pending_msg_id,
                                &streamed_text,
                                &streamed_reasoning,
                                crate::session::MessagePhase::React,
                            );
                        }
                        match accept_runtime_user_message(ctx, message_id, prepared) {
                            Ok(input) => {
                                interrupted_input = Some(input);
                                if let Some(handle) = llm_task.take() {
                                    abort_and_join(handle).await;
                                }
                                break Err(anyhow::anyhow!(
                                    "模型响应已被新的用户消息中断"
                                ));
                            }
                            Err(error) => {
                                ctx.session
                                    .messages
                                    .truncate(message_count_before_interruption);
                                tracing::warn!(
                                    %error,
                                    "流式阶段追加用户消息持久化失败"
                                );
                            }
                        }
                    }
                    Some(Command::Approval { .. }) => {}
                    Some(Command::InjectTool { tool_name, payload }) => {
                        inject_tool_to_session(ctx, &tool_name, &payload);
                    }
                    Some(Command::CompressContext) => {
                        let _ = ctx.stream_tx.send(StreamEvent::AgentNotification {
                            agent_id: "system".to_string(),
                            agent_label: "系统".to_string(),
                            content: "当前轮次执行中，已跳过手动压缩，请在轮次结束后重试"
                                .to_string(),
                            level: "warning".to_string(),
                        });
                    }
                    Some(Command::ResetContext) => {
                        crate::core::reset_context(ctx);
                    }
                    Some(Command::EmitStreamEvent(event)) => {
                        let _ = ctx.stream_tx.send(*event);
                    }
                    Some(Command::SetTrustMode(mode)) => {
                        ctx.trust_mode = mode;
                    }
                }
            }
            chunk = chunk_rx.recv() => {
                match chunk {
                    Some(chunk) => {
                        if let Some(chunk_usage) = &chunk.usage {
                            let usage: TokenUsage = chunk_usage.clone().into();
                            streaming_usage.accumulate(&usage);
                        }
                        streamed_text.push_str(&chunk.content);
                        streamed_reasoning.push_str(&chunk.reasoning_content);
                        sink.push_chunk(&chunk);
                    }
                    None => {
                        let result = match llm_task.take().expect("模型任务必须存在").await {
                            Ok(result) => result,
                            Err(error) if error.is_cancelled() => {
                                sink.finish();
                                emit_cancel_usage(
                                    &ctx.stream_tx,
                                    &streaming_usage,
                                    ctx.context_limit,
                                );
                                return ReactRequestOutcome::Cancelled {
                                    usage: streaming_usage,
                                };
                            }
                            Err(error) => Err(anyhow::anyhow!(error.to_string())),
                        };
                        break result;
                    }
                }
            }
        }
    };
    sink.finish();

    if !streamed_text.trim().is_empty() || !streamed_reasoning.trim().is_empty() {
        upsert_assistant_text_message(
            &mut ctx.session,
            &pending_msg_id,
            &streamed_text,
            &streamed_reasoning,
            crate::session::MessagePhase::React,
        );
        emit_session_message_upsert(ctx, &pending_msg_id);
    }

    if let Some(current_agent_input) = interrupted_input {
        if streaming_usage.total_tokens > 0 {
            emit_token_usage(
                &ctx.stream_tx,
                &streaming_usage,
                None,
                ctx.context_limit,
                "react-interrupted",
                None,
            );
        }
        return ReactRequestOutcome::Interrupted {
            current_agent_input,
            usage: streaming_usage,
        };
    }

    if response_result
        .as_ref()
        .err()
        .and_then(CancelSignal::from_error)
        .is_some()
    {
        if let Some(handle) = llm_task.take() {
            abort_and_join(handle).await;
        }
        emit_cancel_usage(&ctx.stream_tx, &streaming_usage, ctx.context_limit);
        return ReactRequestOutcome::Cancelled {
            usage: streaming_usage,
        };
    }

    ReactRequestOutcome::Completed {
        pending_msg_id,
        response_result,
    }
}

enum TextResponseOutcome {
    Completed,
    EnterSummary,
    Cancelled,
}

struct TextResponseState {
    round: usize,
    outer_iteration: u32,
    executed_tool: bool,
}

/// 保存无工具调用的模型响应，并判断是否可直接作为本轮最终回复。
async fn handle_text_response(
    ctx: &mut TurnContext,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
    pending_msg_id: &str,
    response: &crate::model::ModelFunctionResponse,
    state: TextResponseState,
) -> TextResponseOutcome {
    if is_synthetic_tool_call_placeholder(&response.text) {
        return TextResponseOutcome::EnterSummary;
    }

    upsert_assistant_text_message(
        &mut ctx.session,
        pending_msg_id,
        &response.text,
        &response.reasoning_content,
        crate::session::MessagePhase::React,
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
    let output = LlmOutputRecord {
        stage: format!("react-round-{}", state.round),
        content: response.text.clone(),
        reasoning_content: response.reasoning_content.clone(),
        tool_calls: Vec::new(),
        usage: response.usage.clone(),
    };
    append_runtime_tool_message_with_reasoning(
        &mut ctx.session,
        "llm_output",
        format_llm_output_message(&output),
        response.reasoning_content.clone(),
    );
    ctx.session.persist_to_disk();

    if maybe_update_context_summary(ctx, &response.usage, cmd_rx).await {
        return TextResponseOutcome::Cancelled;
    }

    let can_promote_direct_answer =
        state.outer_iteration == 0 && !state.executed_tool && !response.text.trim().is_empty();
    let can_promote_tool_answer = state.executed_tool && looks_like_final_answer(&response.text);
    if !can_promote_direct_answer && !can_promote_tool_answer {
        return TextResponseOutcome::EnterSummary;
    }

    if let Some(message) = ctx
        .session
        .messages
        .iter_mut()
        .find(|message| message.id == pending_msg_id)
    {
        message.phase = crate::session::MessagePhase::Summary;
    }
    emit_session_message_upsert(ctx, pending_msg_id);
    ctx.session.persist_to_disk();
    TextResponseOutcome::Completed
}

enum ToolBatchOutcome {
    Continue,
    Restart(String),
    Completed,
    Cancelled,
}

fn record_tool_calls<'a>(
    ctx: &mut TurnContext,
    pending_msg_id: &str,
    response: &'a ModelFunctionResponse,
    round: usize,
) -> Vec<&'a ToolCall> {
    let calls = response.tool_calls.iter().collect::<Vec<_>>();
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
        &calls,
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

enum ToolApprovalOutcome {
    Approved,
    Rejected,
    Restart(String),
    Cancelled,
}

async fn request_tool_approval(
    ctx: &mut TurnContext,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
    call: &ToolCall,
    args_summary: &str,
) -> ToolApprovalOutcome {
    let trust_mode = format!("{:?}", ctx.trust_mode);
    if ctx.trust_mode == TrustMode::FullTrust {
        ctx.observer.audit_permission(
            &ctx.session.id,
            &call.name,
            "approved",
            &trust_mode,
            (!args_summary.is_empty()).then_some(args_summary),
        );
        return ToolApprovalOutcome::Approved;
    }

    let request_id = scru128::new().to_string();
    ctx.observer.audit_permission(
        &ctx.session.id,
        &call.name,
        "needs_approval",
        &trust_mode,
        (!args_summary.is_empty()).then_some(args_summary),
    );
    let _ = ctx.stream_tx.send(StreamEvent::ApprovalNeeded {
        request_id: request_id.clone(),
        tool_name: call.name.clone(),
        args_summary: args_summary.to_string(),
    });

    loop {
        match cmd_rx.recv().await {
            Some(Command::Approval {
                request_id: response_id,
                approved,
            }) if response_id == request_id => {
                return if approved {
                    ToolApprovalOutcome::Approved
                } else {
                    ToolApprovalOutcome::Rejected
                };
            }
            Some(Command::Shutdown) | Some(Command::Cancel) | None => {
                return ToolApprovalOutcome::Cancelled;
            }
            Some(Command::Message {
                prepared,
                message_id,
            }) => match accept_runtime_user_message(ctx, message_id, prepared) {
                Ok(input) => return ToolApprovalOutcome::Restart(input),
                Err(error) => tracing::warn!(
                    %error,
                    "审批等待阶段追加用户消息持久化失败"
                ),
            },
            Some(Command::Approval { .. }) => {}
            Some(Command::SetTrustMode(mode)) => ctx.trust_mode = mode,
            Some(Command::InjectTool { tool_name, payload }) => {
                defer_tool_injections(ctx, std::iter::once((tool_name, payload)));
            }
            Some(Command::CompressContext) => {
                let _ = ctx.stream_tx.send(StreamEvent::AgentNotification {
                    agent_id: "system".to_string(),
                    agent_label: "系统".to_string(),
                    content: "当前轮次执行中，已跳过手动压缩，请在轮次结束后重试".to_string(),
                    level: "warning".to_string(),
                });
            }
            Some(Command::ResetContext) => {
                crate::core::reset_context(ctx);
            }
            Some(Command::EmitStreamEvent(event)) => {
                let _ = ctx.stream_tx.send(*event);
            }
        }
    }
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
    flush_deferred_tool_injections(ctx);
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

enum RunningToolOutcome {
    Completed {
        result: ToolResult,
        buffered_commands: Vec<Command>,
        duration_ms: u64,
    },
    Interrupted {
        buffered_commands: Vec<Command>,
        duration_ms: u64,
    },
}

async fn run_tool_call(
    ctx: &mut TurnContext,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
    call: &ToolCall,
    args_summary: &str,
) -> RunningToolOutcome {
    let _ = ctx.stream_tx.send(StreamEvent::ToolStart {
        name: call.name.clone(),
        args_summary: args_summary.to_string(),
    });
    let started_at = std::time::Instant::now();
    let mut buffered_commands = Vec::new();
    let actor_id = ctx.session.id.clone();
    let mut tool_future = start_tool_call(ctx, call, &actor_id);
    let result = loop {
        tokio::select! {
            biased;
            result = &mut tool_future => break Some(result),
            command = cmd_rx.recv() => {
                match command {
                    Some(Command::Approval { .. }) => {}
                    Some(Command::EmitStreamEvent(event)) => {
                        let _ = ctx.stream_tx.send(*event);
                    }
                    Some(Command::SetTrustMode(mode)) => ctx.trust_mode = mode,
                    Some(Command::Cancel) | Some(Command::Shutdown) | None => break None,
                    Some(command) => buffered_commands.push(command),
                }
            }
        }
    };
    drop(tool_future);
    let duration_ms = started_at.elapsed().as_millis() as u64;
    match result {
        Some(result) => RunningToolOutcome::Completed {
            result,
            buffered_commands,
            duration_ms,
        },
        None => RunningToolOutcome::Interrupted {
            buffered_commands,
            duration_ms,
        },
    }
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
    let _ = ctx.stream_tx.send(StreamEvent::ToolResult {
        name: call.name.clone(),
        tool_call_id: Some(call.id.clone()),
        ok: result.ok,
        output: tool_result_stream_output(result),
        full_output: Some(tool_result_full_output(result)),
        duration_ms: Some(duration_ms),
    });
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

    if result.ok {
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
    }
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

/// 记录并执行一批工具调用，所有跨循环转向都通过明确结果返回。
#[allow(clippy::too_many_arguments)]
async fn execute_tool_batch(
    ctx: &mut TurnContext,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
    pending_msg_id: &str,
    response: &ModelFunctionResponse,
    round: usize,
    request_tools: &[ToolSpec],
    accumulated_usage: &mut TokenUsage,
    tool_history: &mut ToolCallHistory,
) -> ToolBatchOutcome {
    let calls = record_tool_calls(ctx, pending_msg_id, response, round);
    let mut needs_failure_recovery = false;

    for call in calls {
        match drain_pending_commands_async(ctx, cmd_rx) {
            PendingCommandEffect::Terminate | PendingCommandEffect::Shutdown => {
                return ToolBatchOutcome::Cancelled;
            }
            PendingCommandEffect::MessagesInjected {
                current_agent_input,
                ..
            } => {
                tool_history.clear();
                if let Some(input) = current_agent_input {
                    ctx.session.persist_to_disk();
                    return ToolBatchOutcome::Restart(input);
                }
                ctx.session.persist_to_disk();
            }
            PendingCommandEffect::None => {}
        }

        let (args_summary, dedupe_key) = match prepare_tool_call(ctx, call, tool_history) {
            ToolPreflightOutcome::Execute {
                args_summary,
                dedupe_key,
            } => (args_summary, dedupe_key),
            ToolPreflightOutcome::Skip { needs_recovery } => {
                needs_failure_recovery |= needs_recovery;
                continue;
            }
        };

        match request_tool_approval(ctx, cmd_rx, call, &args_summary).await {
            ToolApprovalOutcome::Approved => {}
            ToolApprovalOutcome::Rejected => {
                record_rejected_tool_call(ctx, call, &args_summary);
                return ToolBatchOutcome::Completed;
            }
            ToolApprovalOutcome::Restart(input) => {
                tool_history.clear();
                ctx.session.persist_to_disk();
                return ToolBatchOutcome::Restart(input);
            }
            ToolApprovalOutcome::Cancelled => return ToolBatchOutcome::Cancelled,
        }

        let (result, buffered_commands, duration_ms) =
            match run_tool_call(ctx, cmd_rx, call, &args_summary).await {
                RunningToolOutcome::Completed {
                    result,
                    buffered_commands,
                    duration_ms,
                } => (result, buffered_commands, duration_ms),
                RunningToolOutcome::Interrupted {
                    buffered_commands,
                    duration_ms,
                } => {
                    let output = "工具调用因执行取消或会话关闭而中断。".to_string();
                    let _ = ctx.stream_tx.send(StreamEvent::ToolResult {
                        name: call.name.clone(),
                        tool_call_id: Some(call.id.clone()),
                        ok: false,
                        output: output.clone(),
                        full_output: None,
                        duration_ms: Some(duration_ms),
                    });
                    append_tool_result_message(
                        &mut ctx.session,
                        &call.id,
                        &call.name,
                        output,
                        true,
                    );
                    if let PendingCommandEffect::MessagesInjected {
                        current_agent_input: Some(input),
                        ..
                    } = process_buffered_commands(ctx, buffered_commands)
                    {
                        tool_history.clear();
                        ctx.session.persist_to_disk();
                        return ToolBatchOutcome::Restart(input);
                    }
                    return ToolBatchOutcome::Cancelled;
                }
            };

        // 工具自身不产生模型用量；保留统一的用量事件入口。
        let tool_usage = TokenUsage::default();
        accumulated_usage.accumulate(&tool_usage);
        emit_token_usage(
            &ctx.stream_tx,
            &tool_usage,
            None,
            ctx.context_limit,
            "",
            None,
        );

        needs_failure_recovery |= record_completed_tool_call(
            ctx,
            CompletedToolCall {
                call,
                args_summary: &args_summary,
                dedupe_key,
                result: &result,
                duration_ms,
            },
            tool_history,
        );

        match process_buffered_commands(ctx, buffered_commands) {
            PendingCommandEffect::Terminate | PendingCommandEffect::Shutdown => {
                return ToolBatchOutcome::Cancelled;
            }
            PendingCommandEffect::MessagesInjected {
                current_agent_input: Some(input),
                ..
            } => {
                tool_history.clear();
                ctx.session.persist_to_disk();
                return ToolBatchOutcome::Restart(input);
            }
            PendingCommandEffect::MessagesInjected { .. } | PendingCommandEffect::None => {}
        }

        if maybe_update_context_summary(ctx, &response.usage, cmd_rx).await {
            return ToolBatchOutcome::Cancelled;
        }

        match drain_pending_commands_async(ctx, cmd_rx) {
            PendingCommandEffect::Terminate | PendingCommandEffect::Shutdown => {
                return ToolBatchOutcome::Cancelled;
            }
            PendingCommandEffect::MessagesInjected {
                current_agent_input,
                ..
            } => {
                tool_history.clear();
                if let Some(input) = current_agent_input {
                    ctx.session.persist_to_disk();
                    return ToolBatchOutcome::Restart(input);
                }
                ctx.session.persist_to_disk();
            }
            PendingCommandEffect::None => {}
        }
        flush_deferred_tool_injections(ctx);
    }

    if needs_failure_recovery {
        append_failure_recovery_prompt(ctx, tool_history, request_tools);
    } else {
        ctx.session.persist_to_disk();
    }
    ToolBatchOutcome::Continue
}

enum ReactPhaseOutcome {
    EnterSummary { round: usize },
    Restart(String),
    Completed,
    Cancelled,
    Failed(String),
}

/// 执行一次完整的 ReAct 工具阶段，直到需要进入总结或本轮状态发生转向。
#[allow(clippy::too_many_arguments)]
async fn execute_react_phase(
    ctx: &mut TurnContext,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
    start_round: usize,
    outer_iteration: u32,
    user_input: &str,
    accumulated_usage: &mut TokenUsage,
    tool_history: &mut ToolCallHistory,
) -> ReactPhaseOutcome {
    let mut round = start_round;
    let _phase = if outer_iteration == 0 {
        TurnPhase::Initial
    } else {
        TurnPhase::ToolExecution
    };
    let iteration_start_round = round;
    let mut executed_tool_in_iteration = false;

    'react_loop: loop {
        if round == 0 {
            debug_assert!(
                ctx.session.system_prompt_message.is_some(),
                "TurnContext 构建前应已注入 system prompt"
            );
        }
        match drain_pending_commands_async(ctx, cmd_rx) {
            PendingCommandEffect::Terminate => {
                return ReactPhaseOutcome::Cancelled;
            }
            PendingCommandEffect::Shutdown => {
                return ReactPhaseOutcome::Cancelled;
            }
            PendingCommandEffect::MessagesInjected {
                current_agent_input,
                ..
            } => {
                tool_history.clear();
                if let Some(input) = current_agent_input {
                    ctx.session.persist_to_disk();
                    return ReactPhaseOutcome::Restart(input);
                }
            }
            PendingCommandEffect::None => {}
        }
        crate::react::message::flush_deferred_tool_injections(ctx);

        // 工具执行完进入下一轮模型请求前，通知前端"正在分析工具结果"，
        // 避免前端把模型等待时间算到最后一个工具上。
        if round > 0 {
            let _ = ctx.stream_tx.send(StreamEvent::PhaseChanged {
                phase: "analyzing".to_string(),
                iteration: (round + 1) as u32,
            });
        }

        // 内层工具执行阶段轮次上限：达到即结束工具阶段，进入总结。
        // 以本次外层迭代的起始轮次为基准计算，避免重入 Loop 时累计。
        if round.saturating_sub(iteration_start_round) >= ctx.max_tool_rounds {
            return ReactPhaseOutcome::EnterSummary { round };
        }

        let request_tools = tools_for_current_turn(ctx, user_input);

        let (pending_msg_id, response) =
            match request_react_response(ctx, cmd_rx, request_tools.clone()).await {
                ReactRequestOutcome::Completed {
                    pending_msg_id: response_message_id,
                    response_result,
                } => {
                    let response = match response_result {
                        Ok(response) => response,
                        Err(error) => {
                            let error_message = error.to_string();
                            // 上下文超限或空响应时强制压缩后重试。
                            if error_message.contains("context_window_exceeded")
                                || error_message.contains("context_length_exceeded")
                                || (error_message.contains("content_blocks=0")
                                    && error_message.contains("stop_reason=end_turn"))
                            {
                                tracing::warn!("检测到上下文超限，尝试强制压缩");
                                let before_summary_up_to = ctx.session.summary_up_to;
                                let compression_cancelled = maybe_update_context_summary(
                                    ctx,
                                    &TokenUsage {
                                        prompt_tokens: ctx.context_limit,
                                        completion_tokens: 0,
                                        total_tokens: ctx.context_limit,
                                        prompt_cache_hit_tokens: None,
                                        prompt_cache_miss_tokens: None,
                                    },
                                    cmd_rx,
                                )
                                .await;
                                if compression_cancelled {
                                    return ReactPhaseOutcome::Cancelled;
                                }
                                if ctx.session.summary_up_to > before_summary_up_to {
                                    continue 'react_loop;
                                }
                            }
                            persist_error(ctx, format!("ReAct 循环请求失败：{error_message}"));
                            return ReactPhaseOutcome::Failed(error_message);
                        }
                    };
                    (response_message_id, response)
                }
                ReactRequestOutcome::Interrupted {
                    current_agent_input,
                    usage,
                } => {
                    accumulated_usage.accumulate(&usage);
                    tool_history.clear();
                    ctx.session.persist_to_disk();
                    return ReactPhaseOutcome::Restart(current_agent_input);
                }
                ReactRequestOutcome::Cancelled { usage } => {
                    accumulated_usage.accumulate(&usage);
                    return ReactPhaseOutcome::Cancelled;
                }
            };

        accumulated_usage.accumulate(&response.usage);
        emit_token_usage(
            &ctx.stream_tx,
            &response.usage,
            Some(response.usage.prompt_tokens.max(ctx.session.current_tokens)),
            ctx.context_limit,
            format!("react-round-{round}", round = round + 1),
            None,
        );

        round += 1;

        if response.tool_calls.is_empty() {
            match handle_text_response(
                ctx,
                cmd_rx,
                &pending_msg_id,
                &response,
                TextResponseState {
                    round,
                    outer_iteration,
                    executed_tool: executed_tool_in_iteration,
                },
            )
            .await
            {
                TextResponseOutcome::Completed => {
                    return ReactPhaseOutcome::Completed;
                }
                TextResponseOutcome::EnterSummary => {
                    return ReactPhaseOutcome::EnterSummary { round };
                }
                TextResponseOutcome::Cancelled => {
                    return ReactPhaseOutcome::Cancelled;
                }
            }
        }

        executed_tool_in_iteration = true;
        match execute_tool_batch(
            ctx,
            cmd_rx,
            &pending_msg_id,
            &response,
            round,
            &request_tools,
            accumulated_usage,
            tool_history,
        )
        .await
        {
            ToolBatchOutcome::Continue => continue 'react_loop,
            ToolBatchOutcome::Restart(input) => {
                return ReactPhaseOutcome::Restart(input);
            }
            ToolBatchOutcome::Completed => {
                return ReactPhaseOutcome::Completed;
            }
            ToolBatchOutcome::Cancelled => {
                return ReactPhaseOutcome::Cancelled;
            }
        }
    }
}

enum SummaryStepOutcome {
    Completed,
    Continue,
    Interrupted(String),
    Cancelled,
    Failed(String),
}

/// 执行一次总结判断，并把继续执行或结束本轮的决策返回给外层编排。
async fn execute_summary_step(
    ctx: &mut TurnContext,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
    outer_iteration: &mut u32,
    accumulated_usage: &mut TokenUsage,
) -> SummaryStepOutcome {
    match ctx.run_summary_phase(cmd_rx, *outer_iteration + 1).await {
        SummaryPhaseResult::Completed(usage) => {
            accumulated_usage.accumulate(&usage);
            SummaryStepOutcome::Completed
        }
        SummaryPhaseResult::NeedMoreWork { reason, usage } => {
            accumulated_usage.accumulate(&usage);
            *outer_iteration += 1;
            if *outer_iteration >= ctx.max_outer_iterations {
                return match ctx
                    .force_final_response(cmd_rx, ForceFinalReason::OuterLimit)
                    .await
                {
                    ForceFinalResult::Completed(usage) => {
                        accumulated_usage.accumulate(&usage);
                        SummaryStepOutcome::Completed
                    }
                    ForceFinalResult::Cancelled => SummaryStepOutcome::Cancelled,
                    ForceFinalResult::Failed(message) => SummaryStepOutcome::Failed(message),
                };
            }

            // 使用合法工具调用配对注入未完成原因，保证后续 Provider 历史协议完整。
            inject_tool_to_messages(
                &mut ctx.session,
                "summary_need_more_work",
                &serde_json::json!({
                    "reason": reason.trim(),
                    "instruction": "上轮总结判定任务未完成，请根据原因继续执行剩余工作。",
                }),
            );
            ctx.session.persist_to_disk();
            SummaryStepOutcome::Continue
        }
        SummaryPhaseResult::Cancelled(usage) => {
            accumulated_usage.accumulate(&usage);
            ctx.session.persist_to_disk();
            SummaryStepOutcome::Cancelled
        }
        SummaryPhaseResult::Failed { message, usage } => {
            accumulated_usage.accumulate(&usage);
            persist_error(ctx, format!("总结阶段失败：{message}"));
            match ctx
                .force_final_response(cmd_rx, ForceFinalReason::SummaryError)
                .await
            {
                ForceFinalResult::Completed(usage) => {
                    accumulated_usage.accumulate(&usage);
                    SummaryStepOutcome::Completed
                }
                ForceFinalResult::Cancelled => SummaryStepOutcome::Cancelled,
                ForceFinalResult::Failed(force_message) => SummaryStepOutcome::Failed(format!(
                    "总结阶段失败：{message}；强制最终回复失败：{force_message}"
                )),
            }
        }
        SummaryPhaseResult::Interrupted {
            current_agent_input,
            usage,
        } => {
            accumulated_usage.accumulate(&usage);
            ctx.session.persist_to_disk();
            SummaryStepOutcome::Interrupted(current_agent_input)
        }
    }
}

/// 执行一个完整的对话轮次（可能多轮工具调用）。
///
/// Session 已在 deliver 阶段完整构建；本函数只消费 TurnContext 并执行本轮。
pub(super) async fn execute_turn(
    ctx: &mut TurnContext,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
) -> TurnExecutionResult {
    let usage_sink = ctx.turn_usage_sink().clone();
    let _usage_guard = usage_sink.bind(ctx.stream_tx.clone(), ctx.context_limit);
    let merge_plugin_usage = |acc: &mut TokenUsage| {
        acc.accumulate(&usage_sink.take_usage());
    };
    let mut round = 0usize;
    let mut outer_iteration = 0u32;
    let mut accumulated_usage = TokenUsage::default();
    // 插件用量在本轮内即时累加；guard 离开 execute_turn 时自动解绑。
    let mut tool_history = ToolCallHistory::default();
    let mut user_input = ctx
        .session
        .messages
        .iter()
        .rev()
        .find(|message| message.role == MessageRole::User)
        .map(|message| message.text_content())
        .unwrap_or_default();

    // 外层只编排 ReAct 与总结两个独立阶段；具体过程由各阶段函数负责。
    'outer: loop {
        match execute_react_phase(
            ctx,
            cmd_rx,
            round,
            outer_iteration,
            &user_input,
            &mut accumulated_usage,
            &mut tool_history,
        )
        .await
        {
            ReactPhaseOutcome::EnterSummary { round: next_round } => {
                round = next_round;
            }
            ReactPhaseOutcome::Restart(input) => {
                user_input = input;
                round = 0;
                outer_iteration = 0;
                continue 'outer;
            }
            ReactPhaseOutcome::Completed => {
                merge_plugin_usage(&mut accumulated_usage);
                break 'outer TurnExecutionResult::success(accumulated_usage);
            }
            ReactPhaseOutcome::Cancelled => {
                merge_plugin_usage(&mut accumulated_usage);
                break 'outer TurnExecutionResult::cancelled(accumulated_usage);
            }
            ReactPhaseOutcome::Failed(message) => {
                merge_plugin_usage(&mut accumulated_usage);
                break 'outer TurnExecutionResult::failed(accumulated_usage, message);
            }
        }

        match execute_summary_step(ctx, cmd_rx, &mut outer_iteration, &mut accumulated_usage).await
        {
            SummaryStepOutcome::Completed => {
                merge_plugin_usage(&mut accumulated_usage);
                break 'outer TurnExecutionResult::success(accumulated_usage);
            }
            SummaryStepOutcome::Continue => {
                tool_history.clear();
                continue 'outer;
            }
            SummaryStepOutcome::Interrupted(input) => {
                tool_history.clear();
                user_input = input;
                round = 0;
                outer_iteration = 0;
                continue 'outer;
            }
            SummaryStepOutcome::Cancelled => {
                merge_plugin_usage(&mut accumulated_usage);
                break 'outer TurnExecutionResult::cancelled(accumulated_usage);
            }
            SummaryStepOutcome::Failed(message) => {
                merge_plugin_usage(&mut accumulated_usage);
                break 'outer TurnExecutionResult::failed(accumulated_usage, message);
            }
        }
    }
}

pub(super) fn tools_for_current_turn(ctx: &TurnContext, user_input: &str) -> Vec<ToolSpec> {
    filter_tools_for_current_turn(&ctx.tools, &ctx.session, user_input)
}

fn filter_tools_for_current_turn(
    tools: &[ToolSpec],
    session: &Session,
    user_input: &str,
) -> Vec<ToolSpec> {
    let intent_text = if user_input.trim().is_empty() {
        session
            .messages
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::User && !message.model_excluded)
            .map(|message| message.text_content())
            .unwrap_or_default()
    } else {
        user_input.to_string()
    };
    filter_background_task_tools(tools.to_vec(), &intent_text)
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
    fn current_turn_hides_background_task_tools_for_normal_commands() {
        let session = Session::new("test");
        let tools = vec![tool("run_shell"), tool("spawn_task"), tool("wait_tasks")];

        let names = tool_names(filter_tools_for_current_turn(
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

        let names = tool_names(filter_tools_for_current_turn(&tools, &session, ""));

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

        let names = tool_names(filter_tools_for_current_turn(&tools, &session, ""));

        assert_eq!(names, vec!["run_shell", "spawn_task", "wait_tasks"]);
    }
}
