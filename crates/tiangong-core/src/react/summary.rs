//! 总结阶段（Summary phase）。
//!
//! 工具执行阶段结束后，由主模型基于全部结果判断任务完成度并产出最终回复。
//! 本模块承载：
//! - `SummaryPhaseResult`：总结阶段的四种结果（完成/需继续/取消/失败）
//! - `ReactEngine::run_summary_phase`：执行总结 LLM 调用、解析 [NEED_MORE_WORK]
//!   标记、处理取消与上下文压缩
//! - `parse_summary_need_more_work` / `strip_summary_marker`：总结标记解析
//!
//! `force_final_response`（兜底最终回复）仍位于 `context.rs`，二者共同构成
//! 「工具循环结束后如何产出最终回复」的完整路径（后续按 Finalizer 进一步收敛）。

use std::sync::mpsc::Sender as StdSender;

use tokio::sync::mpsc as tokio_mpsc;

use crate::core::command::Command;
use crate::model::TokenUsage;
use crate::react::context::{
    emit_token_usage, maybe_update_context_summary, request_for_summary_phase,
    select_client_for_request,
};
use crate::react::message::append_or_reuse_user_message;
use crate::session::{MessageRole, Session};
use crate::stream_throttle::ThrottledStreamSink;
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
