//! ReactEngine: 单个 agent 的 async ReAct 循环
//!
//! 所有执行路径统一经过 `ReactEngine::execute_turn`，消除 sync/async 双版本。

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::mpsc::{Receiver as StdReceiver, Sender as StdSender};
use std::sync::{Arc, Mutex};

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

use crate::agent_team::lifecycle::TeamContext;

#[derive(Default)]
struct SubAgentDrainResult {
    usage: TokenUsage,
    ran: bool,
}

fn sub_agent_stream_message(
    id: impl Into<String>,
    role: MessageRole,
    content: impl Into<String>,
    reasoning_content: impl Into<String>,
) -> Message {
    Message {
        id: id.into(),
        role,
        content: content.into(),
        reasoning_content: reasoning_content.into(),
        reasoning_signature: None,
        worker_id: None,
        media: Vec::new(),
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: None,
        tool_result_is_error: false,
        compact: false,
        created_at: now_text(),
    }
}

fn send_sub_agent_output(
    parent_tx: &StdSender<StreamEvent>,
    agent_id: &str,
    agent_role: &str,
    agent_label: &str,
    message: Message,
) {
    let _ = parent_tx.send(StreamEvent::AgentOutput {
        agent_id: agent_id.to_string(),
        agent_role: agent_role.to_string(),
        agent_label: agent_label.to_string(),
        messages: vec![message],
    });
}

fn spawn_sub_agent_stream_forwarder(
    agent_id: String,
    agent_role: String,
    agent_label: String,
    parent_tx: StdSender<StreamEvent>,
    child_rx: StdReceiver<StreamEvent>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        for event in child_rx {
            match event {
                StreamEvent::UserMessage {
                    message_id,
                    content,
                    ..
                } => send_sub_agent_output(
                    &parent_tx,
                    &agent_id,
                    &agent_role,
                    &agent_label,
                    sub_agent_stream_message(
                        format!("agent:{agent_id}:user:{message_id}"),
                        MessageRole::User,
                        content,
                        "",
                    ),
                ),
                StreamEvent::Delta {
                    message_id,
                    content,
                } => send_sub_agent_output(
                    &parent_tx,
                    &agent_id,
                    &agent_role,
                    &agent_label,
                    sub_agent_stream_message(
                        format!("agent:{agent_id}:assistant:{message_id}"),
                        MessageRole::Assistant,
                        content,
                        "",
                    ),
                ),
                StreamEvent::Reasoning {
                    message_id,
                    content,
                } => send_sub_agent_output(
                    &parent_tx,
                    &agent_id,
                    &agent_role,
                    &agent_label,
                    sub_agent_stream_message(
                        format!("agent:{agent_id}:assistant:{message_id}"),
                        MessageRole::Assistant,
                        "",
                        content,
                    ),
                ),
                StreamEvent::ToolCalls {
                    message_id,
                    names,
                    usage,
                    ..
                } => {
                    let usage_text = usage
                        .map(|usage| {
                            format!(
                                "\ntokens: prompt={}, completion={}, total={}",
                                usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
                            )
                        })
                        .unwrap_or_default();
                    send_sub_agent_output(
                        &parent_tx,
                        &agent_id,
                        &agent_role,
                        &agent_label,
                        sub_agent_stream_message(
                            format!("agent:{agent_id}:tool-calls:{message_id}"),
                            MessageRole::System,
                            format!("LLM 输出{usage_text}\ntool_calls: {}", names.join(", ")),
                            "",
                        ),
                    );
                }
                StreamEvent::ToolStart { name, args_summary } => {
                    let mut content = format!("工具开始 [{name}]");
                    if !args_summary.is_empty() {
                        content.push_str(&format!("\n命令: {args_summary}"));
                    }
                    send_sub_agent_output(
                        &parent_tx,
                        &agent_id,
                        &agent_role,
                        &agent_label,
                        sub_agent_stream_message(
                            format!("agent:{agent_id}:tool-start:{}", scru128::new()),
                            MessageRole::System,
                            content,
                            "",
                        ),
                    );
                }
                StreamEvent::ToolResult {
                    name,
                    tool_call_id,
                    ok,
                    output,
                    full_output,
                } => {
                    let persisted_output = full_output.unwrap_or(output);
                    let mut content = format!(
                        "工具执行 [{name}]\nok={} exit_code={}",
                        ok,
                        if ok { 0 } else { 1 }
                    );
                    if !persisted_output.trim().is_empty() {
                        content.push_str(&format!("\nstdout:\n{persisted_output}"));
                    }
                    send_sub_agent_output(
                        &parent_tx,
                        &agent_id,
                        &agent_role,
                        &agent_label,
                        sub_agent_stream_message(
                            tool_call_id.unwrap_or_else(|| {
                                format!("agent:{agent_id}:tool-result:{}", scru128::new())
                            }),
                            MessageRole::System,
                            content,
                            "",
                        ),
                    );
                }
                StreamEvent::Retry {
                    message,
                    attempt,
                    max_attempts,
                } => send_sub_agent_output(
                    &parent_tx,
                    &agent_id,
                    &agent_role,
                    &agent_label,
                    sub_agent_stream_message(
                        format!("agent:{agent_id}:retry:{}", scru128::new()),
                        MessageRole::System,
                        format!("LLM 请求重试中（{attempt}/{max_attempts}）：{message}"),
                        "",
                    ),
                ),
                StreamEvent::ApprovalNeeded {
                    tool_name,
                    args_summary,
                    ..
                } => send_sub_agent_output(
                    &parent_tx,
                    &agent_id,
                    &agent_role,
                    &agent_label,
                    sub_agent_stream_message(
                        format!("agent:{agent_id}:approval:{}", scru128::new()),
                        MessageRole::System,
                        format!("等待确认：{tool_name} {args_summary}"),
                        "",
                    ),
                ),
                StreamEvent::AgentCreated { .. }
                | StreamEvent::AgentStatusChanged { .. }
                | StreamEvent::AgentNotification { .. }
                | StreamEvent::AgentMessage { .. }
                | StreamEvent::FileLockChanged { .. } => {
                    let _ = parent_tx.send(event);
                }
                StreamEvent::Error { message } => {
                    let _ = parent_tx.send(StreamEvent::AgentNotification {
                        agent_id: agent_id.clone(),
                        agent_label: agent_label.clone(),
                        content: format!("执行出错：{message}"),
                        level: "error".to_string(),
                    });
                    send_sub_agent_output(
                        &parent_tx,
                        &agent_id,
                        &agent_role,
                        &agent_label,
                        sub_agent_stream_message(
                            format!("agent:{agent_id}:error:{}", scru128::new()),
                            MessageRole::System,
                            format!("执行出错：{message}"),
                            "",
                        ),
                    );
                }
                _ => {}
            }
        }
    })
}

/// 单个 agent 的 async ReAct 执行引擎
pub(crate) struct ReactEngine {
    engine: crate::runtime::RuntimeEngine,
    tools: Vec<ToolSpec>,
    mcp_targets: HashMap<String, McpFunctionTarget>,
    max_rounds: usize,
    team: Option<Arc<Mutex<TeamContext>>>,
    agent_id: String,
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
            team: None,
            agent_id: "main".to_string(),
        }
    }

    /// 使用已有团队上下文执行指定 Agent。
    pub(crate) fn with_shared_team(
        mut self,
        team: Arc<Mutex<TeamContext>>,
        agent_id: String,
    ) -> Self {
        self.team = Some(team);
        self.agent_id = agent_id;
        self
    }

    /// 执行一个完整的对话轮次（可能多轮工具调用），async 版
    ///
    /// 每轮之间检查 cmd_rx：新消息注入上下文，cancel 立即生效。
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute_turn(
        &mut self,
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

        if self.agent_id == "main" {
            let routed = self
                .team
                .as_ref()
                .and_then(|team| {
                    team.lock().ok().map(|mut team| {
                        crate::agent_team::lifecycle::route_user_mentions(
                            &mut team,
                            _user_input,
                            stream_tx,
                        )
                    })
                })
                .unwrap_or(false);
            if routed {
                let sub_result = self.drain_sub_agent_inboxes(session, stream_tx).await;
                accumulated_usage.accumulate(&sub_result.usage);
                session.persist_to_disk();
                let _ = stream_tx.send(StreamEvent::Done {
                    usage: Some(accumulated_usage.clone()),
                });
                return accumulated_usage;
            }
        }

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

                // 团队协作工具拦截
                if crate::agent_team::lifecycle::is_team_tool(&call.name) {
                    let args_summary = format_call_args_summary(call);
                    let _ = stream_tx.send(StreamEvent::ToolStart {
                        name: call.name.clone(),
                        args_summary: args_summary.clone(),
                    });
                    let result = if let Some(team) = self.team.as_ref() {
                        if let Ok(mut team) = team.lock() {
                            crate::agent_team::lifecycle::execute_team_tool(
                                &mut team,
                                &self.agent_id,
                                call,
                                session,
                                &self.tools,
                                stream_tx,
                            )
                        } else {
                            crate::agent_team::lifecycle::error_tool_result(
                                &call.name,
                                "团队状态锁定失败",
                            )
                        }
                    } else {
                        crate::agent_team::lifecycle::error_tool_result(
                            &call.name,
                            "团队功能未启用",
                        )
                    };
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
                        tool_result_provider_text(&call.name, &result, false),
                        !result.ok,
                    );
                    if result.ok {
                        let tool_call_key = tool_call_dedupe_key(&call.name, &call.arguments);
                        successful_tool_call_keys.insert(tool_call_key);
                    }
                    continue;
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

                // 文件编辑锁检查
                if matches!(call.name.as_str(), "write_file" | "replace_in_file")
                    && let Some(team) = self.team.as_ref()
                {
                    let file_path = call
                        .arguments
                        .as_object()
                        .and_then(|o| o.get("path").and_then(|v| v.as_str()).map(String::from));
                    if let Some(ref path) = file_path {
                        let path_buf = std::path::PathBuf::from(path);
                        let now = chrono::Local::now().naive_local();
                        let lock_error = team
                            .lock()
                            .map_err(|_| "团队状态锁定失败".to_string())
                            .and_then(|mut team| {
                                team.file_locks
                                    .ensure_can_write(&path_buf, &self.agent_id, &now)
                            });
                        if let Err(message) = lock_error {
                            let _ = stream_tx.send(StreamEvent::ToolResult {
                                name: call.name.clone(),
                                tool_call_id: Some(call.id.clone()),
                                ok: false,
                                output: message.clone(),
                                full_output: None,
                            });
                            append_tool_result_message(
                                session, &call.id, &call.name, message, true,
                            );
                            continue;
                        }
                    }
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

            // 执行有待处理任务的 Sub Agent
            let sub_result = self.drain_sub_agent_inboxes(session, stream_tx).await;
            accumulated_usage.accumulate(&sub_result.usage);

            let main_messages = self.drain_main_agent_messages();
            if !main_messages.is_empty() {
                for message in main_messages {
                    loop_context.push(Message::new(
                        MessageRole::User,
                        format!(
                            "[from:{} at {}]\n{}",
                            message.from, message.created_at, message.content
                        ),
                    ));
                }
                continue 'react_loop;
            }

            if sub_result.ran {
                session.persist_to_disk();
                let _ = stream_tx.send(StreamEvent::Done {
                    usage: Some(accumulated_usage.clone()),
                });
                return accumulated_usage;
            }

            session.persist_to_disk();
        }

        accumulated_usage
    }

    fn drain_main_agent_messages(&self) -> Vec<crate::agent_team::message_bus::AgentMessage> {
        self.team
            .as_ref()
            .and_then(|team| team.lock().ok().map(|mut team| team.drain_main_inbox()))
            .unwrap_or_default()
    }

    /// 轮询所有活跃 Sub Agent 的收件箱，为有待处理消息的 Agent 执行 ReactEngine。
    /// 返回 Sub Agent 执行消耗的 token 总量。
    async fn drain_sub_agent_inboxes(
        &mut self,
        parent_session: &mut Session,
        stream_tx: &StdSender<StreamEvent>,
    ) -> SubAgentDrainResult {
        let mut result = SubAgentDrainResult::default();

        let Some(team_arc) = self.team.clone() else {
            return result;
        };

        let max_concurrent = crate::agent_team::tools::MAX_CONCURRENT_SUB_AGENTS;
        let token_budget = crate::agent_team::tools::SUB_AGENT_TOTAL_TOKEN_BUDGET;
        let sub_max_rounds = crate::agent_team::tools::SUB_AGENT_MAX_ROUNDS;

        // 收集待执行的 Agent（有消息且未超限）
        type PendingAgent = (String, String, String, String, Vec<String>, String, Session);
        let mut pending: Vec<PendingAgent> = Vec::new();
        {
            let Ok(mut team) = team_arc.lock() else {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: "团队状态锁定失败".to_string(),
                });
                return result;
            };
            let agent_infos: Vec<_> = team
                .registry
                .alive_agents()
                .iter()
                .filter(|a| a.status == crate::agent_team::descriptor::AgentStatus::Idle)
                .map(|a| {
                    (
                        a.agent_id.clone(),
                        a.label.clone(),
                        a.role.clone(),
                        a.system_prompt.clone(),
                        a.tools.clone(),
                    )
                })
                .collect();

            for (agent_id, agent_label, agent_role, system_prompt, tool_names) in agent_infos {
                if pending.len() >= max_concurrent {
                    break;
                }
                if result.usage.total_tokens >= token_budget {
                    let _ = stream_tx.send(StreamEvent::AgentNotification {
                        agent_id: "system".to_string(),
                        agent_label: "系统".to_string(),
                        content: format!(
                            "Sub Agent token 预算已用尽（{}/{}），剩余 Agent 将在下一轮执行",
                            result.usage.total_tokens, token_budget
                        ),
                        level: "warning".to_string(),
                    });
                    break;
                }
                let messages = team.registry.drain_inbox(&agent_id);
                if messages.is_empty() {
                    continue;
                }
                let Some(child_session) = team.registry.get_session(&agent_id).cloned() else {
                    continue;
                };
                team.registry.update_status(
                    &agent_id,
                    crate::agent_team::descriptor::AgentStatus::Running,
                );
                let _ = stream_tx.send(StreamEvent::AgentStatusChanged {
                    agent_id: agent_id.clone(),
                    label: agent_label.clone(),
                    status: "running".to_string(),
                });
                let combined = messages
                    .into_iter()
                    .map(|m| format!("[from:{} at {}]\n{}", m.from, m.created_at, m.content))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                pending.push((
                    agent_id,
                    agent_label,
                    agent_role,
                    system_prompt,
                    tool_names,
                    combined,
                    child_session,
                ));
            }
        }

        if pending.is_empty() {
            return result;
        }
        result.ran = true;

        // 并行执行所有待执行的 Sub Agent（协作并发）
        type SubResult = (String, String, String, Session, Vec<Message>, TokenUsage);
        let mut futures: Vec<Pin<Box<dyn Future<Output = SubResult>>>> = Vec::new();

        for (
            agent_id,
            agent_label,
            agent_role,
            system_prompt,
            tool_names,
            combined_content,
            mut child_session,
        ) in pending
        {
            let sub_tools: Vec<ToolSpec> = self
                .tools
                .iter()
                .filter(|t| tool_names.iter().any(|name| name == &t.name))
                .filter(|t| !matches!(t.name.as_str(), "create_agent" | "dismiss_agent"))
                .cloned()
                .collect();

            let mut sub_engine = ReactEngine::new(
                self.engine.clone(),
                sub_tools,
                self.mcp_targets.clone(),
                sub_max_rounds,
            )
            .with_shared_team(team_arc.clone(), agent_id.clone());

            let (sub_cmd_tx, mut sub_cmd_rx) = tokio_mpsc::unbounded_channel();
            let _ = sub_cmd_tx.send(Command::Message {
                content: combined_content,
                message_id: Some(scru128::new().to_string()),
                media: Vec::new(),
            });

            let stream_tx_clone = stream_tx.clone();
            let id = agent_id;
            let label = agent_label;
            let role = agent_role;
            let prompt = system_prompt;
            let start_message_len = child_session.messages.len();

            let fut = Box::pin(async move {
                let (child_stream_tx, child_stream_rx) = std::sync::mpsc::channel();
                let forwarder = spawn_sub_agent_stream_forwarder(
                    id.clone(),
                    role.clone(),
                    label.clone(),
                    stream_tx_clone,
                    child_stream_rx,
                );
                let _keep_sub_cmd_tx_alive = sub_cmd_tx;
                let usage = sub_engine
                    .execute_turn(
                        &mut child_session,
                        &prompt,
                        &child_stream_tx,
                        &mut sub_cmd_rx,
                        None,
                    )
                    .await;
                drop(child_stream_tx);
                let _ = forwarder.join();
                let new_messages = child_session
                    .messages
                    .iter()
                    .skip(start_message_len)
                    .cloned()
                    .collect::<Vec<_>>();
                (id, label, role, child_session, new_messages, usage)
            });
            futures.push(fut);
        }

        // 并发执行所有 Sub Agent
        let results = futures_util::future::join_all(futures).await;

        // 处理结果
        for (agent_id, agent_label, _agent_role, child_session, _output_messages, usage) in results
        {
            // 检查 Sub Agent 是否有错误输出
            let has_error = child_session
                .messages
                .iter()
                .any(|m| m.role == MessageRole::System && m.content.starts_with("[错误]"));

            let status = if has_error { "error" } else { "idle" };
            let _ = stream_tx.send(StreamEvent::AgentStatusChanged {
                agent_id: agent_id.clone(),
                label: agent_label.clone(),
                status: status.to_string(),
            });

            if has_error {
                let error_content = child_session
                    .messages
                    .iter()
                    .filter(|m| m.role == MessageRole::System && m.content.starts_with("[错误]"))
                    .map(|m| m.content.replace("[错误] ", ""))
                    .collect::<Vec<_>>()
                    .join("; ");
                append_runtime_tool_message(
                    parent_session,
                    &format!("sub_agent_{agent_label}"),
                    format!("[{agent_label}] 执行出错：{error_content}"),
                );
            } else {
                let summary = child_session
                    .messages
                    .iter()
                    .filter(|m| m.role == MessageRole::Assistant)
                    .filter_map(|m| {
                        let c = m.content.trim().to_string();
                        if c.is_empty() { None } else { Some(c) }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                if !summary.is_empty() {
                    let brief = if summary.chars().count() > 500 {
                        format!("{}...", summary.chars().take(500).collect::<String>())
                    } else {
                        summary
                    };
                    append_runtime_tool_message(
                        parent_session,
                        &format!("sub_agent_{agent_label}"),
                        format!("[{agent_label}] 执行完成\n{brief}"),
                    );
                }
            }

            result.usage.accumulate(&usage);

            // 临时 Agent 执行完毕后自动销毁
            let Ok(mut team) = team_arc.lock() else {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: "团队状态锁定失败".to_string(),
                });
                continue;
            };
            let should_dismiss = team
                .registry
                .get(&agent_id)
                .map(|d| d.lifecycle == crate::agent_team::descriptor::AgentLifecycle::Temporary)
                .unwrap_or(false);
            if should_dismiss {
                for path in team.file_locks.release_all(&agent_id) {
                    let _ = stream_tx.send(StreamEvent::FileLockChanged {
                        path,
                        holder_agent_id: Some(agent_id.clone()),
                        holder_agent_label: Some(agent_label.clone()),
                        action: "unlocked".to_string(),
                    });
                }
                team.registry.unregister(&agent_id);
                let _ = stream_tx.send(StreamEvent::AgentStatusChanged {
                    agent_id,
                    label: agent_label.clone(),
                    status: "terminated".to_string(),
                });
                append_runtime_tool_message(
                    parent_session,
                    &format!("sub_agent_{agent_label}"),
                    format!("[{agent_label}] 临时 Agent 已自动销毁"),
                );
            } else {
                team.registry.set_session(&agent_id, child_session);
                team.registry
                    .update_status(&agent_id, crate::agent_team::descriptor::AgentStatus::Idle);
            }
        }

        result
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
