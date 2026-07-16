//! 单轮 Agent Loop 的执行过程。
//!
//! 本模块只负责从已构建的 TurnContext 执行模型请求、工具调用与总结阶段；
//! turn 的插件生命周期、状态提交和最终持久化由 react/turn.rs 负责。

use std::collections::{HashMap, HashSet};

use tokio::sync::{mpsc as tokio_mpsc, oneshot, watch};

use crate::core::command::Command;
use crate::formatting::{format_llm_output_message, format_tool_trace_message};
use crate::model::{ModelFunctionResponse, ModelRequest, TokenUsage, ToolCall, ToolSpec};
use crate::permission::TrustMode;
use crate::react::context::{
    emit_token_usage, maybe_update_context_summary, persist_error, select_client_for_request,
};
use crate::react::message::*;
use crate::runtime::LlmOutputRecord;
use crate::session::{Message, MessageRole};
use crate::stream_throttle::ThrottledStreamSink;
use crate::tool::ToolResult;
use crate::turn_context::TurnContext;
use tiangong_types::{StreamEvent, StreamToolCall};

use super::cancel::{CancelSignal, abort_and_join, emit_cancel_usage, run_cancelable_child};
use super::helpers::{looks_like_final_answer, record_plugin_usage};
use super::outcome::TurnExecutionResult;
use super::summary::{ForceFinalReason, ForceFinalResult, SummaryPhaseResult};
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

struct ApprovalResponse {
    request_id: String,
    approved: bool,
}

enum DeferredTurnCommand {
    CompressContext,
    ResetContext,
    InjectTool {
        tool_name: String,
        payload: serde_json::Value,
    },
}

/// 在没有活跃子循环时应用本阶段暂存的 Session 命令。
///
/// 子循环（模型请求 / 工具执行等）持有 `&mut ctx`，期间收到的需要修改 Session
/// 的命令（`ResetContext` / `InjectTool`）不能立即执行，故转发到 `deferred_tx`
/// 暂存；待子循环结束、ctx 借用归还后，在循环顶部统一 apply。`CompressContext`
/// 在回合执行中一律跳过（仅提示），因为压缩需在稳定的 Session 状态上进行。
fn apply_deferred_turn_commands(
    ctx: &mut TurnContext,
    deferred_rx: &mut tokio_mpsc::UnboundedReceiver<DeferredTurnCommand>,
) {
    while let Ok(command) = deferred_rx.try_recv() {
        match command {
            DeferredTurnCommand::CompressContext => {
                let _ = ctx.stream_tx.send(StreamEvent::AgentNotification {
                    agent_id: "system".to_string(),
                    agent_label: "系统".to_string(),
                    content: "当前轮次执行中，已跳过手动压缩，请在轮次结束后重试".to_string(),
                    level: "warning".to_string(),
                });
            }
            DeferredTurnCommand::ResetContext => crate::core::reset_context(ctx),
            DeferredTurnCommand::InjectTool { tool_name, payload } => {
                crate::react::message::defer_tool_injection(ctx, tool_name, payload);
            }
        }
    }
    crate::react::message::flush_deferred_tool_injections(ctx);
}

// TurnContext 定义与基础能力方法位于 `crate::turn_context`。本文件只实现单轮
// Agent Loop 的阶段编排与具体执行步骤。

// ===== turn 执行辅助 =====

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
    Cancelled {
        usage: TokenUsage,
    },
}

/// 发起一轮流式模型请求；取消由父循环通过独立 oneshot 传入。
async fn request_react_response(
    ctx: &mut TurnContext,
    mut cancel_rx: oneshot::Receiver<()>,
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
    let mut streamed_text = String::new();
    let mut streamed_reasoning = String::new();
    let mut streaming_usage = TokenUsage::default();

    let response_result = loop {
        tokio::select! {
            biased;
            _ = &mut cancel_rx => break Err(anyhow::Error::new(CancelSignal::Abort)),
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
    mut cancel_rx: oneshot::Receiver<()>,
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

    if run_cancelable_child(&mut cancel_rx, |child_cancel| {
        maybe_update_context_summary(ctx, &response.usage, child_cancel)
    })
    .await
    {
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
    Cancelled,
}

async fn request_tool_approval(
    ctx: &mut TurnContext,
    mut cancel_rx: oneshot::Receiver<()>,
    approval_rx: &mut tokio_mpsc::UnboundedReceiver<ApprovalResponse>,
    trust_rx: &mut watch::Receiver<TrustMode>,
    call: &ToolCall,
    args_summary: &str,
) -> ToolApprovalOutcome {
    ctx.trust_mode = *trust_rx.borrow_and_update();
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
        tokio::select! {
            biased;
            _ = &mut cancel_rx => return ToolApprovalOutcome::Cancelled,
            changed = trust_rx.changed() => {
                if changed.is_ok() {
                    ctx.trust_mode = *trust_rx.borrow_and_update();
                }
            }
            response = approval_rx.recv() => match response {
                Some(ApprovalResponse {
                    request_id: response_id,
                    approved,
                }) if response_id == request_id => {
                    return if approved {
                        ToolApprovalOutcome::Approved
                    } else {
                        ToolApprovalOutcome::Rejected
                    };
                }
                Some(_) => {}
                None => return ToolApprovalOutcome::Cancelled,
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
        duration_ms: u64,
    },
    Cancelled {
        duration_ms: u64,
    },
}

async fn run_tool_call(
    ctx: &mut TurnContext,
    mut cancel_rx: oneshot::Receiver<()>,
    call: &ToolCall,
    args_summary: &str,
) -> RunningToolOutcome {
    let _ = ctx.stream_tx.send(StreamEvent::ToolStart {
        name: call.name.clone(),
        args_summary: args_summary.to_string(),
    });
    let started_at = std::time::Instant::now();
    let actor_id = ctx.session.id.clone();
    let mut tool_future = start_tool_call(ctx, call, &actor_id);
    let result = tokio::select! {
        biased;
        _ = &mut cancel_rx => None,
        result = &mut tool_future => Some(result),
    };
    drop(tool_future);
    let duration_ms = started_at.elapsed().as_millis() as u64;
    match result {
        Some(result) => RunningToolOutcome::Completed {
            result,
            duration_ms,
        },
        None => RunningToolOutcome::Cancelled { duration_ms },
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
    mut cancel_rx: oneshot::Receiver<()>,
    approval_rx: &mut tokio_mpsc::UnboundedReceiver<ApprovalResponse>,
    trust_rx: &mut watch::Receiver<TrustMode>,
    pending_msg_id: &str,
    response: &ModelFunctionResponse,
    round: usize,
    request_tools: &[ToolSpec],
    tool_history: &mut ToolCallHistory,
) -> ToolBatchOutcome {
    let calls = record_tool_calls(ctx, pending_msg_id, response, round);
    let mut needs_failure_recovery = false;

    for call in calls {
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

        let approval = run_cancelable_child(&mut cancel_rx, |child_cancel| {
            request_tool_approval(
                ctx,
                child_cancel,
                approval_rx,
                trust_rx,
                call,
                &args_summary,
            )
        })
        .await;
        match approval {
            ToolApprovalOutcome::Approved => {}
            ToolApprovalOutcome::Rejected => {
                record_rejected_tool_call(ctx, call, &args_summary);
                return ToolBatchOutcome::Completed;
            }
            ToolApprovalOutcome::Cancelled => return ToolBatchOutcome::Cancelled,
        }

        let running_tool = run_cancelable_child(&mut cancel_rx, |child_cancel| {
            run_tool_call(ctx, child_cancel, call, &args_summary)
        })
        .await;
        let (result, duration_ms) = match running_tool {
            RunningToolOutcome::Completed {
                result,
                duration_ms,
            } => (result, duration_ms),
            RunningToolOutcome::Cancelled { duration_ms } => {
                let output = "工具调用因执行取消或会话关闭而中断。".to_string();
                let _ = ctx.stream_tx.send(StreamEvent::ToolResult {
                    name: call.name.clone(),
                    tool_call_id: Some(call.id.clone()),
                    ok: false,
                    output: output.clone(),
                    full_output: None,
                    duration_ms: Some(duration_ms),
                });
                append_tool_result_message(&mut ctx.session, &call.id, &call.name, output, true);
                // Cancel 由当前工具循环向外返回；执行期间缓存的其他命令随本轮放弃。
                return ToolBatchOutcome::Cancelled;
            }
        };

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

        if run_cancelable_child(&mut cancel_rx, |child_cancel| {
            maybe_update_context_summary(ctx, &response.usage, child_cancel)
        })
        .await
        {
            return ToolBatchOutcome::Cancelled;
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

/// 执行一个完整的对话轮次（可能多轮工具调用）。
///
/// Session 已在 deliver 阶段完整构建；本函数只消费 TurnContext 并执行本轮。
///
/// # 命令路由架构（本次重构核心）
///
/// 本函数是 `cmd_rx` 的**唯一监听者**。所有运行时命令在此处的 `select!` 中
/// 统一分发，子循环（模型请求 / 工具执行 / 审批 / 总结）不再各自持有 `cmd_rx`，
/// 而是通过 `oneshot::Receiver<()>` 接收取消信号：
/// - 收到 `Cancel` / `Shutdown` 或 `cmd_rx` 关闭时，立即 `cmd_rx.close()` 并通过
///   对应的 `cancel_tx` 通知当前子循环自行收尾，再 await 其结果后返回 `cancelled`。
/// - `Approval` / `SetTrustMode` / `CompressContext` / `ResetContext` / `InjectTool`
///   等命令被转发到各自的专用通道（`approval_tx` / `trust_tx` / `deferred_tx`），
///   由子循环在合适的时机消费。
///
/// 这样做的收益：删除了此前散落在各阶段的 `process_commands` / 命令排空逻辑、
/// 运行时消息注入（`Command::Message`）以及 `Interrupted` / `Restart` 中间状态，
/// turn 执行流程简化为「线性执行 + 可取消」。
///
/// # 两层循环
///
/// 外层 `'execute_loop` 负责 ReAct↔Summary 阶段切换（summary 判定 NeedMoreWork 时重入）；
/// 内层 `'react_loop` 负责多轮工具调用，直到无工具调用或达到 `max_tool_rounds`。
pub(super) async fn execute_turn(
    ctx: &mut TurnContext,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
) -> TurnExecutionResult {
    let mut round = 0usize;
    let mut outer_iteration = 0u32;
    let mut accumulated_usage = TokenUsage::default();
    let mut tool_history = ToolCallHistory::default();
    // stream_tx / context_limit 提前 clone/copy，避免在 select! 分支里借用 ctx
    // （ctx 的 &mut 已移交给子循环 future）。
    let stream_tx = ctx.stream_tx.clone();
    let context_limit = ctx.context_limit;
    // 三条本回合内部分发通道：子循环消费，父层 select 写入。
    let (deferred_tx, mut deferred_rx) = tokio_mpsc::unbounded_channel();
    let (approval_tx, mut approval_rx) = tokio_mpsc::unbounded_channel();
    let (trust_tx, mut trust_rx) = watch::channel(ctx.trust_mode);

    'execute_loop: loop {
        let iteration_start_round = round;
        let mut executed_tool_in_iteration = false;

        'react_loop: loop {
            apply_deferred_turn_commands(ctx, &mut deferred_rx);
            ctx.trust_mode = *trust_tx.borrow();

            if round == 0 {
                debug_assert!(
                    ctx.session.system_prompt_message.is_some(),
                    "TurnContext 构建前应已注入 system prompt"
                );
            }
            if round > 0 {
                let _ = ctx.stream_tx.send(StreamEvent::PhaseChanged {
                    phase: "analyzing".to_string(),
                    iteration: (round + 1) as u32,
                });
            }
            if round.saturating_sub(iteration_start_round) >= ctx.max_tool_rounds {
                break 'react_loop;
            }

            let request_tools = ctx.tools.clone();
            let (cancel_tx, cancel_rx) = oneshot::channel();
            let mut cancel_tx = Some(cancel_tx);
            let mut request_future = Box::pin(request_react_response(
                ctx,
                cancel_rx,
                request_tools.clone(),
            ));
            let mut parent_cancelled = false;
            // 命令分发 + 子循环驱动的统一模板（request / compression / text / tool /
            // summary 五处复用同一结构）。Cancel/Shutdown/通道关闭 → 通知子循环
            // 收尾并标记 parent_cancelled；其余命令转发到专用通道。子循环通过
            // cancel_rx 感知取消后自行 break 并返回部分结果，父层 await 拿回。
            // NOTE: 当前存在命令分发的样板重复，后续可抽公共 helper 收敛。
            let request_outcome = loop {
                tokio::select! {
                    biased;
                    command = cmd_rx.recv() => match command {
                        Some(Command::Cancel | Command::Shutdown) | None => {
                            cmd_rx.close();
                            if let Some(cancel_tx) = cancel_tx.take() {
                                let _ = cancel_tx.send(());
                            }
                            parent_cancelled = true;
                            break (&mut request_future).await;
                        }
                        Some(Command::Approval { request_id, approved }) => {
                            let _ = approval_tx.send(ApprovalResponse {
                                request_id,
                                approved,
                            });
                        }
                        Some(Command::SetTrustMode(mode)) => {
                            let _ = trust_tx.send(mode);
                        }
                        Some(Command::CompressContext) => {
                            let _ = deferred_tx.send(DeferredTurnCommand::CompressContext);
                        }
                        Some(Command::ResetContext) => {
                            let _ = deferred_tx.send(DeferredTurnCommand::ResetContext);
                        }
                        Some(Command::InjectTool { tool_name, payload }) => {
                            let _ = deferred_tx.send(DeferredTurnCommand::InjectTool {
                                tool_name,
                                payload,
                            });
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
                            &mut accumulated_usage,
                            usage,
                            source,
                            emit_event,
                        ),
                    },
                    outcome = &mut request_future => break outcome,
                }
            };
            drop(request_future);
            if parent_cancelled {
                match request_outcome {
                    ReactRequestOutcome::Completed {
                        response_result: Ok(response),
                        ..
                    } => accumulated_usage.accumulate(&response.usage),
                    ReactRequestOutcome::Cancelled { usage } => {
                        accumulated_usage.accumulate(&usage)
                    }
                    ReactRequestOutcome::Completed { .. } => {}
                }
                return TurnExecutionResult::cancelled(accumulated_usage);
            }

            let (pending_msg_id, response) = match request_outcome {
                ReactRequestOutcome::Completed {
                    pending_msg_id,
                    response_result: Ok(response),
                } => {
                    apply_deferred_turn_commands(ctx, &mut deferred_rx);
                    ctx.trust_mode = *trust_tx.borrow();
                    (pending_msg_id, response)
                }
                ReactRequestOutcome::Completed {
                    response_result: Err(error),
                    ..
                } => {
                    let error_message = error.to_string();
                    let should_compress = error_message.contains("context_window_exceeded")
                        || error_message.contains("context_length_exceeded")
                        || (error_message.contains("content_blocks=0")
                            && error_message.contains("stop_reason=end_turn"));
                    if should_compress {
                        tracing::warn!("检测到上下文超限，尝试强制压缩");
                        let before_summary_up_to = ctx.session.summary_up_to;
                        let forced_usage = TokenUsage {
                            prompt_tokens: context_limit,
                            completion_tokens: 0,
                            total_tokens: context_limit,
                            prompt_cache_hit_tokens: None,
                            prompt_cache_miss_tokens: None,
                        };
                        let (cancel_tx, cancel_rx) = oneshot::channel();
                        let mut cancel_tx = Some(cancel_tx);
                        let mut compression_future =
                            Box::pin(maybe_update_context_summary(ctx, &forced_usage, cancel_rx));
                        let mut parent_cancelled = false;
                        let compression_cancelled = loop {
                            tokio::select! {
                                biased;
                                command = cmd_rx.recv() => match command {
                                    Some(Command::Cancel | Command::Shutdown) | None => {
                                        cmd_rx.close();
                                        if let Some(cancel_tx) = cancel_tx.take() {
                                            let _ = cancel_tx.send(());
                                        }
                                        parent_cancelled = true;
                                        break (&mut compression_future).await;
                                    }
                                    Some(Command::Approval { .. }) => {}
                                    Some(Command::SetTrustMode(mode)) => {
                                        let _ = trust_tx.send(mode);
                                    }
                                    Some(Command::CompressContext) => {
                                        let _ = deferred_tx.send(DeferredTurnCommand::CompressContext);
                                    }
                                    Some(Command::ResetContext) => {
                                        let _ = deferred_tx.send(DeferredTurnCommand::ResetContext);
                                    }
                                    Some(Command::InjectTool { tool_name, payload }) => {
                                        let _ = deferred_tx.send(DeferredTurnCommand::InjectTool {
                                            tool_name,
                                            payload,
                                        });
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
                                        &mut accumulated_usage,
                                        usage,
                                        source,
                                        emit_event,
                                    ),
                                },
                                result = &mut compression_future => break result,
                            }
                        };
                        drop(compression_future);
                        if parent_cancelled || compression_cancelled {
                            return TurnExecutionResult::cancelled(accumulated_usage);
                        }
                        if ctx.session.summary_up_to > before_summary_up_to {
                            continue 'react_loop;
                        }
                    }
                    persist_error(ctx, format!("ReAct 循环请求失败：{error_message}"));
                    return TurnExecutionResult::failed(accumulated_usage, error_message);
                }
                ReactRequestOutcome::Cancelled { usage } => {
                    accumulated_usage.accumulate(&usage);
                    return TurnExecutionResult::cancelled(accumulated_usage);
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
                let (cancel_tx, cancel_rx) = oneshot::channel();
                let mut cancel_tx = Some(cancel_tx);
                let mut text_future = Box::pin(handle_text_response(
                    ctx,
                    cancel_rx,
                    &pending_msg_id,
                    &response,
                    TextResponseState {
                        round,
                        outer_iteration,
                        executed_tool: executed_tool_in_iteration,
                    },
                ));
                let mut parent_cancelled = false;
                let text_outcome = loop {
                    tokio::select! {
                        biased;
                        command = cmd_rx.recv() => match command {
                            Some(Command::Cancel | Command::Shutdown) | None => {
                                cmd_rx.close();
                                if let Some(cancel_tx) = cancel_tx.take() {
                                    let _ = cancel_tx.send(());
                                }
                                parent_cancelled = true;
                                break (&mut text_future).await;
                            }
                            Some(Command::Approval { .. }) => {}
                            Some(Command::SetTrustMode(mode)) => {
                                let _ = trust_tx.send(mode);
                            }
                            Some(Command::CompressContext) => {
                                let _ = deferred_tx.send(DeferredTurnCommand::CompressContext);
                            }
                            Some(Command::ResetContext) => {
                                let _ = deferred_tx.send(DeferredTurnCommand::ResetContext);
                            }
                            Some(Command::InjectTool { tool_name, payload }) => {
                                let _ = deferred_tx.send(DeferredTurnCommand::InjectTool {
                                    tool_name,
                                    payload,
                                });
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
                                &mut accumulated_usage,
                                usage,
                                source,
                                emit_event,
                            ),
                        },
                        outcome = &mut text_future => break outcome,
                    }
                };
                drop(text_future);
                if parent_cancelled {
                    return TurnExecutionResult::cancelled(accumulated_usage);
                }
                apply_deferred_turn_commands(ctx, &mut deferred_rx);
                ctx.trust_mode = *trust_tx.borrow();
                match text_outcome {
                    TextResponseOutcome::Completed => {
                        return TurnExecutionResult::success(accumulated_usage);
                    }
                    TextResponseOutcome::EnterSummary => break 'react_loop,
                    TextResponseOutcome::Cancelled => {
                        return TurnExecutionResult::cancelled(accumulated_usage);
                    }
                }
            }

            executed_tool_in_iteration = true;
            let (cancel_tx, cancel_rx) = oneshot::channel();
            let mut cancel_tx = Some(cancel_tx);
            let mut tool_future = Box::pin(execute_tool_batch(
                ctx,
                cancel_rx,
                &mut approval_rx,
                &mut trust_rx,
                &pending_msg_id,
                &response,
                round,
                &request_tools,
                &mut tool_history,
            ));
            let mut parent_cancelled = false;
            let tool_outcome = loop {
                tokio::select! {
                    biased;
                    command = cmd_rx.recv() => match command {
                        Some(Command::Cancel | Command::Shutdown) | None => {
                            cmd_rx.close();
                            if let Some(cancel_tx) = cancel_tx.take() {
                                let _ = cancel_tx.send(());
                            }
                            parent_cancelled = true;
                            break (&mut tool_future).await;
                        }
                        Some(Command::Approval { request_id, approved }) => {
                            let _ = approval_tx.send(ApprovalResponse {
                                request_id,
                                approved,
                            });
                        }
                        Some(Command::SetTrustMode(mode)) => {
                            let _ = trust_tx.send(mode);
                        }
                        Some(Command::CompressContext) => {
                            let _ = deferred_tx.send(DeferredTurnCommand::CompressContext);
                        }
                        Some(Command::ResetContext) => {
                            let _ = deferred_tx.send(DeferredTurnCommand::ResetContext);
                        }
                        Some(Command::InjectTool { tool_name, payload }) => {
                            let _ = deferred_tx.send(DeferredTurnCommand::InjectTool {
                                tool_name,
                                payload,
                            });
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
                            &mut accumulated_usage,
                            usage,
                            source,
                            emit_event,
                        ),
                    },
                    outcome = &mut tool_future => break outcome,
                }
            };
            drop(tool_future);
            if parent_cancelled {
                return TurnExecutionResult::cancelled(accumulated_usage);
            }
            apply_deferred_turn_commands(ctx, &mut deferred_rx);
            ctx.trust_mode = *trust_tx.borrow();
            match tool_outcome {
                ToolBatchOutcome::Continue => continue 'react_loop,
                ToolBatchOutcome::Completed => {
                    return TurnExecutionResult::success(accumulated_usage);
                }
                ToolBatchOutcome::Cancelled => {
                    return TurnExecutionResult::cancelled(accumulated_usage);
                }
            }
        }

        let (cancel_tx, cancel_rx) = oneshot::channel();
        let mut cancel_tx = Some(cancel_tx);
        // 总结阶段以一个 async 块封装：内部可能串行调用 run_summary_phase →
        // force_final_response，两者共享同一个 cancel_rx（第一层未触发取消时
        // 第二层可复用）。闭包内修改 outer_iteration 会反映到外层（future drop
        // 后借用归还），用于 NeedMoreWork 后下一轮 'execute_loop 的迭代计数。
        let mut summary_future = Box::pin(async {
            let mut cancel_rx = cancel_rx;
            let summary_result = run_cancelable_child(&mut cancel_rx, |child_cancel| {
                ctx.run_summary_phase(child_cancel, outer_iteration + 1)
            })
            .await;
            match summary_result {
                SummaryPhaseResult::Completed(usage) => SummaryPhaseResult::Completed(usage),
                SummaryPhaseResult::NeedMoreWork { reason, usage } => {
                    outer_iteration += 1;
                    if outer_iteration >= ctx.max_outer_iterations {
                        let force_result = run_cancelable_child(&mut cancel_rx, |child_cancel| {
                            ctx.force_final_response(child_cancel, ForceFinalReason::OuterLimit)
                        })
                        .await;
                        return match force_result {
                            ForceFinalResult::Completed(force_usage) => {
                                let mut combined_usage = usage;
                                combined_usage.accumulate(&force_usage);
                                SummaryPhaseResult::Completed(combined_usage)
                            }
                            ForceFinalResult::Cancelled => SummaryPhaseResult::Cancelled(usage),
                            ForceFinalResult::Failed(message) => {
                                SummaryPhaseResult::Failed { message, usage }
                            }
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
                    SummaryPhaseResult::NeedMoreWork { reason, usage }
                }
                SummaryPhaseResult::Cancelled(usage) => {
                    ctx.session.persist_to_disk();
                    SummaryPhaseResult::Cancelled(usage)
                }
                SummaryPhaseResult::Failed { message, usage } => {
                    persist_error(ctx, format!("总结阶段失败：{message}"));
                    let force_result = run_cancelable_child(&mut cancel_rx, |child_cancel| {
                        ctx.force_final_response(child_cancel, ForceFinalReason::SummaryError)
                    })
                    .await;
                    match force_result {
                        ForceFinalResult::Completed(force_usage) => {
                            let mut combined_usage = usage;
                            combined_usage.accumulate(&force_usage);
                            SummaryPhaseResult::Completed(combined_usage)
                        }
                        ForceFinalResult::Cancelled => SummaryPhaseResult::Cancelled(usage),
                        ForceFinalResult::Failed(force_message) => SummaryPhaseResult::Failed {
                            message: format!(
                                "总结阶段失败：{message}；强制最终回复失败：{force_message}"
                            ),
                            usage,
                        },
                    }
                }
            }
        });
        let mut parent_cancelled = false;
        let summary_result = loop {
            tokio::select! {
                biased;
                command = cmd_rx.recv() => match command {
                    Some(Command::Cancel | Command::Shutdown) | None => {
                        cmd_rx.close();
                        if let Some(cancel_tx) = cancel_tx.take() {
                            let _ = cancel_tx.send(());
                        }
                        parent_cancelled = true;
                        break (&mut summary_future).await;
                    }
                    Some(Command::Approval { .. }) => {}
                    Some(Command::SetTrustMode(mode)) => {
                        let _ = trust_tx.send(mode);
                    }
                    Some(Command::CompressContext) => {
                        let _ = deferred_tx.send(DeferredTurnCommand::CompressContext);
                    }
                    Some(Command::ResetContext) => {
                        let _ = deferred_tx.send(DeferredTurnCommand::ResetContext);
                    }
                    Some(Command::InjectTool { tool_name, payload }) => {
                        let _ = deferred_tx.send(DeferredTurnCommand::InjectTool {
                            tool_name,
                            payload,
                        });
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
                        &mut accumulated_usage,
                        usage,
                        source,
                        emit_event,
                    ),
                },
                result = &mut summary_future => break result,
            }
        };
        drop(summary_future);
        let summary_usage = match &summary_result {
            SummaryPhaseResult::Completed(usage) | SummaryPhaseResult::Cancelled(usage) => usage,
            SummaryPhaseResult::NeedMoreWork { usage, .. }
            | SummaryPhaseResult::Failed { usage, .. } => usage,
        };
        accumulated_usage.accumulate(summary_usage);
        if parent_cancelled {
            return TurnExecutionResult::cancelled(accumulated_usage);
        }
        apply_deferred_turn_commands(ctx, &mut deferred_rx);
        ctx.trust_mode = *trust_tx.borrow();

        match summary_result {
            SummaryPhaseResult::Completed(_) => {
                return TurnExecutionResult::success(accumulated_usage);
            }
            SummaryPhaseResult::NeedMoreWork { .. } => {
                tool_history.clear();
                continue 'execute_loop;
            }
            SummaryPhaseResult::Cancelled(_) => {
                return TurnExecutionResult::cancelled(accumulated_usage);
            }
            SummaryPhaseResult::Failed { message, .. } => {
                return TurnExecutionResult::failed(accumulated_usage, message);
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
    use crate::model::SingleProviderClient;
    use crate::model::{ToolCall, ToolSpec};
    use crate::observe::Observer;
    use crate::permission::TrustMode;
    use crate::prompt::SystemPromptConfig;
    use crate::session::{MessageRole, Session};
    use crate::tool::ToolResult;
    use crate::tool_override::ToolOverrideHandler;
    use crate::turn_context::TurnContext;
    use std::collections::HashMap;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use tiangong_llm::{ModelEndpoint, ProviderProtocol};
    use tiangong_types::StreamEvent;
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
        harness.drain_stream();
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

        // cmd_tx 是 Send 的,可以移到独立任务里延时发送 Cancel;
        // 主任务独占 ctx + cmd_rx 跑 execute_turn。
        let cmd_tx = harness.cmd_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let _ = cmd_tx.send(Command::Cancel);
        });

        let result = execute_turn(&mut harness.ctx, &mut harness.cmd_rx).await;

        assert!(
            matches!(result.outcome, TurnExecutionOutcome::Cancelled),
            "收到 Cancel 应返回 Cancelled,实际: {:?}",
            result.outcome
        );
        harness.drain_stream();
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
}
