//! 最终化（Finalization）模块。
//!
//! 工具执行阶段结束后，如何产出最终回复的所有逻辑都集中在本模块：
//! - 正常路径：`run_summary_phase` —— 主模型基于全部结果判断完成度并输出回复
//! - 兜底路径：`force_final_response` —— 重入次数达上限或总结阶段失败时强制回复
//! - 总结提示词：`SUMMARY_PHASE_PROMPT` / `request_for_summary_phase`
//!
//! 二者此前散落在 engine.rs 与 context.rs，现统一收敛于此，使「最终回复只有一个出口」。
//! 上下文管理（压缩、token usage、system prompt 重建）仍留在 context.rs。

use std::sync::mpsc::Sender as StdSender;

use tokio::sync::mpsc as tokio_mpsc;

use crate::core::command::Command;
use crate::model::{ModelClient, ModelRequest, TokenUsage};
use crate::react::context::{
    emit_token_usage, maybe_update_context_summary, rebuild_system_prompt,
    select_client_for_request,
};
use crate::react::message::append_or_reuse_user_message;
use crate::runtime::{RuntimeEngine, use_stream_mode};
use crate::session::{Message, MessagePhase, MessageRole, Session, now_text};
use crate::stream_throttle::{StreamTextKind, ThrottledStreamSink};
use tiangong_types::StreamEvent;

use super::cancel::{CancelSignal, CancelStrategy, emit_cancel_usage};
use super::engine::{ReactEngine, TurnPhase, tools_for_current_turn};

/// 总结阶段的执行结果。
pub(super) enum SummaryPhaseResult {
    /// 任务完成，已输出最终回复。
    Completed(TokenUsage),
    /// 任务未完成，需要重新进入工具执行阶段。
    NeedMoreWork { reason: String, usage: TokenUsage },
    /// 用户取消。
    Cancelled(TokenUsage),
    /// 总结阶段 LLM 请求失败。
    Failed { message: String, usage: TokenUsage },
}

impl ReactEngine {
    /// 执行总结阶段。
    ///
    /// 由主模型（非 lite 模型）基于工具执行阶段的全部结果，判断任务完成度：
    /// - 完成 → 输出最终回复（Summary phase），返回 `Completed`
    /// - 需要用户输入 → 输出提问，返回 `Completed`（视作本轮结束）
    /// - 仍有遗漏且可继续 → 输出 [NEED_MORE_WORK]，返回 `NeedMoreWork`
    /// - 取消 → 返回 `Cancelled`
    /// - LLM 错误 → 返回 `Failed`
    pub(super) async fn run_summary_phase(
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

/// 总结阶段的判断指令（作为运行时上下文注入，不常驻 system prompt）。
///
/// 由主模型在总结阶段判断任务完成度：完成则给最终回复；需要用户提供信息则提问；
/// 仍有遗漏且本 Agent 能继续通过工具推进时，输出 [NEED_MORE_WORK] 触发重入 Loop。
pub(super) const SUMMARY_PHASE_PROMPT: &str = "\
你当前处于总结阶段。请基于以上所有工作，给出最终回复。\n\
\n\
输出原则：\n\
- 若上一轮（工具执行阶段）已经给出详实的回答/结果，请**保留其要点与细节**，不要过度精简，\n\
  只需补充结论或下一步建议即可。用户需要的是完整可用的信息，而非被压缩的摘要。\n\
- 仅当信息确实冗余、重复或与结论无关时才删减。\n\
\n\
判断逻辑：\n\
1. 如果用户请求的所有操作都已执行并得到结果，请保留要点并给出最终回复。\n\
2. 如果需要用户提供额外信息、凭据、授权、选择或确认才能继续，请直接向用户提问。\n\
3. 如果有关键步骤遗漏未执行、且你确实可以通过工具继续推进，请在回复开头输出 [NEED_MORE_WORK]，\n\
   然后简要说明还需要做什么。系统将重新进入工具执行阶段。\n\
\n\
注意：不要重复执行工具调用。不要重复已有内容。如果只是给用户后续建议，不要使用 [NEED_MORE_WORK]。";

/// 构建总结阶段的 LLM 请求。
///
/// 将 `SUMMARY_PHASE_PROMPT` 作为运行时上下文追加到对话末尾，不携带 tools，
/// 使用主模型 client，由主模型自行判断任务完成度并输出最终回复。
pub(super) fn request_for_summary_phase(session: &Session) -> ModelRequest {
    let mut context = session.context();
    context.push(
        Message::new(
            MessageRole::System,
            format!("<runtime_context>\n{SUMMARY_PHASE_PROMPT}\n</runtime_context>"),
        )
        .with_phase(MessagePhase::Normal),
    );
    ModelRequest {
        session_title: session.title.clone(),
        user_input: String::new(),
        context,
        thinking: Some(crate::model::ThinkingConfig {
            budget_tokens: 4096,
        }),
        reasoning_effort: None,
        thinking_disabled: false,
        include_media: false,
    }
}

/// 强制最终回复的触发原因。
#[derive(Debug, Clone, Copy)]
pub(super) enum ForceFinalReason {
    /// 总结阶段后重入 Loop 的次数已达上限。
    OuterLimit,
    /// 总结阶段 LLM 请求失败。
    SummaryError,
}

impl ForceFinalReason {
    fn prompt(self) -> &'static str {
        match self {
            Self::OuterLimit => {
                "任务已经过多轮迭代仍未完全完成。请基于以上所有工作给出最终回复。\n\
要求：\n\
1. 总结已完成的操作和结果。\n\
2. 如果有未完成的任务，说明原因和后续建议。\n\
3. 不要重复执行工具调用。\n\
4. 如果需要用户提供信息才能继续，请明确列出需要什么。"
            }
            Self::SummaryError => {
                "总结阶段执行失败。请基于以上所有工作，尽量给出最终回复。\n\
要求：\n\
1. 总结已完成的操作和结果。\n\
2. 如果有未完成的任务，说明原因和后续建议。\n\
3. 不要重复执行工具调用。\n\
4. 如果需要用户提供信息才能继续，请明确列出需要什么。"
            }
        }
    }
}

/// 超限时强制最终回复（兜底路径）。
///
/// 触发场景：
/// - `OuterLimit`：总结阶段判定 NeedMoreWork，但重入 Loop 次数已达上限
/// - `SummaryError`：总结阶段 LLM 请求失败
///
/// 与 `run_summary_phase`（正常路径）共同构成最终回复的唯一出口。
pub(super) fn force_final_response(
    session: &mut Session,
    engine: &RuntimeEngine,
    stream_tx: &StdSender<StreamEvent>,
    reason: ForceFinalReason,
) {
    // 确保 system prompt 已构建
    if session.system_prompt_message.is_none() {
        rebuild_system_prompt(session, engine);
    }
    // 注入提示消息到 session
    session.messages.push(Message {
        id: scru128::new().to_string(),
        role: MessageRole::Tool,
        content: vec![crate::session::ContentBlock::text(format!(
            "<system-reminder>\n{}\n</system-reminder>",
            reason.prompt()
        ))],
        reasoning_content: String::new(),
        reasoning_signature: None,
        worker_id: None,
        media: Vec::new(),
        media_migrated: true,
        elapsed_ms: None,
        turn_status: None,
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: Some("force_final_response".to_string()),
        tool_result_is_error: false,
        compact: false,
        phase: MessagePhase::Normal,
        created_at: now_text(),
    });

    let req = ModelRequest {
        session_title: session.title.clone(),
        user_input: String::new(),
        context: session.context(),
        thinking: Some(crate::model::ThinkingConfig {
            budget_tokens: 4096,
        }),
        reasoning_effort: None,
        thinking_disabled: false,
        include_media: false,
    };

    let pending_msg_id = scru128::new().to_string();

    let resp = if use_stream_mode() {
        let sink = ThrottledStreamSink::with_text_kind(
            pending_msg_id.clone(),
            stream_tx.clone(),
            StreamTextKind::Summary,
        );
        let response_result = select_client_for_request(engine, &req)
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
                crate::react::context::persist_error(
                    session,
                    format!("force_final_response（流式）失败：{err}"),
                );
                return;
            }
        }
    } else {
        let msg_id_non_stream = pending_msg_id.clone();
        match select_client_for_request(engine, &req).complete(&req) {
            Ok(r) => {
                if !r.text.is_empty() {
                    let _ = stream_tx.send(StreamEvent::SummaryText {
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
                crate::react::context::persist_error(
                    session,
                    format!("force_final_response（非流式）失败：{err}"),
                );
                return;
            }
        }
    };

    session.append_message_with_id(
        pending_msg_id,
        MessageRole::Assistant,
        resp.text,
        resp.reasoning_content,
    );
    if let Some(message) = session.messages.last_mut() {
        message.phase = MessagePhase::Summary;
        message.reasoning_signature = resp.reasoning_signature.clone();
    }
    emit_token_usage(
        stream_tx,
        &resp.usage,
        Some(resp.usage.prompt_tokens.max(session.current_tokens)),
        engine.context_limit,
        "force_final_response",
        None,
    );
    let _ = stream_tx.send(StreamEvent::Done {
        usage: Some(resp.usage.clone()),
    });
    session.persist_to_disk();
}
