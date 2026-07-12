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
use crate::model::{ModelRequest, TokenUsage};
use crate::react::context::{
    emit_token_usage, maybe_update_context_summary, rebuild_system_prompt,
    select_client_for_request,
};
use crate::react::message::{
    RuntimeMessageDisposition, accept_runtime_user_message, upsert_assistant_text_message,
};
use crate::session::{Message, MessagePhase, MessageRole, Session, now_text};
use crate::stream_throttle::{StreamTextKind, ThrottledStreamSink};
use tiangong_types::StreamEvent;

use super::cancel::{
    CancelSignal, abort_and_join, emit_cancel_usage, emit_cancelled, wait_for_abort_signal,
};
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
    /// 执行中收到新的用户消息，当前总结已停止。
    Interrupted {
        current_agent_input: String,
        usage: TokenUsage,
    },
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
        &mut self,
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
        // 总结阶段传与 ReAct 阶段相同的 tools schema（按相同 intent 过滤）以保持
        // KV cache 前缀一致，但通过 Some(ToolChoice::None) 显式禁止模型调用工具。
        //
        // 此前曾用 tool_choice 缺省（None），但 build_provider_request 在有 tools 时
        // 会把缺省回落到 ToolChoice::Auto（model.rs::build_provider_request），模型可
        // 在总结阶段发起工具调用；而本阶段只消费 response.text、忽略 response.tool_calls，
        // 误调用会出现空文本被当作最终回复。ToolChoice 现已新增 None 变体（各 provider
        // 映射为 OpenAI/DeepSeek "none"、Anthropic {"type":"none"}），显式禁用工具调用，
        // 既保留 cache 前缀又从源头杜绝误调用。
        let summary_tools = tools_for_current_turn(&self.tools, session, "");
        let mut llm_fut = Some(tokio::task::spawn(async move {
            client
                .stream_function_calls_with_tool_choice(
                    req_clone,
                    summary_tools,
                    Some(crate::model::ToolChoice::None),
                    chunk_tx,
                )
                .await
        }));
        let mut streaming_usage = tiangong_types::TokenUsage::default();
        let mut summary_interruption = None;
        let mut streamed_text = String::new();
        let mut streamed_reasoning = String::new();

        let response_result: anyhow::Result<crate::model::ModelFunctionResponse> = loop {
            tokio::select! {
                biased;
                cmd_opt = cmd_rx.recv() => {
                    match cmd_opt {
                        Some(Command::Shutdown) => {
                            self.request_shutdown();
                            break Err(anyhow::Error::new(CancelSignal::Abort));
                        }
                        Some(Command::Cancel) | None => {
                            break Err(anyhow::Error::new(CancelSignal::Abort));
                        }
                        Some(Command::Message {
                            prepared,
                            message_id,
                            trust_mode_override,
                            persistence_ack,
                        }) => {
                            sink.flush();
                            let message_count_before_interruption = session.messages.len();
                            if !streamed_text.trim().is_empty()
                                || !streamed_reasoning.trim().is_empty()
                            {
                                upsert_assistant_text_message(
                                    session,
                                    &pending_msg_id,
                                    &streamed_text,
                                    &streamed_reasoning,
                                    crate::session::MessagePhase::Summary,
                                );
                            }
                            match accept_runtime_user_message(
                                &self.engine,
                                &self.agent_id,
                                session,
                                stream_tx,
                                message_id,
                                prepared,
                                persistence_ack,
                            ) {
                                Ok(RuntimeMessageDisposition::CurrentAgentInput(input)) => {
                                    self.apply_message_trust_mode_override(trust_mode_override);
                                    summary_interruption = Some(input);
                                    if let Some(handle) = llm_fut.take() {
                                        abort_and_join(handle).await;
                                    }
                                    break Err(anyhow::anyhow!(
                                        "总结响应已被新的用户消息中断"
                                    ));
                                }
                                Ok(RuntimeMessageDisposition::RoutedToPlugin) => {}
                                Err(err) => {
                                    session.messages.truncate(message_count_before_interruption);
                                    tracing::warn!(
                                        error = %err,
                                        "总结阶段追加用户消息持久化失败"
                                    );
                                }
                            }
                        }
                        Some(Command::UpdateCwd { cwd }) => {
                            session.cwd = cwd;
                            crate::core::apply_session_cwd(session);
                            if let Some(handle) = llm_fut.take() {
                                abort_and_join(handle).await;
                            }
                            let _ = stream_tx.send(StreamEvent::Error {
                                message: "工作目录已更新，本轮已安全中断，请重新发送消息".to_string(),
                            });
                            break Err(anyhow::Error::new(CancelSignal::Abort));
                        }
                        Some(Command::UpdateSessionMetadata {
                            update,
                            persistence_ack,
                        }) => {
                            let trust_mode = self.engine.permission_gate().trust_mode_handle();
                            if let Err(error) = crate::core::apply_session_metadata_update(
                                session,
                                &trust_mode,
                                update,
                                persistence_ack,
                            ) {
                                tracing::warn!(%error, "总结阶段更新会话元数据失败");
                            }
                        }
                        Some(Command::ReloadConfig) => {}
                        Some(command @ Command::Approval { .. })
                        | Some(command @ Command::PluginControl { .. }) => {
                            self.forward_plugin_runtime_command(&command);
                        }
                        Some(Command::InjectTool { tool_name, payload }) => {
                            crate::react::message::inject_tool_to_session(
                                session,
                                stream_tx,
                                &tool_name,
                                &payload,
                            );
                        }
                        Some(Command::CommitPluginDeliveries {
                            delivery_ids,
                            tool_injections,
                            persistence_ack,
                        }) => {
                            if let Err(error) = crate::react::message::commit_plugin_deliveries(
                                session,
                                stream_tx,
                                delivery_ids,
                                tool_injections,
                                persistence_ack,
                            ) {
                                tracing::warn!(%error, "提交插件持久投递失败");
                            }
                        }
                        Some(Command::CompressContext) => {
                            let _ = stream_tx.send(StreamEvent::AgentNotification {
                                agent_id: "system".to_string(),
                                agent_label: "系统".to_string(),
                                content: "当前轮次执行中，已跳过手动压缩，请在轮次结束后重试".to_string(),
                                level: "warning".to_string(),
                            });
                        }
                        Some(Command::ResetContext) => {
                            crate::core::reset_context_for_session(
                                session,
                                stream_tx,
                                &self.engine,
                            );
                        }
                        Some(Command::EmitStreamEvent(ev)) => {
                            let ev = *ev;
                            let _ = stream_tx.send(ev);
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
                            streamed_text.push_str(&chunk.content);
                            streamed_reasoning.push_str(&chunk.reasoning_content);
                            sink.push_chunk(&chunk);
                        }
                        None => {
                            let response_result = match llm_fut.take().unwrap().await {
                                Ok(r) => r,
                                Err(e) if e.is_cancelled() => {
                                    sink.finish();
                                    persist_partial_summary(
                                        session,
                                        stream_tx,
                                        &pending_msg_id,
                                        &streamed_text,
                                        &streamed_reasoning,
                                    );
                                    emit_cancel_usage(
                                        stream_tx,
                                        &streaming_usage,
                                        self.engine.context_limit,
                                    );
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
        persist_partial_summary(
            session,
            stream_tx,
            &pending_msg_id,
            &streamed_text,
            &streamed_reasoning,
        );

        if let Some(current_agent_input) = summary_interruption {
            if streaming_usage.total_tokens > 0 {
                emit_token_usage(
                    stream_tx,
                    &streaming_usage,
                    None,
                    self.engine.context_limit,
                    "summary-interrupted",
                    None,
                );
            }
            return SummaryPhaseResult::Interrupted {
                current_agent_input,
                usage: streaming_usage,
            };
        }

        let response = match response_result {
            Ok(response) => response,
            Err(err) => {
                if let Some(signal) = CancelSignal::from_error(&err) {
                    let CancelSignal::Abort = signal;
                    if let Some(handle) = llm_fut.take() {
                        abort_and_join(handle).await;
                    }
                    emit_cancel_usage(stream_tx, &streaming_usage, self.engine.context_limit);
                    return SummaryPhaseResult::Cancelled(streaming_usage);
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

        // 兼容性防御：正常 provider 在 ToolChoice::None 下不应返回 tool_calls，
        // 但部分 OpenAI-compatible / vLLM / 第三方后端可能忽略 tool_choice: "none"。
        // 一旦出现 tool_calls 且文本为空，说明本阶段未产出有效最终回复——
        // 视作总结失败，交由上层 force_final_response 兜底，避免把空文本当作完成。
        if !response.tool_calls.is_empty() {
            tracing::warn!(
                count = response.tool_calls.len(),
                protocol = ?self.engine.client().protocol(),
                "summary phase returned tool calls despite ToolChoice::None"
            );
            if response.text.trim().is_empty() {
                return SummaryPhaseResult::Failed {
                    message: "总结阶段无视 ToolChoice::None 返回了工具调用且无文本回复".to_string(),
                    usage,
                };
            }
        }

        // 解析总结阶段标记，判定完成度（Done/AskUser/NeedMoreWork）。
        let decision = parse_summary_phase_output(&response.text);
        let summary_content = decision.payload().to_string();
        let needs_more_work = matches!(decision, SummaryDecision::NeedMoreWork(_));

        // 空正文 Done：LLM 只输出 [DONE]，表示「上一轮 ReAct 已有完整可用的最终答复」。
        // 此时落盘一条空 Summary 消息只会与上一轮回复重复展示（双重总结）。
        // 改为不落盘新消息，直接把上一轮 ReAct 的过程回复（phase=React）提升为最终回复。
        if !needs_more_work && summary_content.trim().is_empty() {
            if let Some(message_id) = promote_last_react_message_to_summary(session) {
                crate::react::message::emit_session_message_upsert(session, stream_tx, &message_id);
            }
            let compression_cancelled = maybe_update_context_summary(
                session,
                &self.engine,
                &usage,
                stream_tx,
                self.cancel_flag
                    .as_ref()
                    .expect("cancel_flag 必须在 execute_turn 前注入")
                    .clone(),
                self.shutdown_flag.clone(),
            )
            .await;
            if compression_cancelled {
                emit_cancelled(stream_tx);
                return SummaryPhaseResult::Cancelled(usage);
            }
            session.persist_to_disk();
            return SummaryPhaseResult::Completed(usage);
        }

        upsert_assistant_text_message(
            session,
            &pending_msg_id,
            &summary_content,
            &response.reasoning_content,
            crate::session::MessagePhase::Normal,
        );
        if let Some(message) = session
            .messages
            .iter_mut()
            .find(|message| message.id == pending_msg_id)
        {
            message.reasoning_signature = response.reasoning_signature;
            // 需要 more work 的总结视为过程性内容；Done/AskUser 视为最终回复。
            message.phase = if needs_more_work {
                crate::session::MessagePhase::React
            } else {
                crate::session::MessagePhase::Summary
            };
        }
        crate::react::message::emit_session_message_upsert(session, stream_tx, &pending_msg_id);
        let compression_cancelled = maybe_update_context_summary(
            session,
            &self.engine,
            &usage,
            stream_tx,
            self.cancel_flag
                .as_ref()
                .expect("cancel_flag 必须在 execute_turn 前注入")
                .clone(),
            self.shutdown_flag.clone(),
        )
        .await;
        if compression_cancelled {
            emit_cancelled(stream_tx);
            return SummaryPhaseResult::Cancelled(usage);
        }
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

/// 把 session 中最后一条 `phase=React` 的 assistant 消息提升为最终回复（`phase=Summary`）。
///
/// 用于总结阶段判定为「空正文 Done」时：LLM 认为上一轮 ReAct 已有完整可用的答复，
/// 无需新内容。此时直接复用上一轮回复作为最终回复，避免落盘空消息造成重复展示。
/// 找不到符合条件的消息时不做任何改动（兜底）。
fn promote_last_react_message_to_summary(session: &mut Session) -> Option<String> {
    for message in session.messages.iter_mut().rev() {
        if message.role == MessageRole::Assistant
            && message.phase == crate::session::MessagePhase::React
        {
            message.phase = crate::session::MessagePhase::Summary;
            return Some(message.id.clone());
        }
    }
    tracing::warn!("空正文 Done 但未找到可提升的 React 消息，保持现状");
    None
}

/// 总结阶段对任务完成度的判定结果。
///
/// 由 [`parse_summary_phase_output`] 从模型回复的标记解析得到。语义上：
/// - `Done`：任务完成（含普通最终回复与 `[DONE]`），本轮结束
/// - `AskUser`：需要用户提供信息（`[ASK_USER]`），视作本轮结束
/// - `NeedMoreWork`：未完成但可继续（`[NEED_MORE_WORK]`），重入 ReAct Loop
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SummaryDecision {
    Done(String),
    AskUser(String),
    NeedMoreWork(String),
}

impl SummaryDecision {
    /// 取判定附带的文本正文（去标记后的回复内容）。
    fn payload(&self) -> &str {
        match self {
            SummaryDecision::Done(s)
            | SummaryDecision::AskUser(s)
            | SummaryDecision::NeedMoreWork(s) => s,
        }
    }
}

/// 解析总结阶段回复，得到完成度判定。
///
/// - 首行 `[NEED_MORE_WORK]` → [`SummaryDecision::NeedMoreWork`]（去标记后的正文为下一步说明）
/// - 首行 `[ASK_USER]` → [`SummaryDecision::AskUser`]（去标记后的正文为向用户的提问）
/// - 首行 `[DONE]` → [`SummaryDecision::Done`]（去标记后的正文为最终回复）
/// - 无标记 → 视为完成，[`SummaryDecision::Done`]（原文）
fn parse_summary_phase_output(text: &str) -> SummaryDecision {
    let trimmed = text.trim();
    if let Some(rest) = strip_summary_marker(trimmed, "[NEED_MORE_WORK]") {
        return SummaryDecision::NeedMoreWork(rest.trim().to_string());
    }
    if let Some(rest) = strip_summary_marker(trimmed, "[ASK_USER]") {
        return SummaryDecision::AskUser(rest.trim().to_string());
    }
    if let Some(rest) = strip_summary_marker(trimmed, "[DONE]") {
        return SummaryDecision::Done(rest.trim().to_string());
    }
    SummaryDecision::Done(trimmed.to_string())
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

fn persist_partial_summary(
    session: &mut Session,
    stream_tx: &StdSender<StreamEvent>,
    message_id: &str,
    text: &str,
    reasoning: &str,
) {
    if text.trim().is_empty() && reasoning.trim().is_empty() {
        return;
    }
    upsert_assistant_text_message(
        session,
        message_id,
        text,
        reasoning,
        crate::session::MessagePhase::Summary,
    );
    if let Err(error) = session.try_persist_to_disk() {
        tracing::warn!(%error, "持久化部分总结响应失败");
    }
    crate::react::message::emit_session_message_upsert(session, stream_tx, message_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::react::test_support::{
        MockLlmServer, MockResponse, RecordedRuntimeCommand, approval, plugin_control,
        runtime_with_recorder,
    };

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn summary_stream_forwards_plugin_runtime_commands() {
        let server = MockLlmServer::start(MockResponse::Stall);
        let (engine, recorder) = runtime_with_recorder(server.base_url());
        let mut react = ReactEngine::new(engine, Vec::new(), 2, 1)
            .with_cancel_flag(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )))
            .with_shutdown_flag(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )));
        let mut session = Session::new("summary-stream-runtime-command");
        let (stream_tx, _stream_rx) = std::sync::mpsc::channel();
        let (command_tx, mut command_rx) = tokio_mpsc::unbounded_channel();

        let execution = tokio::spawn(async move {
            let result = react
                .run_summary_phase(&mut session, &stream_tx, &mut command_rx, 1)
                .await;
            assert!(matches!(result, SummaryPhaseResult::Cancelled(_)));
        });
        server.wait_until_connected().await;
        command_tx.send(plugin_control("cancel-child")).unwrap();
        command_tx.send(approval("child-approval", false)).unwrap();
        command_tx.send(Command::Cancel).unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(3), execution)
            .await
            .expect("总结流式执行未及时退出")
            .unwrap();
        assert_eq!(
            recorder.commands(),
            vec![
                RecordedRuntimeCommand::PluginControl {
                    plugin_id: "test-plugin".to_string(),
                    action: "cancel-child".to_string(),
                },
                RecordedRuntimeCommand::Approval {
                    request_id: "child-approval".to_string(),
                    approved: false,
                },
            ]
        );
    }

    #[test]
    fn done_marker_parses_to_done() {
        assert_eq!(
            parse_summary_phase_output("[DONE]\n已完成。变更包括 A 和 B。"),
            SummaryDecision::Done("已完成。变更包括 A 和 B。".to_string())
        );
    }

    #[test]
    fn done_marker_case_insensitive_and_strips_separators() {
        // 大小写不敏感；标记后可跟冒号/中文冒号/连字符/空白
        assert_eq!(
            parse_summary_phase_output("[done]：已完成。"),
            SummaryDecision::Done("已完成。".to_string())
        );
    }

    #[test]
    fn ask_user_marker_parses_to_ask_user() {
        assert_eq!(
            parse_summary_phase_output("[ASK_USER] 请提供 API 凭据。"),
            SummaryDecision::AskUser("请提供 API 凭据。".to_string())
        );
    }

    #[test]
    fn need_more_work_marker_parses_to_need_more_work() {
        assert_eq!(
            parse_summary_phase_output("[NEED_MORE_WORK] 还需运行测试并修复失败用例。"),
            SummaryDecision::NeedMoreWork("还需运行测试并修复失败用例。".to_string())
        );
    }

    #[test]
    fn no_marker_defaults_to_done() {
        // 无标记视为完成，保留原文。
        assert_eq!(
            parse_summary_phase_output("  任务已全部完成。  "),
            SummaryDecision::Done("任务已全部完成。".to_string())
        );
    }

    #[test]
    fn empty_text_defaults_to_done_empty() {
        assert_eq!(
            parse_summary_phase_output("   "),
            SummaryDecision::Done(String::new())
        );
    }

    #[test]
    fn payload_returns_inner_text() {
        assert_eq!(SummaryDecision::Done("a".into()).payload(), "a");
        assert_eq!(SummaryDecision::AskUser("b".into()).payload(), "b");
        assert_eq!(SummaryDecision::NeedMoreWork("c".into()).payload(), "c");
    }

    #[test]
    fn promote_last_react_message_promotes_the_last_react_assistant() {
        // 构造一个含多条消息的 session：用户消息 + React 过程回复 + 工具结果。
        let mut session = Session::new("test");
        session.append_message(MessageRole::User, "帮我创建定时任务");
        session.append_message_with_id(
            "m1".to_string(),
            MessageRole::Assistant,
            "已创建定时提醒：每天 9 点叫你起床。",
            String::new(),
        );
        // 把这条 assistant 消息标记为 React（模拟 ReAct 过程回复）
        if let Some(m) = session.messages.last_mut() {
            m.phase = MessagePhase::React;
        }

        promote_last_react_message_to_summary(&mut session);

        // 最后一条 assistant 消息应被提升为 Summary
        let promoted = session
            .messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::Assistant)
            .expect("应存在 assistant 消息");
        assert_eq!(promoted.phase, MessagePhase::Summary);
    }

    #[test]
    fn promote_last_react_message_promotes_only_the_latest() {
        // 多条 React 消息时，只提升最后一条。
        let mut session = Session::new("test");
        for i in 0..3 {
            session.append_message_with_id(
                format!("m{i}"),
                MessageRole::Assistant,
                format!("过程回复 {i}"),
                String::new(),
            );
            if let Some(m) = session.messages.last_mut() {
                m.phase = MessagePhase::React;
            }
        }

        promote_last_react_message_to_summary(&mut session);

        // 只有最后一条（"过程回复 2"）变为 Summary，其余仍为 React
        let react_count = session
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::Assistant && m.phase == MessagePhase::React)
            .count();
        let summary_count = session
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::Assistant && m.phase == MessagePhase::Summary)
            .count();
        assert_eq!(react_count, 2, "前两条应仍为 React");
        assert_eq!(summary_count, 1, "只有最后一条被提升为 Summary");
    }

    #[test]
    fn promote_last_react_message_noop_without_react_message() {
        // session 中没有 React assistant 消息时不做任何改动（兜底）。
        let mut session = Session::new("test");
        session.append_message(MessageRole::User, "你好");
        session.append_message_with_id(
            "m1".to_string(),
            MessageRole::Assistant,
            "你好，有什么可以帮你？",
            String::new(),
        );
        // 这条 assistant 是默认的 Normal，不是 React
        let before_phase = session
            .messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::Assistant)
            .map(|m| m.phase)
            .unwrap();

        promote_last_react_message_to_summary(&mut session);

        let after_phase = session
            .messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::Assistant)
            .map(|m| m.phase)
            .unwrap();
        assert_eq!(before_phase, after_phase, "无 React 消息时不应改动");
    }
}

/// 总结阶段的判断指令（作为运行时上下文注入，不常驻 system prompt）。
///
/// 总结阶段的核心职责是「完成度判断器 + 续作路由」，而非无条件重写最终答案：
/// 完成则结束、未完成且可继续则 NEED_MORE_WORK 回到 ReAct、需要用户输入则提问。
pub(super) const SUMMARY_PHASE_PROMPT: &str = "\
你当前处于总结阶段。你首先是一个「完成度判断器」，其次才是最终回复的作者。\n\
请先判断上一轮 ReAct 是否已经完成用户任务，再据此输出。\n\
\n\
判断与输出规则（请在回复首行用对应标记）：\n\
1. 若上一轮已经给出完整、可用的最终答复：只输出 [DONE]，不要带任何正文。\n\
   系统会自动复用上一轮回复作为最终回复，复述只会造成重复展示。\n\
2. 若任务已完成，但上一轮回复缺少面向用户的最终说明（例如只输出了工具结果、\n\
   没有总结性文字）：输出 [DONE]，并在新行给出最终回复正文。\n\
3. 若任务未完成、且你确实还能通过工具继续推进（例如只查了一半、改了没验证、\n\
   测试失败但可继续修、还有明确下一步）：输出 [NEED_MORE_WORK]，\n\
   然后简要说明还需要做什么。系统将重新进入工具执行阶段。\n\
4. 若需要用户提供信息（凭据、授权、选择、确认）才能继续：输出 [ASK_USER]，\n\
   然后提出问题。这视作本轮结束。\n\
\n\
输出原则：\n\
- 规则 1 是默认情况：只要上一轮已有可用的最终答复，就只输出 [DONE]，不要复述。\n\
- 仅当确实需要补充新的面向用户的说明时，才在标记后给出正文（规则 2/3/4）。\n\
- 本阶段不会执行任何工具调用。不要在回复中要求调用工具。\n\
- 如果只是给用户后续建议（而非确实还有未完成工作），不要使用 [NEED_MORE_WORK]。";

/// 构建总结阶段的 LLM 请求。
///
/// 将 `SUMMARY_PHASE_PROMPT` 作为运行时上下文追加到对话末尾。请求本身不携带 tools
/// 选择信息——是否禁用工具调用由调用方通过 `Some(ToolChoice::None)` 显式控制（见
/// `run_summary_phase` / `force_final_response`），本函数只负责构造消息上下文与
/// thinking 预算。
/// 构建总结阶段（完成度判断器）的 LLM 请求。
///
/// 将 `SUMMARY_PHASE_PROMPT` 作为运行时上下文追加到对话末尾。请求本身不携带 tools
/// 选择信息——是否禁用工具调用由调用方通过 `Some(ToolChoice::None)` 显式控制（见
/// `run_summary_phase` / `force_final_response`）。
pub(super) fn request_for_summary_phase(session: &Session) -> ModelRequest {
    build_text_finalization_request(
        session,
        &format!("<runtime_context>\n{SUMMARY_PHASE_PROMPT}\n</runtime_context>"),
    )
}

/// 构建一次「只产出文本最终回复」的 LLM 请求（共用请求体）。
///
/// 总结阶段（`run_summary_phase`）与强制终结（`force_final_response`）的请求体
/// 仅在 runtime_context 提示内容上不同——其余（thinking 预算、无 media、空 user_input）
/// 完全一致。本函数抽出共用部分，调用方负责提供 prompt 文本；传空串时跳过 push
/// （用于 force-final：提示已作为 Tool 消息进入 session 历史，无需在请求里重复）。
/// 工具调用禁用由调用方在发起 client 调用时通过 `Some(ToolChoice::None)` 控制。
fn build_text_finalization_request(session: &Session, prompt: &str) -> ModelRequest {
    let mut context = session.context();
    if !prompt.is_empty() {
        context.push(
            Message::new(MessageRole::System, prompt.to_string()).with_phase(MessagePhase::Normal),
        );
    }
    ModelRequest {
        session_title: session.title.clone(),
        user_input: String::new(),
        context,
        thinking: Some(crate::model::ThinkingConfig {
            budget_tokens: 4096,
        }),
        reasoning_effort: None,
        thinking_disabled: false,
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

impl ReactEngine {
    /// 超限时强制最终回复（兜底路径）。
    ///
    /// 触发场景：
    /// - `OuterLimit`：总结阶段判定 NeedMoreWork，但重入 Loop 次数已达上限
    /// - `SummaryError`：总结阶段 LLM 请求失败
    ///
    /// 与 `run_summary_phase`（正常路径）共同构成最终回复的唯一出口。
    ///
    /// 与正常总结阶段保持一致的 KV cache 策略：传入与 ReAct 阶段相同的 tools schema，
    /// 并通过 `ToolChoice::None` 禁止工具调用。兜底路径恰恰发生在长上下文、多轮 ReAct
    /// 之后——这正是 KV cache 最有价值的场景，若此处不传 tools 会在最需要 cache 前缀命中的阶段损失命中。
    ///
    /// 注意：force-final 的提示词以「强制终结」语义给出（不允许 [NEED_MORE_WORK]），
    /// 与 `run_summary_phase` 的「完成度判断器」语义相反——这是二者不能合并的根本原因：
    /// 否则 OuterLimit / SummaryError 会再次触发重入，造成无限循环。
    pub(super) async fn force_final_response(
        &self,
        session: &mut Session,
        stream_tx: &StdSender<StreamEvent>,
        reason: ForceFinalReason,
    ) -> bool {
        // 确保 system prompt 已构建
        if session.system_prompt_message.is_none() {
            rebuild_system_prompt(session, &self.engine);
        }
        // 将强制终结提示作为 Tool 消息持久化进 session（区别于 run_summary_phase 把
        // 提示放在请求 context 而不入 session）：兜底原因需在会话恢复后可见，便于诊断。
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
            elapsed_ms: None,
            turn_status: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: Some("force_final_response".to_string()),
            tool_result_is_error: false,
            compact: false,
            model_excluded: false,
            phase: MessagePhase::Normal,
            created_at: now_text(),
        });

        // force-final 的提示已作为 Tool 消息进入 session 历史（用于恢复后可见）；
        // 请求构造复用 build_text_finalization_request 的统一请求体（thinking 预算等），
        // 这里无需再向请求 context 追加 prompt——传空串时 build_text_finalization_request
        // 会跳过 push，避免空 system 消息污染上下文。
        let req = build_text_finalization_request(session, "");

        let pending_msg_id = scru128::new().to_string();

        let resp = match self
            .run_text_finalization_llm(session, &req, &pending_msg_id, stream_tx)
            .await
        {
            Some(r) => r,
            None => {
                crate::react::context::persist_error(
                    session,
                    "force_final_response 失败".to_string(),
                );
                return false;
            }
        };

        if self
            .cancel_flag
            .as_ref()
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
            || self
                .shutdown_flag
                .as_ref()
                .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
        {
            let _ = stream_tx.send(StreamEvent::Error {
                message: "已取消".to_string(),
            });
            return false;
        }

        if !self.commit_summary_message(
            session,
            stream_tx,
            &pending_msg_id,
            &resp,
            "force_final_response",
        ) {
            return false;
        }
        let _ = stream_tx.send(StreamEvent::Done {
            usage: Some(resp.usage.clone()),
        });
        true
    }

    /// 执行一次「只产出文本最终回复」的 LLM 调用（tools + `ToolChoice::None`，流式）。
    ///
    /// 总结阶段与强制终结共用：传相同 intent 过滤后的 tools schema 保持 KV cache 前缀，
    /// 通过 `ToolChoice::None` 禁止工具调用。`complete_with_functions_stream_with_tool_choice`
    /// 内部按 `use_stream_mode()` 自动选择流式/非流式实现，并在流式失败时回退非流式。
    ///
    /// 成功返回响应；失败则上报错误、持久化错误痕迹并返回 `None`（调用方据此终止）。
    async fn run_text_finalization_llm(
        &self,
        session: &Session,
        req: &ModelRequest,
        pending_msg_id: &str,
        stream_tx: &StdSender<StreamEvent>,
    ) -> Option<crate::model::ModelFunctionResponse> {
        let final_tools = tools_for_current_turn(&self.tools, session, "");
        let sink = ThrottledStreamSink::with_text_kind(
            pending_msg_id.to_string(),
            stream_tx.clone(),
            StreamTextKind::Summary,
        );
        let (chunk_tx, mut chunk_rx) = tokio_mpsc::unbounded_channel();
        let client = select_client_for_request(&self.engine, req).clone();
        let request = req.clone();
        let llm_fut = tokio::spawn(async move {
            client
                .stream_function_calls_with_tool_choice(
                    request,
                    final_tools,
                    Some(crate::model::ToolChoice::None),
                    chunk_tx,
                )
                .await
        });
        let cancel_flag = self
            .cancel_flag
            .as_ref()
            .expect("cancel_flag 必须在 execute_turn 前注入")
            .clone();
        let shutdown_flag = self.shutdown_flag.clone();
        let response_result = loop {
            tokio::select! {
                biased;
                _ = wait_for_abort_signal(cancel_flag.clone(), shutdown_flag.clone()) => {
                    llm_fut.abort();
                    let _ = llm_fut.await;
                    sink.finish();
                    let _ = stream_tx.send(StreamEvent::Error {
                        message: "已取消".to_string(),
                    });
                    return None;
                }
                chunk = chunk_rx.recv() => {
                    if let Some(chunk) = chunk {
                        sink.push_chunk(&chunk);
                    } else {
                        break match llm_fut.await {
                            Ok(result) => result,
                            Err(error) => Err(anyhow::anyhow!(error.to_string())),
                        };
                    }
                }
            }
        };
        sink.finish();
        match response_result {
            Ok(r) => Some(r),
            Err(err) => {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: err.to_string(),
                });
                None
            }
        }
    }

    /// 把一次「只产出文本最终回复」的 LLM 调用结果落盘为 Summary 消息。
    ///
    /// 共用于总结阶段的 Done/AskUser 分支与强制终结：append assistant 消息、
    /// 设置 phase=Summary 与 reasoning_signature、上报 token usage、持久化。
    fn commit_summary_message(
        &self,
        session: &mut Session,
        stream_tx: &StdSender<StreamEvent>,
        pending_msg_id: &str,
        resp: &crate::model::ModelFunctionResponse,
        usage_source: &str,
    ) -> bool {
        session.append_message_with_id(
            pending_msg_id.to_string(),
            MessageRole::Assistant,
            resp.text.clone(),
            resp.reasoning_content.clone(),
        );
        if let Some(message) = session.messages.last_mut() {
            message.phase = MessagePhase::Summary;
            message.reasoning_signature = resp.reasoning_signature.clone();
        }
        emit_token_usage(
            stream_tx,
            &resp.usage,
            Some(resp.usage.prompt_tokens.max(session.current_tokens)),
            self.engine.context_limit,
            usage_source,
            None,
        );
        if let Err(error) = session.try_persist_to_disk() {
            let _ = stream_tx.send(StreamEvent::Error {
                message: format!("持久化最终回复失败：{error}"),
            });
            return false;
        }
        crate::react::message::emit_session_message_upsert(session, stream_tx, pending_msg_id);
        true
    }
}
