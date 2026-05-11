//! ReactEngine: 单个 agent 的 async ReAct 循环
//!
//! 所有执行路径统一经过 `ReactEngine::execute_turn`，消除 sync/async 双版本。

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender as StdSender;

use tokio::sync::mpsc as tokio_mpsc;

use crate::agents::execution_mcp_agent::McpFunctionTarget;
use crate::app_state::formatting::{format_llm_output_message, format_tool_trace_message};
use crate::core::command::{Command, PendingCommandEffect};
use crate::model::{ModelRequest, TokenUsage, ToolSpec};
use crate::observe::{audit_permission_with_context, audit_tool_execution};
use crate::permission::{
    PermissionDecision, evaluate_tool_permission, format_call_args_summary, infer_audit_target,
    normalize_permission_target,
};
use crate::prompt::PromptAssembler;
use crate::react::context::{
    force_final_response, loop_context_with_memory, maybe_update_context_summary,
    select_client_for_request,
};
use crate::react::message::*;
use crate::runtime::LlmOutputRecord;
use crate::session::{Message, MessageRole, Session, now_text};
use crate::stream_throttle::ThrottledStreamSink;
use tiangong_types::{StreamEvent, StreamToolCall};

/// 单个 agent 的 async ReAct 执行引擎
pub(crate) struct ReactEngine {
    engine: crate::runtime::RuntimeEngine,
    tools: Vec<ToolSpec>,
    mcp_targets: HashMap<String, McpFunctionTarget>,
    max_rounds: usize,
}

impl ReactEngine {
    pub(crate) fn new(
        engine: crate::runtime::RuntimeEngine,
        tools: Vec<ToolSpec>,
        mcp_targets: HashMap<String, McpFunctionTarget>,
        max_rounds: usize,
    ) -> Self {
        Self {
            engine,
            tools,
            mcp_targets,
            max_rounds,
        }
    }

    /// 执行一个完整的对话轮次（可能多轮工具调用），async 版
    ///
    /// 每轮之间检查 cmd_rx：新消息注入上下文，cancel 立即生效。
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute_turn(
        &self,
        session: &mut Session,
        _user_input: &str,
        stream_tx: &StdSender<StreamEvent>,
        cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
        memory_handle: Option<&tiangong_memory::MemoryHandle>,
    ) -> TokenUsage {
        let mut loop_context: Vec<Message> = Vec::new();
        let mut round = 0;
        let mut accumulated_usage = TokenUsage::default();
        let mut pending_media_assets: Vec<tiangong_types::MediaAsset> = Vec::new();
        let mut memory_context: Option<String> = None;
        let mut memory_recall_attempted = false;
        let mut successful_tool_call_keys = HashSet::new();
        let mut memory_candidate_count = 0usize;

        'react_loop: loop {
            match drain_pending_commands_async(session, &mut loop_context, stream_tx, cmd_rx) {
                PendingCommandEffect::Terminate => return accumulated_usage,
                PendingCommandEffect::MessageInjected => {
                    memory_context = None;
                    memory_recall_attempted = false;
                    successful_tool_call_keys.clear();
                    memory_candidate_count = 0;
                }
                PendingCommandEffect::None => {}
            }

            if round >= self.max_rounds {
                force_final_response(session, &loop_context, &self.engine, stream_tx);
                break;
            }

            let request_tools = self.tools.to_vec();
            let loop_context_with_memory =
                loop_context_with_memory(&loop_context, memory_context.as_deref());
            let assembler = PromptAssembler::new(self.engine.context_limit);
            let assembled = assembler.assemble(
                session,
                "",
                request_tools.clone(),
                self.engine.models_config(),
                self.engine.agent_config(),
                &loop_context_with_memory,
            );

            let system_prompt = assembled.final_system_prompt();
            let req = ModelRequest {
                session_title: session.title.clone(),
                user_input: assembled.user_input.clone(),
                context: assembled.build_messages(),
                assembled_system_prompt: Some(system_prompt),
                thinking: Some(crate::model::ThinkingConfig {
                    budget_tokens: 4096,
                }),
                include_media: false,
            };

            let pending_msg_id = scru128::new().to_string();
            let sink = ThrottledStreamSink::new(pending_msg_id.clone(), stream_tx.clone());

            // async 流式调用 + select! 取消
            let (chunk_tx, mut chunk_rx) =
                tokio_mpsc::unbounded_channel::<crate::model::ModelStreamChunk>();
            let client = select_client_for_request(&self.engine, &req).clone();
            let req_clone = req.clone();
            let tools_clone = request_tools.clone();
            let llm_fut = tokio::task::spawn(async move {
                client
                    .stream_function_calls_with_tool_choice(req_clone, tools_clone, None, chunk_tx)
                    .await
            });

            let mut user_message_injected_during_stream = false;
            let response_result: anyhow::Result<crate::model::ModelFunctionResponse> = loop {
                tokio::select! {
                    biased;
                    cmd_opt = cmd_rx.recv() => {
                        match cmd_opt {
                            Some(Command::Cancel) | Some(Command::Shutdown) | None => {
                                llm_fut.abort();
                                sink.finish();
                                let _ = stream_tx.send(StreamEvent::Error {
                                    message: "已取消".into(),
                                });
                                return accumulated_usage;
                            }
                            Some(Command::Message { content, message_id, media }) => {
                                append_user_message_to_loop_context(
                                    session, &mut loop_context, stream_tx,
                                    content, message_id, media,
                                );
                                user_message_injected_during_stream = true;
                    memory_candidate_count = 0;
                            }
                            Some(Command::UpdateCwd { cwd }) => {
                                session.cwd = cwd;
                                crate::core::apply_session_cwd(session);
                            }
                            Some(Command::ReloadConfig) => {}
                            Some(Command::Approval { .. }) => {}
                        }
                    }
                    chunk_opt = chunk_rx.recv() => {
                        match chunk_opt {
                            Some(chunk) => sink.push_chunk(&chunk),
                            None => {
                                let response_result = match llm_fut.await {
                                    Ok(r) => r,
                                    Err(e) if e.is_cancelled() => {
                                        sink.finish();
                                        let _ = stream_tx.send(StreamEvent::Error {
                                            message: "已取消".into(),
                                        });
                                        return accumulated_usage;
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
                Ok(r) => r,
                Err(err) => {
                    let _ = stream_tx.send(StreamEvent::Error {
                        message: err.to_string(),
                    });
                    return accumulated_usage;
                }
            };

            accumulated_usage.accumulate(&response.usage);
            round += 1;

            if response.tool_calls.is_empty() {
                if is_synthetic_tool_call_placeholder(&response.text) {
                    continue;
                }

                session.append_message_with_id_and_media(
                    pending_msg_id,
                    MessageRole::Assistant,
                    response.text.clone(),
                    response.reasoning_content.clone(),
                    std::mem::take(&mut pending_media_assets),
                );
                if let Some(message) = session.messages.last_mut() {
                    message.reasoning_signature = response.reasoning_signature.clone();
                }
                let output = LlmOutputRecord {
                    stage: format!("react-round-{round}"),
                    content: String::new(),
                    reasoning_content: String::new(),
                    tool_calls: Vec::new(),
                    usage: response.usage.clone(),
                };
                append_runtime_tool_message(
                    session,
                    "llm_output",
                    format_llm_output_message(&output),
                );
                session.persist_to_disk();
                maybe_update_context_summary(session, &self.engine, response.usage.total_tokens);

                if user_message_injected_during_stream {
                    memory_context = None;
                    memory_recall_attempted = false;
                    successful_tool_call_keys.clear();
                    memory_candidate_count = 0;
                    continue 'react_loop;
                }

                let _ = stream_tx.send(StreamEvent::Done {
                    usage: Some(accumulated_usage.clone()),
                });
                return accumulated_usage;
            }

            // 工具调用
            let executable_calls = response.tool_calls.iter().collect::<Vec<_>>();
            if executable_calls.is_empty() {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: "模型没有返回可执行工具调用，任务已停止".to_string(),
                });
                return accumulated_usage;
            }
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
            let _ = stream_tx.send(StreamEvent::ToolCalls {
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

            // 执行工具
            for call in executable_calls {
                match drain_pending_commands_async(session, &mut loop_context, stream_tx, cmd_rx) {
                    PendingCommandEffect::Terminate => return accumulated_usage,
                    PendingCommandEffect::MessageInjected => {
                        memory_context = None;
                        memory_recall_attempted = false;
                        successful_tool_call_keys.clear();
                        session.persist_to_disk();
                        continue 'react_loop;
                    }
                    PendingCommandEffect::None => {}
                }

                let args_summary = format_call_args_summary(call);
                let (target_scope, target_summary) = infer_audit_target(call);
                let normalized_target = normalize_permission_target(
                    session,
                    target_scope.as_deref(),
                    target_summary.as_deref(),
                );
                let tool_call_key = tool_call_dedupe_key(&call.name, &call.arguments);
                if successful_tool_call_keys.contains(&tool_call_key) {
                    append_duplicate_tool_result(session, stream_tx, &call.id, &call.name);
                    continue;
                }

                let decision = evaluate_tool_permission(
                    &self.engine,
                    &call.name,
                    target_scope.as_deref(),
                    normalized_target.as_deref(),
                );
                let trust_mode = format!("{:?}", self.engine.permission_gate().trust_mode());
                match decision {
                    PermissionDecision::Approved => {
                        audit_permission_with_context(
                            &session.id,
                            &call.name,
                            "approved",
                            &trust_mode,
                            (!args_summary.is_empty()).then_some(args_summary.as_str()),
                            target_scope.as_deref(),
                            normalized_target.as_deref().or(target_summary.as_deref()),
                        );
                    }
                    PermissionDecision::Denied { reason } => {
                        audit_permission_with_context(
                            &session.id,
                            &call.name,
                            "denied",
                            &trust_mode,
                            (!args_summary.is_empty()).then_some(args_summary.as_str()),
                            target_scope.as_deref(),
                            normalized_target.as_deref().or(target_summary.as_deref()),
                        );
                        let _ = stream_tx.send(StreamEvent::ToolResult {
                            name: call.name.clone(),
                            tool_call_id: Some(call.id.clone()),
                            ok: false,
                            output: format!("权限拒绝：{reason}"),
                            full_output: None,
                        });
                        append_tool_result_message(
                            session,
                            &call.id,
                            &call.name,
                            format!("权限拒绝：{reason}"),
                            true,
                        );
                        continue;
                    }
                    PermissionDecision::NeedsApproval { request_id } => {
                        audit_permission_with_context(
                            &session.id,
                            &call.name,
                            "needs_approval",
                            &trust_mode,
                            (!args_summary.is_empty()).then_some(args_summary.as_str()),
                            target_scope.as_deref(),
                            normalized_target.as_deref().or(target_summary.as_deref()),
                        );
                        crate::approval_store::add_pending(
                            &session.id,
                            crate::session::PendingApproval {
                                request_id: request_id.clone(),
                                tool_name: call.name.clone(),
                                tool_args_summary: args_summary.clone(),
                                created_at: now_text(),
                            },
                        );
                        let _ = stream_tx.send(StreamEvent::ApprovalNeeded {
                            request_id: request_id.clone(),
                            tool_name: call.name.clone(),
                            args_summary: args_summary.clone(),
                        });

                        let approved = loop {
                            match cmd_rx.recv().await {
                                Some(Command::Approval {
                                    request_id: rid,
                                    approved,
                                }) if rid == request_id => {
                                    break approved;
                                }
                                Some(Command::Cancel) | Some(Command::Shutdown) | None => {
                                    let _ = stream_tx.send(StreamEvent::Error {
                                        message: "已取消".into(),
                                    });
                                    return accumulated_usage;
                                }
                                Some(Command::Message {
                                    content,
                                    message_id,
                                    media,
                                }) => {
                                    append_user_message_to_loop_context(
                                        session,
                                        &mut loop_context,
                                        stream_tx,
                                        content,
                                        message_id,
                                        media,
                                    );
                                    memory_candidate_count = 0;
                                }
                                Some(Command::UpdateCwd { cwd }) => {
                                    session.cwd = cwd;
                                    crate::core::apply_session_cwd(session);
                                }
                                Some(Command::ReloadConfig) => {}
                                Some(Command::Approval { .. }) => {}
                            }
                        };

                        crate::approval_store::remove_pending(&session.id, &request_id);

                        if !approved {
                            audit_tool_execution(
                                &session.id,
                                &call.name,
                                false,
                                (!args_summary.is_empty()).then_some(args_summary.as_str()),
                                target_scope.as_deref(),
                                normalized_target.as_deref().or(target_summary.as_deref()),
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
                                "用户拒绝执行".to_string(),
                                true,
                            );
                            session.persist_to_disk();
                            let _ = stream_tx.send(StreamEvent::ToolResult {
                                name: call.name.clone(),
                                tool_call_id: Some(call.id.clone()),
                                ok: false,
                                output: "用户拒绝执行".to_string(),
                                full_output: None,
                            });
                            let _ = stream_tx.send(StreamEvent::Done {
                                usage: Some(accumulated_usage.clone()),
                            });
                            return accumulated_usage;
                        }
                    }
                }

                if check_cancel(cmd_rx, stream_tx) {
                    return accumulated_usage;
                }

                let _ = stream_tx.send(StreamEvent::ToolStart {
                    name: call.name.clone(),
                    args_summary: args_summary.clone(),
                });

                let (result, memory_tool_usage, allow_memory_context) = {
                    let (mut result, memory_tool_usage, allow_memory_context) = if call.name
                        == "recall_memory"
                    {
                        if memory_recall_attempted {
                            crate::core::duplicate_memory_recall_tool_result()
                        } else {
                            memory_recall_attempted = true;
                            crate::core::execute_memory_recall_tool(call, memory_handle, session)
                                .await
                        }
                    } else if call.name == "analyze_attachment" {
                        (
                            crate::core::execute_attachment_analysis_tool(
                                call,
                                &self.engine,
                                session,
                            ),
                            tiangong_types::TokenUsage::default(),
                            false,
                        )
                    } else {
                        (
                            self.engine.execute_tool_call(
                                call,
                                &self.mcp_targets,
                                &self.engine.agent_config().mcp,
                            ),
                            tiangong_types::TokenUsage::default(),
                            false,
                        )
                    };
                    crate::memory::turn_result::localize_tool_result_images(
                        &call.name,
                        &mut result,
                    );
                    (result, memory_tool_usage, allow_memory_context)
                };
                accumulated_usage.accumulate(&memory_tool_usage);

                audit_tool_execution(
                    &session.id,
                    &call.name,
                    result.ok,
                    (!args_summary.is_empty()).then_some(args_summary.as_str()),
                    target_scope.as_deref(),
                    normalized_target.as_deref().or(target_summary.as_deref()),
                    &result.summary,
                );
                let _ = stream_tx.send(StreamEvent::ToolResult {
                    name: call.name.clone(),
                    tool_call_id: Some(call.id.clone()),
                    ok: result.ok,
                    output: tool_result_stream_output(&result),
                    full_output: Some(tool_result_full_output(&result)),
                });
                append_tool_result_message(
                    session,
                    &call.id,
                    &call.name,
                    tool_result_provider_text(&call.name, &result, allow_memory_context),
                    !result.ok,
                );
                append_runtime_tool_message(
                    session,
                    &call.name,
                    format_tool_trace_message(&result),
                );
                if !result.ok && check_cancel(cmd_rx, stream_tx) {
                    return accumulated_usage;
                }

                if result.ok {
                    successful_tool_call_keys.insert(tool_call_key);
                    pending_media_assets.extend(
                        crate::memory::turn_result::parse_media_assets_from_tool_result(
                            &call.name,
                            &result.stdout,
                            &result.summary,
                        ),
                    );
                }

                // 记忆候选评估
                if check_cancel(cmd_rx, stream_tx) {
                    return accumulated_usage;
                }
                if let Some(handle) = memory_handle {
                    let file_path =
                        if matches!(call.name.as_str(), "write_file" | "replace_in_file") {
                            call.arguments.as_object().and_then(|o| {
                                o.get("path").and_then(|v| v.as_str()).map(String::from)
                            })
                        } else {
                            None
                        };
                    if let Some(candidate) =
                        crate::memory::turn_result::evaluate_tool_result_for_memory(
                            &call.name,
                            result.ok,
                            &result.summary,
                            file_path.as_deref(),
                            memory_candidate_count,
                        )
                    {
                        handle.submit_memory_candidate(candidate);
                        memory_candidate_count += 1;
                    }
                }
                if call.name == "recall_memory"
                    && result.ok
                    && allow_memory_context
                    && !result.stdout.trim().is_empty()
                {
                    memory_context = Some(result.stdout.clone());
                }
                maybe_update_context_summary(session, &self.engine, response.usage.total_tokens);

                match drain_pending_commands_async(session, &mut loop_context, stream_tx, cmd_rx) {
                    PendingCommandEffect::Terminate => return accumulated_usage,
                    PendingCommandEffect::MessageInjected => {
                        memory_context = None;
                        memory_recall_attempted = false;
                        successful_tool_call_keys.clear();
                        memory_candidate_count = 0;
                        session.persist_to_disk();
                        continue 'react_loop;
                    }
                    PendingCommandEffect::None => {}
                }
            }

            session.persist_to_disk();
        }

        accumulated_usage
    }
}

fn drain_pending_commands_async(
    session: &mut Session,
    loop_context: &mut Vec<Message>,
    stream_tx: &StdSender<StreamEvent>,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
) -> PendingCommandEffect {
    let mut injected_message = false;

    while let Ok(cmd) = cmd_rx.try_recv() {
        match cmd {
            Command::Cancel => {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: "已取消".into(),
                });
                return PendingCommandEffect::Terminate;
            }
            Command::Shutdown => return PendingCommandEffect::Terminate,
            Command::Message {
                content,
                message_id,
                media,
            } => {
                append_user_message_to_loop_context(
                    session,
                    loop_context,
                    stream_tx,
                    content,
                    message_id,
                    media,
                );
                injected_message = true;
            }
            Command::UpdateCwd { cwd } => {
                session.cwd = cwd;
                crate::core::apply_session_cwd(session);
            }
            Command::ReloadConfig => {}
            Command::Approval { .. } => {}
        }
    }

    if injected_message {
        PendingCommandEffect::MessageInjected
    } else {
        PendingCommandEffect::None
    }
}

/// 非阻塞检查是否有取消或关闭命令待处理。
fn check_cancel(
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
    stream_tx: &StdSender<StreamEvent>,
) -> bool {
    while let Ok(cmd) = cmd_rx.try_recv() {
        match cmd {
            Command::Cancel | Command::Shutdown => {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: "已取消".into(),
                });
                return true;
            }
            _ => {}
        }
    }
    false
}
