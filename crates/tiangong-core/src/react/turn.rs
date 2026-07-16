//! 单个 turn 的生命周期与 ReAct 执行流程。
//!
//! [`TurnContext`] 定义在 `crate::turn_context`,是 turn 级能力容器。本文件负责
//! turn 的启动、ReAct 循环、插件回调、状态提交与最终持久化。

use std::collections::{HashMap, HashSet};

use tokio::sync::mpsc as tokio_mpsc;

use crate::context::assembler::filter_background_task_tools;
use crate::core::command::{Command, PendingCommandEffect};
use crate::formatting::{format_llm_output_message, format_tool_trace_message};
use crate::model::{ModelRequest, TokenUsage, ToolSpec};
use crate::permission::TrustMode;
use crate::react::context::{
    emit_token_usage, maybe_update_context_summary, persist_error, select_client_for_request,
};
use crate::react::message::*;
use crate::runtime::LlmOutputRecord;
use crate::session::{Message, MessageRole, Session};
use crate::stream_throttle::ThrottledStreamSink;
use crate::turn_context::TurnContext;
use tiangong_types::{StreamEvent, StreamToolCall};

use super::cancel::{CancelSignal, abort_and_join, emit_cancel_usage, emit_cancelled};
use super::helpers::{
    drain_pending_commands_async, looks_like_final_answer, process_buffered_commands,
};
use super::summary::{ForceFinalReason, SummaryPhaseResult};

/// 执行并收尾一个完整的 turn task。
///
/// `deliver` 已完成用户消息接收并构建 [`TurnContext`]；本函数依次负责插件生命周期、
/// Agent Loop、消息协议收尾、轮次状态提交和最终持久化。
pub(crate) async fn run_turn(
    mut ctx: TurnContext,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<Command>,
) {
    // ── 固定轮次锚点 ──
    // 当前用户消息已在 deliver 阶段写入 Session。后续插件回调、耗时与状态都使用
    // 同一消息索引和 ID，避免执行过程中追加的消息改变本轮归属。
    let stream_tx = ctx.stream_tx.clone();
    let turn_started = std::time::Instant::now();
    let Some(turn_start_idx) = ctx.session.latest_user_message_index() else {
        let _ = stream_tx.send(StreamEvent::Error {
            message: "本轮 Session 缺少用户消息".to_string(),
        });
        return;
    };
    let user_msg_id = ctx.session.messages[turn_start_idx].id.clone();

    // ── 启动插件生命周期 ──
    // 插件看到的是已包含本轮用户消息的完整 Session。
    for plugin in &ctx.plugins {
        plugin.on_turn_started(&mut ctx.session, turn_start_idx);
    }

    // ── 执行 Agent Loop ──
    // execute_turn 负责模型请求、工具调用和运行时命令；返回值只包含本轮累计用量。
    let usage = execute_turn(&mut ctx, &mut cmd_rx).await;
    ctx.session.token_usage.accumulate(&usage);

    // 当前执行函数尚未返回明确终态，因此收尾层先按成功处理；单一终态合同的收敛
    // 已作为独立任务记录在 TODO.md，避免在本次结构整理中改变既有行为。
    let mut terminal = StreamEvent::Done { usage: None };

    // ── 修复消息协议并处理延迟注入 ──
    // 先为悬空的 tool_call 补齐失败结果，保证 Provider 历史满足
    // Assistant(tool_call) -> Tool(result) 的配对要求。
    let interrupted_tools = ctx
        .session
        .close_unfinished_tool_calls_with_reason("工具调用因本轮结束而中断，未执行。");
    if !interrupted_tools.is_empty() {
        for (tool_call_id, tool_name, output) in interrupted_tools {
            let _ = stream_tx.send(StreamEvent::ToolResult {
                name: tool_name,
                tool_call_id: Some(tool_call_id),
                ok: false,
                output,
                full_output: None,
                duration_ms: None,
            });
        }
        terminal = StreamEvent::Error {
            message: "本轮仍有未完成的工具调用，已安全中断".to_string(),
        };
    }

    // 悬空调用闭合后再刷新延迟注入，避免新的 Tool 消息破坏既有调用顺序。
    {
        let mut session = std::mem::replace(&mut ctx.session, Session::new("placeholder"));
        crate::react::message::flush_deferred_tool_injections(&mut session, &ctx);
        ctx.session = session;
    }

    // ── 提交轮次状态与插件收尾 ──
    // 先形成内存中的轮次状态，让取消钩子和结束钩子看到一致的执行结果。
    let elapsed_ms = turn_started.elapsed().as_millis() as u64;
    let mut status = TurnOutcome::from_terminal(&terminal).status;

    if status == tiangong_types::TurnStatus::Cancelled {
        for plugin in &ctx.plugins {
            plugin.on_cancel(&mut ctx.session).await;
        }
    }

    let mut user_msg_updated = false;
    if let Some(msg) = ctx
        .session
        .messages
        .iter_mut()
        .find(|m| m.id == user_msg_id && m.role == MessageRole::User)
    {
        msg.set_turn_result(elapsed_ms, status);
        user_msg_updated = true;
    }
    // on_turn_finished 使用与 on_turn_started 相同的起点，并可在最终落盘前处理
    // 本轮新增消息（例如建立索引或提交记忆任务）。
    for plugin in &ctx.plugins {
        plugin.on_turn_finished(&mut ctx.session, turn_start_idx);
    }

    // ── 清理运行态并最终持久化 ──
    // base64 等瞬态内容只用于本轮模型请求，不能进入磁盘会话合同。
    ctx.session.clear_transient_content();

    if let Err(error) = ctx.session.try_persist_to_disk() {
        // 最终落盘失败必须把本轮降级为 Failed，并带着失败状态再尝试保存一次。
        terminal = StreamEvent::Error {
            message: format!("最终会话持久化失败：{error}"),
        };
        status = tiangong_types::TurnStatus::Failed;
        if let Some(msg) = ctx
            .session
            .messages
            .iter_mut()
            .find(|m| m.id == user_msg_id && m.role == MessageRole::User)
        {
            msg.set_turn_result(elapsed_ms, status);
        }
        let _ = ctx.session.try_persist_to_disk();
    }

    // ── 发布权威快照和收尾终态 ──
    // 宿主依赖“用户消息终态快照在前、Done/Error 在后”关联远程请求，因此顺序不可交换。
    if user_msg_updated {
        crate::react::message::emit_session_message_upsert(&ctx.session, &ctx, &user_msg_id);
    }

    // run_turn 层的收尾终态在最终持久化之后直接发送，不再经过 forwarder 屏障。
    let _ = stream_tx.send(terminal);
}

/// `run_turn` 收尾阶段使用的单个对话轮次状态。
///
/// `status` 由终态事件推导：`Done` → Success；`Error` 文案含「取消/cancel/abort」
/// 时为 Cancelled，否则为 Failed。run_turn 据此把 `status` 与执行时长
/// 写入用户消息，供前端（含历史会话）展示。
struct TurnOutcome {
    status: tiangong_types::TurnStatus,
}

impl TurnOutcome {
    fn from_terminal(event: &StreamEvent) -> Self {
        let status = match event {
            StreamEvent::Done { .. } => tiangong_types::TurnStatus::Success,
            StreamEvent::Error { message } => {
                let lower = message.to_lowercase();
                if lower.contains("取消")
                    || lower.contains("cancel")
                    || lower.contains("abort")
                    || lower.contains("中断")
                {
                    tiangong_types::TurnStatus::Cancelled
                } else {
                    tiangong_types::TurnStatus::Failed
                }
            }
            _ => tiangong_types::TurnStatus::Failed,
        };
        Self { status }
    }
}

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

// TurnContext 定义与基础能力方法位于 `crate::turn_context`。本文件仅实现单轮
// 生命周期及 ReAct 执行流程。

// ===== turn 执行辅助 =====

fn defer_tool_injections(
    ctx: &TurnContext,
    session: &mut Session,
    injections: impl IntoIterator<Item = (String, serde_json::Value)>,
) {
    for (tool_name, payload) in injections {
        crate::react::message::defer_tool_injection(session, ctx, tool_name, payload);
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

/// 执行一个完整的对话轮次（可能多轮工具调用）。
///
/// Session 已在 deliver 阶段完整构建；本函数只消费 TurnContext 并执行本轮。
async fn execute_turn(
    ctx: &mut TurnContext,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
) -> TokenUsage {
    let mut session = std::mem::replace(&mut ctx.session, Session::new("placeholder"));
    let usage = execute_turn_with_session(ctx, &mut session, cmd_rx).await;
    ctx.session = session;
    usage
}

#[allow(clippy::too_many_arguments, unreachable_code)]
async fn execute_turn_with_session(
    ctx: &mut TurnContext,
    session: &mut Session,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
) -> TokenUsage {
    let usage_sink = ctx.turn_usage_sink().clone();
    let _usage_guard = usage_sink.bind(ctx.stream_tx.clone(), ctx.context_limit);
    let merge_plugin_usage = |acc: &mut TokenUsage| {
        acc.accumulate(&usage_sink.take_usage());
    };
    let mut round = 0usize;
    let mut outer_iteration = 0u32;
    let mut accumulated_usage = TokenUsage::default();
    // 绑定本轮 turn-scoped 插件 usage sink：插件经 PluginFeedbackTx.report_token_usage
    // 即时累加到本轮并立即发送 StreamEvent::TokenUsage（不走命令队列，避免被
    // drain_pending_commands_async 等 drain 吞掉）。_usage_guard drop 时自动解绑，迟到的 usage 不会
    // 计入下一轮。每个 return accumulated_usage 前先 merge_plugin_usage 折算插件用量。

    let mut successful_tool_call_keys = HashSet::new();
    let mut failed_tool_call_keys: HashMap<String, String> = HashMap::new();
    let mut failed_tool_names = HashSet::new();
    // 从 session 最后一条用户消息提取初始 user_input
    let mut user_input = session
        .messages
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::User)
        .map(|m| m.text_content())
        .unwrap_or_default();

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
            if round == 0 {
                debug_assert!(
                    session.system_prompt_message.is_some(),
                    "TurnContext 构建前应已注入 system prompt"
                );
            }
            match drain_pending_commands_async(session, ctx, cmd_rx) {
                PendingCommandEffect::Terminate => {
                    merge_plugin_usage(&mut accumulated_usage);
                    return accumulated_usage;
                }
                PendingCommandEffect::Shutdown => {
                    merge_plugin_usage(&mut accumulated_usage);
                    return accumulated_usage;
                }
                PendingCommandEffect::MessagesInjected {
                    current_agent_input,
                    ..
                } => {
                    successful_tool_call_keys.clear();
                    failed_tool_call_keys.clear();
                    failed_tool_names.clear();
                    if let Some(input) = current_agent_input {
                        user_input = input;
                        round = 0;
                        outer_iteration = 0;
                        session.persist_to_disk();
                        continue 'outer;
                    }
                }
                PendingCommandEffect::None => {}
            }
            crate::react::message::flush_deferred_tool_injections(session, ctx);

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
                break 'react_loop;
            }

            let request_tools = tools_for_current_turn(&ctx.tools, session, &user_input);

            let (thinking, reasoning_effort, thinking_disabled) = build_thinking_config(ctx);
            let req = ModelRequest {
                session_title: session.title.clone(),
                // 当前用户消息已在 deliver 阶段写入 session.messages。
                // ReAct 多轮继续请求时不能再次追加 user_input，否则模型会把同一请求
                // 误认为新的用户消息，反复从第一步重新开始。
                user_input: String::new(),
                context: session.context(),
                thinking,
                reasoning_effort,
                thinking_disabled,
            };

            let pending_msg_id = scru128::new().to_string();
            // 工具执行阶段的流式文本作为过程性输出（ReactText），前端紧凑展示。
            let sink = ThrottledStreamSink::with_text_kind(
                pending_msg_id.clone(),
                ctx.stream_tx.clone(),
                crate::stream_throttle::StreamTextKind::React,
            );

            // async 流式调用 + select! 取消
            let (chunk_tx, mut chunk_rx) =
                tokio_mpsc::unbounded_channel::<crate::model::ModelStreamChunk>();
            let client = select_client_for_request(ctx, &req).clone();
            let req_clone = req.clone();
            let tools_clone = request_tools.clone();
            let mut llm_fut = Some(tokio::task::spawn(async move {
                client
                    .stream_function_calls_with_tool_choice(req_clone, tools_clone, None, chunk_tx)
                    .await
            }));
            let mut stream_interruption = None;
            let mut streamed_text = String::new();
            let mut streamed_reasoning = String::new();
            let mut streaming_usage = tiangong_types::TokenUsage::default();
            let response_result: anyhow::Result<crate::model::ModelFunctionResponse> = loop {
                tokio::select! {
                    biased;
                    cmd_opt = cmd_rx.recv() => {
                        match cmd_opt {
                            Some(Command::Shutdown) => {
                                break Err(anyhow::Error::new(CancelSignal::Abort));
                            }
                            Some(Command::Cancel) | None => {
                                break Err(anyhow::Error::new(CancelSignal::Abort));
                            }
                            Some(Command::Message {
                                prepared,
                                message_id,
                            }) => {
                                sink.flush();
                                let message_count_before_interruption = session.messages.len();
                                if !streamed_text.trim().is_empty()
                                    || !streamed_reasoning.trim().is_empty()
                                {
                                    crate::react::message::upsert_assistant_text_message(
                                        session,
                                        &pending_msg_id,
                                        &streamed_text,
                                        &streamed_reasoning,
                                        crate::session::MessagePhase::React,
                                    );
                                }
                                match accept_runtime_user_message(
                                    session,
                                    &ctx.stream_tx,
                                    message_id,
                                    prepared,
                                ) {
                                    Ok(input) => {
                                        stream_interruption = Some(input);
                                        if let Some(handle) = llm_fut.take() {
                                            abort_and_join(handle).await;
                                        }
                                        break Err(anyhow::anyhow!(
                                            "模型响应已被新的用户消息中断"
                                        ));
                                    },
                                    Err(err) => {
                                        session
                                            .messages
                                            .truncate(message_count_before_interruption);
                                        tracing::warn!(
                                            error = %err,
                                            "流式阶段追加用户消息持久化失败"
                                        );
                                    },
                                }
                            }
                            Some(Command::Approval { .. }) => {}
                            Some(Command::InjectTool { tool_name, payload }) => {
                                crate::react::message::inject_tool_to_session(
                                    session,
                                    ctx,
                                    &tool_name,
                                    &payload,
                                );
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
                                crate::core::reset_context_for_session(
                                    session,
                                    ctx,
                                );
                            }
                            Some(Command::EmitStreamEvent(ev)) => {
                                let ev = *ev;
                                let _ = ctx.stream_tx.send(ev);
                            }
                            Some(Command::SetTrustMode(mode)) => {
                                ctx.trust_mode = mode;
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
                                streamed_text.push_str(&chunk.content);
                                streamed_reasoning.push_str(&chunk.reasoning_content);
                                sink.push_chunk(&chunk)
                            }
                            None => {
                                let response_result = match llm_fut.take().unwrap().await {
                                    Ok(r) => r,
                                    Err(e) if e.is_cancelled() => {
                                        sink.finish();
                                        accumulated_usage.accumulate(&streaming_usage);
                                        emit_cancel_usage(&ctx.stream_tx,
                                            &streaming_usage,
                                            ctx.context_limit,
                                        );
                                        merge_plugin_usage(&mut accumulated_usage);
                                        return accumulated_usage;
                                    },
                                    Err(e) => Err(anyhow::anyhow!(e.to_string())),
                                };
                                break response_result;
                            }
                        }
                    }
                }
            };
            sink.finish();

            if !streamed_text.trim().is_empty() || !streamed_reasoning.trim().is_empty() {
                crate::react::message::upsert_assistant_text_message(
                    session,
                    &pending_msg_id,
                    &streamed_text,
                    &streamed_reasoning,
                    crate::session::MessagePhase::React,
                );
                crate::react::message::emit_session_message_upsert(session, ctx, &pending_msg_id);
            }

            if let Some(input) = stream_interruption {
                accumulated_usage.accumulate(&streaming_usage);
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
                successful_tool_call_keys.clear();
                failed_tool_call_keys.clear();
                failed_tool_names.clear();
                user_input = input;
                round = 0;
                outer_iteration = 0;
                session.persist_to_disk();
                continue 'outer;
            }

            let response = match response_result {
                Ok(r) => r,
                Err(err) => {
                    if let Some(signal) = CancelSignal::from_error(&err) {
                        let CancelSignal::Abort = signal;
                        if let Some(handle) = llm_fut.take() {
                            abort_and_join(handle).await;
                        }
                        accumulated_usage.accumulate(&streaming_usage);
                        emit_cancel_usage(&ctx.stream_tx, &streaming_usage, ctx.context_limit);
                        merge_plugin_usage(&mut accumulated_usage);
                        return accumulated_usage;
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
                        let compression_cancelled =
                            crate::react::context::maybe_update_context_summary(
                                session,
                                ctx,
                                &tiangong_types::TokenUsage {
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
                            emit_cancelled(&ctx.stream_tx);
                            merge_plugin_usage(&mut accumulated_usage);
                            return accumulated_usage;
                        }
                        if session.summary_up_to > before_summary_up_to {
                            continue 'react_loop;
                        }
                    }
                    let _ = ctx.stream_tx.send(StreamEvent::Error {
                        message: err_msg.clone(),
                    });
                    // 持久化错误到 session，避免前端 Error 事件时序丢失导致中断无痕迹。
                    persist_error(session, format!("ReAct 循环请求失败：{err_msg}"));
                    {
                        merge_plugin_usage(&mut accumulated_usage);
                        return accumulated_usage;
                    }
                }
            };

            accumulated_usage.accumulate(&response.usage);
            emit_token_usage(
                &ctx.stream_tx,
                &response.usage,
                Some(response.usage.prompt_tokens.max(session.current_tokens)),
                ctx.context_limit,
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
                crate::react::message::upsert_assistant_text_message(
                    session,
                    &pending_msg_id,
                    &response.text,
                    &response.reasoning_content,
                    crate::session::MessagePhase::React,
                );
                if let Some(message) = session
                    .messages
                    .iter_mut()
                    .find(|message| message.id == pending_msg_id)
                {
                    message.reasoning_signature = response.reasoning_signature.clone();
                }
                crate::react::message::emit_session_message_upsert(session, ctx, &pending_msg_id);
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
                let compression_cancelled =
                    maybe_update_context_summary(session, ctx, &response.usage, cmd_rx).await;
                if compression_cancelled {
                    emit_cancelled(&ctx.stream_tx);
                    merge_plugin_usage(&mut accumulated_usage);
                    return accumulated_usage;
                }

                // 简单问答快速路径：首轮、本轮未执行工具、未注入新消息且 LLM 已给出
                // 实质文本回复时，直接将该回复提升为最终回复（Summary），跳过总结阶段
                // 的额外模型调用，满足"简单问答零额外开销"。
                if outer_iteration == 0
                    && !executed_tool_in_iteration
                    && !response.text.trim().is_empty()
                {
                    if let Some(message) = session
                        .messages
                        .iter_mut()
                        .find(|message| message.id == pending_msg_id)
                    {
                        // 该消息此前已标记为 phase=React（上方无工具调用分支），
                        // 此处提升为 Summary：必须重新落盘，否则会话恢复后 phase 仍为
                        // React，前端会把最终回复折叠为过程内容（双总结问题的根因）。
                        message.phase = crate::session::MessagePhase::Summary;
                    }
                    crate::react::message::emit_session_message_upsert(
                        session,
                        ctx,
                        &pending_msg_id,
                    );
                    session.persist_to_disk();
                    merge_plugin_usage(&mut accumulated_usage);
                    let _ = ctx.stream_tx.send(StreamEvent::Done {
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
                    if let Some(message) = session
                        .messages
                        .iter_mut()
                        .find(|message| message.id == pending_msg_id)
                    {
                        message.phase = crate::session::MessagePhase::Summary;
                    }
                    crate::react::message::emit_session_message_upsert(
                        session,
                        ctx,
                        &pending_msg_id,
                    );
                    session.persist_to_disk();
                    merge_plugin_usage(&mut accumulated_usage);
                    let _ = ctx.stream_tx.send(StreamEvent::Done {
                        usage: Some(accumulated_usage.clone()),
                    });
                    return accumulated_usage;
                }

                break 'react_loop;
            }

            // 工具调用
            let executable_calls = response.tool_calls.iter().collect::<Vec<_>>();
            // 此处 tool_calls 必非空（上方 is_empty 分支已 break 'react_loop）；
            // 用 debug_assert! 固化这一不变式，避免后续维护误删上方分支后留下静默死代码。
            debug_assert!(
                !executable_calls.is_empty(),
                "react_loop tool_calls 非空不变式被破坏"
            );
            let tool_names: Vec<String> = executable_calls.iter().map(|c| c.name.clone()).collect();
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
            let _ = ctx.stream_tx.send(StreamEvent::ToolCalls {
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
            crate::react::message::emit_session_message_upsert(session, ctx, &pending_msg_id);

            // 执行工具
            let mut need_failure_recovery_prompt = false;
            for call in executable_calls {
                // 本轮已尝试执行工具调用（无论成功/失败/跳过），标记以阻止
                // 简单问答快速路径把后续无 tool_calls 的回复误判为直接回复。
                executed_tool_in_iteration = true;
                match drain_pending_commands_async(session, ctx, cmd_rx) {
                    PendingCommandEffect::Terminate => {
                        merge_plugin_usage(&mut accumulated_usage);
                        return accumulated_usage;
                    }
                    PendingCommandEffect::Shutdown => {
                        merge_plugin_usage(&mut accumulated_usage);
                        return accumulated_usage;
                    }
                    PendingCommandEffect::MessagesInjected {
                        current_agent_input,
                        ..
                    } => {
                        successful_tool_call_keys.clear();
                        failed_tool_call_keys.clear();
                        failed_tool_names.clear();
                        if let Some(input) = current_agent_input {
                            user_input = input;
                            round = 0;
                            outer_iteration = 0;
                            session.persist_to_disk();
                            continue 'outer;
                        }
                        session.persist_to_disk();
                    }
                    PendingCommandEffect::None => {}
                }

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
                        full_output: Some(message.clone()),
                        duration_ms: None,
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

                let args_summary = format_tool_args_summary(call);
                let tool_call_key = tool_call_dedupe_key(&call.name, &call.arguments);
                if successful_tool_call_keys.contains(&tool_call_key) {
                    append_duplicate_tool_result(session, ctx, &call.id, &call.name);
                    continue;
                }
                if let Some(original_error) = failed_tool_call_keys.get(&tool_call_key).cloned() {
                    let repeated_failure = ToolFailureRecord::repeated(
                        &call.name,
                        &call.id,
                        args_summary.clone(),
                        original_error,
                    );
                    append_repeated_failed_tool_result(
                        session,
                        ctx,
                        &call.id,
                        &call.name,
                        &repeated_failure.render_for_model(),
                    );
                    failed_tool_names.insert(call.name.clone());
                    need_failure_recovery_prompt = true;
                    continue;
                }

                let trust_mode = format!("{:?}", ctx.trust_mode);
                if ctx.trust_mode == TrustMode::FullTrust {
                    // FullTrust 放行一切：记录审批通过的审计后直接执行工具。
                    ctx.observer.audit_permission(
                        &session.id,
                        &call.name,
                        "approved",
                        &trust_mode,
                        (!args_summary.is_empty()).then_some(args_summary.as_str()),
                    );
                } else {
                    // 非 FullTrust：统一走审批流程（发 ApprovalNeeded 事件 + 阻塞等待用户决策）。
                    let request_id = scru128::new().to_string();
                    ctx.observer.audit_permission(
                        &session.id,
                        &call.name,
                        "needs_approval",
                        &trust_mode,
                        (!args_summary.is_empty()).then_some(args_summary.as_str()),
                    );
                    let _ = ctx.stream_tx.send(StreamEvent::ApprovalNeeded {
                        request_id: request_id.clone(),
                        tool_name: call.name.clone(),
                        args_summary: args_summary.clone(),
                    });

                    enum ApprovalWaitOutcome {
                        Decision(bool),
                        CurrentInput(String),
                    }

                    let approval_outcome = loop {
                        match cmd_rx.recv().await {
                            Some(Command::Approval {
                                request_id: rid,
                                approved,
                            }) if rid == request_id => {
                                break ApprovalWaitOutcome::Decision(approved);
                            }
                            Some(Command::Shutdown) => {
                                let _ = ctx.stream_tx.send(StreamEvent::Error {
                                    message: "已取消".into(),
                                });
                                merge_plugin_usage(&mut accumulated_usage);
                                return accumulated_usage;
                            }
                            Some(Command::Cancel) | None => {
                                let _ = ctx.stream_tx.send(StreamEvent::Error {
                                    message: "已取消".into(),
                                });
                                {
                                    merge_plugin_usage(&mut accumulated_usage);
                                    return accumulated_usage;
                                }
                            }
                            Some(Command::Message {
                                prepared,
                                message_id,
                            }) => {
                                match accept_runtime_user_message(
                                    session,
                                    &ctx.stream_tx,
                                    message_id,
                                    prepared,
                                ) {
                                    Ok(input) => {
                                        break ApprovalWaitOutcome::CurrentInput(input);
                                    }
                                    Err(err) => tracing::warn!(
                                        error = %err,
                                        "审批等待阶段追加用户消息持久化失败"
                                    ),
                                }
                            }
                            Some(Command::Approval { .. }) => {}
                            Some(Command::SetTrustMode(mode)) => {
                                ctx.trust_mode = mode;
                            }
                            Some(Command::InjectTool { tool_name, payload }) => {
                                defer_tool_injections(
                                    ctx,
                                    session,
                                    std::iter::once((tool_name, payload)),
                                );
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
                                crate::core::reset_context_for_session(session, ctx);
                            }
                            Some(Command::EmitStreamEvent(ev)) => {
                                let ev = *ev;
                                let _ = ctx.stream_tx.send(ev);
                            }
                        }
                    };

                    if let ApprovalWaitOutcome::CurrentInput(input) = &approval_outcome {
                        successful_tool_call_keys.clear();
                        failed_tool_call_keys.clear();
                        failed_tool_names.clear();
                        user_input = input.clone();
                        round = 0;
                        outer_iteration = 0;
                        session.persist_to_disk();
                        continue 'outer;
                    }
                    let ApprovalWaitOutcome::Decision(approved) = approval_outcome else {
                        unreachable!("current input handled above")
                    };

                    if !approved {
                        ctx.observer.audit_tool_execution(
                            &session.id,
                            &call.name,
                            false,
                            (!args_summary.is_empty()).then_some(args_summary.as_str()),
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
                            ToolFailureRecord::new(
                                &call.name,
                                &call.id,
                                args_summary.clone(),
                                ToolFailureKind::UserRejected,
                                "用户拒绝执行",
                            )
                            .render_for_model(),
                            true,
                        );
                        crate::react::message::flush_deferred_tool_injections(session, ctx);
                        session.persist_to_disk();
                        let _ = ctx.stream_tx.send(StreamEvent::ToolResult {
                            name: call.name.clone(),
                            tool_call_id: Some(call.id.clone()),
                            ok: false,
                            output: "用户拒绝执行".to_string(),
                            full_output: None,
                            duration_ms: None,
                        });
                        merge_plugin_usage(&mut accumulated_usage);
                        let _ = ctx.stream_tx.send(StreamEvent::Done {
                            usage: Some(accumulated_usage.clone()),
                        });
                        return accumulated_usage;
                    }
                }

                let _ = ctx.stream_tx.send(StreamEvent::ToolStart {
                    name: call.name.clone(),
                    args_summary: args_summary.clone(),
                });
                let tool_start_time = std::time::Instant::now();

                let mut buffered_tool_commands = Vec::new();
                // 工具处理器只在启动瞬间借用 Session；执行 Future 不再持有借用。
                let actor_id = session.id.clone();
                let mut tool_future = ctx.start_tool_call(call, session, &actor_id);
                let result = loop {
                    tokio::select! {
                        biased;
                        result = &mut tool_future => break Some(result),
                        command = cmd_rx.recv() => {
                            match command {
                                Some(Command::Approval { .. }) => {}
                                Some(Command::EmitStreamEvent(event)) => {
                                    let _ = ctx.stream_tx.send(*event);
                                },
                                Some(Command::SetTrustMode(mode)) => {
                                    ctx.trust_mode = mode;
                                },
                                Some(Command::Cancel) => {
                                    break None;
                                },
                                Some(Command::Shutdown) => {
                                    break None;
                                },
                                Some(command) => buffered_tool_commands.push(command),
                                None => {
                                    break None;
                                },
                            }
                        }
                    }
                };
                drop(tool_future);
                let Some(result) = result else {
                    let output = "工具调用因执行取消或会话关闭而中断。".to_string();
                    let _ = ctx.stream_tx.send(StreamEvent::ToolResult {
                        name: call.name.clone(),
                        tool_call_id: Some(call.id.clone()),
                        ok: false,
                        output: output.clone(),
                        full_output: None,
                        duration_ms: Some(tool_start_time.elapsed().as_millis() as u64),
                    });
                    append_tool_result_message(session, &call.id, &call.name, output, true);
                    let buffered_effect =
                        process_buffered_commands(session, ctx, buffered_tool_commands);
                    if let PendingCommandEffect::MessagesInjected {
                        current_agent_input: Some(input),
                        ..
                    } = buffered_effect
                    {
                        successful_tool_call_keys.clear();
                        failed_tool_call_keys.clear();
                        failed_tool_names.clear();
                        user_input = input;
                        round = 0;
                        outer_iteration = 0;
                        session.persist_to_disk();
                        continue 'outer;
                    }
                    merge_plugin_usage(&mut accumulated_usage);
                    let _ = ctx.stream_tx.send(StreamEvent::Error {
                        message: "已取消".into(),
                    });
                    return accumulated_usage;
                };
                let tool_llm_usage = tiangong_types::TokenUsage::default();
                let allow_memory_context = false;
                let usage_source = "";
                accumulated_usage.accumulate(&tool_llm_usage);
                emit_token_usage(
                    &ctx.stream_tx,
                    &tool_llm_usage,
                    None,
                    ctx.context_limit,
                    usage_source,
                    None,
                );

                ctx.observer.audit_tool_execution(
                    &session.id,
                    &call.name,
                    result.ok,
                    (!args_summary.is_empty()).then_some(args_summary.as_str()),
                    &result.summary,
                );
                let _ = ctx.stream_tx.send(StreamEvent::ToolResult {
                    name: call.name.clone(),
                    tool_call_id: Some(call.id.clone()),
                    ok: result.ok,
                    output: tool_result_stream_output(&result),
                    full_output: Some(tool_result_full_output(&result)),
                    // 工具产出的图片/视频已包含在 output（stdout）中，模型与前端
                    // 均可直接识别 markdown 图片语法，无需 core 额外提取媒体资产。
                    duration_ms: Some(tool_start_time.elapsed().as_millis() as u64),
                });
                append_tool_result_message(
                    session,
                    &call.id,
                    &call.name,
                    if result.ok {
                        tool_result_provider_text(&call.name, &result, allow_memory_context)
                    } else {
                        ToolFailureRecord::new(
                            &call.name,
                            &call.id,
                            args_summary.clone(),
                            classify_tool_result_failure(&result),
                            tool_result_full_output(&result),
                        )
                        .render_for_model()
                    },
                    !result.ok,
                );
                append_runtime_tool_message(
                    session,
                    &call.name,
                    format_tool_trace_message(&result),
                );
                match process_buffered_commands(session, ctx, buffered_tool_commands) {
                    PendingCommandEffect::Terminate => {
                        merge_plugin_usage(&mut accumulated_usage);
                        return accumulated_usage;
                    }
                    PendingCommandEffect::Shutdown => {
                        merge_plugin_usage(&mut accumulated_usage);
                        return accumulated_usage;
                    }
                    PendingCommandEffect::MessagesInjected {
                        current_agent_input: Some(input),
                        ..
                    } => {
                        successful_tool_call_keys.clear();
                        failed_tool_call_keys.clear();
                        failed_tool_names.clear();
                        user_input = input;
                        round = 0;
                        outer_iteration = 0;
                        session.persist_to_disk();
                        continue 'outer;
                    }
                    PendingCommandEffect::MessagesInjected { .. } | PendingCommandEffect::None => {}
                }
                if result.ok {
                    failed_tool_call_keys.remove(&tool_call_key);
                    failed_tool_names.remove(&call.name);
                    successful_tool_call_keys.insert(tool_call_key);
                } else {
                    let error_summary = ToolFailureRecord::new(
                        &call.name,
                        &call.id,
                        args_summary.clone(),
                        classify_tool_result_failure(&result),
                        tool_result_full_output(&result),
                    )
                    .render_for_model();
                    failed_tool_call_keys.insert(tool_call_key, error_summary);
                    failed_tool_names.insert(call.name.clone());
                    need_failure_recovery_prompt = true;
                }

                // 记忆候选评估已下沉到 memory 插件 on_turn_finished（从 session 重建候选，
                // 统一提交给 actor 的 pending list，反刍时自动合并）。
                let compression_cancelled =
                    maybe_update_context_summary(session, ctx, &response.usage, cmd_rx).await;
                if compression_cancelled {
                    emit_cancelled(&ctx.stream_tx);
                    merge_plugin_usage(&mut accumulated_usage);
                    return accumulated_usage;
                }

                match drain_pending_commands_async(session, ctx, cmd_rx) {
                    PendingCommandEffect::Terminate => {
                        merge_plugin_usage(&mut accumulated_usage);
                        return accumulated_usage;
                    }
                    PendingCommandEffect::Shutdown => {
                        merge_plugin_usage(&mut accumulated_usage);
                        return accumulated_usage;
                    }
                    PendingCommandEffect::MessagesInjected {
                        current_agent_input,
                        ..
                    } => {
                        successful_tool_call_keys.clear();
                        failed_tool_call_keys.clear();
                        failed_tool_names.clear();
                        if let Some(input) = current_agent_input {
                            user_input = input;
                            round = 0;
                            outer_iteration = 0;
                            session.persist_to_disk();
                            continue 'outer;
                        }
                        session.persist_to_disk();
                    }
                    PendingCommandEffect::None => {}
                }
                crate::react::message::flush_deferred_tool_injections(session, ctx);
            }

            if need_failure_recovery_prompt {
                let mut failed_tools = failed_tool_names.iter().cloned().collect::<Vec<_>>();
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
                session.messages.push(reminder);
                session.persist_to_disk();
                continue 'react_loop;
            }

            session.persist_to_disk();
        }

        // ── 总结阶段 ──
        // 内层工具执行阶段结束后，由主模型独立判断任务完成度并输出最终回复。
        let summary_result = ctx
            .run_summary_phase(session, cmd_rx, outer_iteration + 1)
            .await;
        match summary_result {
            SummaryPhaseResult::Completed(usage) => {
                accumulated_usage.accumulate(&usage);
                merge_plugin_usage(&mut accumulated_usage);
                let _ = ctx.stream_tx.send(StreamEvent::Done {
                    usage: Some(accumulated_usage.clone()),
                });
                return accumulated_usage;
            }
            SummaryPhaseResult::NeedMoreWork { reason, usage } => {
                accumulated_usage.accumulate(&usage);
                outer_iteration += 1;
                if outer_iteration >= ctx.max_outer_iterations {
                    // 重入次数已达上限，强制输出最终回复。
                    ctx.force_final_response(session, cmd_rx, ForceFinalReason::OuterLimit)
                        .await;
                    {
                        merge_plugin_usage(&mut accumulated_usage);
                        return accumulated_usage;
                    }
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
                successful_tool_call_keys.clear();
                failed_tool_call_keys.clear();
                failed_tool_names.clear();
                continue 'outer;
            }
            SummaryPhaseResult::Cancelled(usage) => {
                accumulated_usage.accumulate(&usage);
                session.persist_to_disk();
                {
                    merge_plugin_usage(&mut accumulated_usage);
                    return accumulated_usage;
                }
            }
            SummaryPhaseResult::Failed { message, usage } => {
                accumulated_usage.accumulate(&usage);
                // 初次总结失败只是可恢复的中间状态；只有强制终结也失败时，
                // run_text_finalization_llm 才发送唯一终态 Error。
                persist_error(session, format!("总结阶段失败：{message}"));
                ctx.force_final_response(session, cmd_rx, ForceFinalReason::SummaryError)
                    .await;
                {
                    merge_plugin_usage(&mut accumulated_usage);
                    return accumulated_usage;
                }
            }
            SummaryPhaseResult::Interrupted {
                current_agent_input,
                usage,
            } => {
                accumulated_usage.accumulate(&usage);
                successful_tool_call_keys.clear();
                failed_tool_call_keys.clear();
                failed_tool_names.clear();
                user_input = current_agent_input;
                round = 0;
                outer_iteration = 0;
                session.persist_to_disk();
                continue 'outer;
            }
        }
    }

    merge_plugin_usage(&mut accumulated_usage);
    accumulated_usage
}

pub(super) fn tools_for_current_turn(
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
