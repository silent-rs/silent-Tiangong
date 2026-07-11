//! Sub Agent（多代理团队协作）桥接层。
//!
//! 本模块从 `engine.rs` 拆出，承载 ReAct 引擎中所有与 `TeamContext` 相关的职责：
//! - 子 Agent 的流式事件转发（把子 Agent 的 `StreamEvent` 翻译为父级
//!   `AgentOutput` 事件）
//! - 待执行子 Agent 的派发（`spawn_ready_sub_agents`）
//! - 活跃子 Agent 的收件箱轮询与结果回收（`drain_sub_agent_inboxes`）
//!
//! 这些方法仍挂在 `ReactEngine` 上（跨文件 `impl` 块），但逻辑上彼此聚合、
//! 与主 ReAct 状态机解耦，便于后续按事件接口进一步解耦（见重构计划）。

use std::future::Future;
use std::pin::Pin;
use std::sync::mpsc::{Receiver as StdReceiver, Sender as StdSender};
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use tokio::sync::mpsc as tokio_mpsc;

use crate::agent_team::lifecycle::TeamContext;
use crate::core::command::Command;
use crate::model::TokenUsage;
use crate::react::message::{append_or_reuse_user_message, append_runtime_tool_message};
use crate::session::{Message, MessageRole, Session, now_text};
use tiangong_types::StreamEvent;

use super::engine::ReactEngine;

/// 子 Agent 收件箱轮询结果。
#[derive(Default)]
pub(super) struct SubAgentDrainResult {
    pub usage: TokenUsage,
    pub ran: bool,
    pub cancelled: bool,
}

/// 子 Agent 执行 future 的输出：
/// `(agent_id, agent_label, agent_role, child_session, new_messages, usage)`。
pub(super) type SubResult = (String, String, String, Session, Vec<Message>, TokenUsage);
pub(super) type SubAgentFuture = Pin<Box<dyn Future<Output = SubResult>>>;
pub(super) type ActiveSubAgent = (String, String, String, tokio_mpsc::UnboundedSender<Command>);

/// 构造一条用于子 Agent 流转发的 `Message`。
fn sub_agent_stream_message(
    id: impl Into<String>,
    role: MessageRole,
    content: impl Into<String>,
    reasoning_content: impl Into<String>,
) -> Message {
    Message {
        id: id.into(),
        role,
        content: vec![crate::session::ContentBlock::text(content.into())],
        reasoning_content: reasoning_content.into(),
        reasoning_signature: None,
        worker_id: None,
        elapsed_ms: None,
        turn_status: None,
        tool_calls: Vec::new(),
        tool_call_id: None,
        tool_name: None,
        tool_result_is_error: false,
        compact: false,
        phase: crate::session::MessagePhase::Normal,
        created_at: now_text(),
    }
}

/// 把一条 `Message` 包装为 `StreamEvent::AgentOutput` 推送给父级。
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

/// 启动一个独立线程，把子 Agent 的内部 `StreamEvent` 流转发、翻译为父级事件。
///
/// 子 Agent 自身运行在独立的 stream channel 上；此转发器负责把它的各种细粒度
/// 事件（Delta/Reasoning/ToolCalls/...）收敛为对父级有意义的 `AgentOutput`，
/// 并透传团队级事件（AgentCreated/StatusChanged/...）。
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
                }
                | StreamEvent::ReactText {
                    message_id,
                    content,
                }
                | StreamEvent::SummaryText {
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
                    ..
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
                | StreamEvent::FileLockChanged { .. }
                | StreamEvent::PhaseChanged { .. } => {
                    let _ = parent_tx.send(event);
                }
                StreamEvent::TokenUsage {
                    usage,
                    current_tokens,
                    compression_threshold_tokens,
                    context_limit_tokens,
                    source,
                    ..
                } => {
                    let _ = parent_tx.send(StreamEvent::TokenUsage {
                        usage,
                        current_tokens,
                        compression_threshold_tokens,
                        context_limit_tokens,
                        source,
                        agent_id: Some(agent_id.clone()),
                    });
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

/// 渲染当前团队花名册（仅存活 Agent），用于子 Agent system prompt 上下文。
fn format_team_roster(team_arc: &Arc<Mutex<TeamContext>>) -> String {
    let Ok(team) = team_arc.lock() else {
        return String::new();
    };
    let mut agents = team.registry.alive_agents();
    agents.sort_by(|a, b| a.role.cmp(&b.role));
    agents
        .iter()
        .map(|a| format!("- {} (@{})", a.label, a.role))
        .collect::<Vec<_>>()
        .join("\n")
}

impl ReactEngine {
    /// 派发所有待执行（Idle 且有待处理消息）的子 Agent。
    ///
    /// 受并发上限与 token 预算约束；派发后会把对应 Agent 置为 Running，
    /// 并把其 `execute_turn` 作为 future 压入 `futures`。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn spawn_ready_sub_agents(
        &self,
        team_arc: &Arc<Mutex<TeamContext>>,
        stream_tx: &StdSender<StreamEvent>,
        futures: &mut FuturesUnordered<SubAgentFuture>,
        active_sub_agents: &mut Vec<ActiveSubAgent>,
        used_tokens: usize,
        max_concurrent: usize,
        token_budget: usize,
        sub_max_rounds: usize,
    ) -> bool {
        let remaining_slots = max_concurrent.saturating_sub(active_sub_agents.len());
        if remaining_slots == 0 {
            return false;
        }
        if used_tokens >= token_budget {
            let _ = stream_tx.send(StreamEvent::AgentNotification {
                agent_id: "system".to_string(),
                agent_label: "系统".to_string(),
                content: format!(
                    "Sub Agent token 预算已用尽（{used_tokens}/{token_budget}），剩余 Agent 将在下一轮执行"
                ),
                level: "warning".to_string(),
            });
            return false;
        }

        type PendingAgent = (String, String, String, String, Vec<String>, String, Session);
        let mut pending: Vec<PendingAgent> = Vec::new();
        {
            let Ok(mut team) = team_arc.lock() else {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: "团队状态锁定失败".to_string(),
                });
                return false;
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
                if pending.len() >= remaining_slots {
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
            return false;
        }

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
            let sub_tools: Vec<crate::model::ToolSpec> = self
                .tools
                .iter()
                .filter(|t| tool_names.iter().any(|name| name == &t.name))
                .filter(|t| !matches!(t.name.as_str(), "create_agent" | "dismiss_agent"))
                .cloned()
                .collect();

            let mut sub_engine = ReactEngine::new(
                self.engine.clone(),
                sub_tools,
                sub_max_rounds,
                crate::agent_team::tools::SUB_AGENT_MAX_OUTER_ITERATIONS,
            )
            .with_shared_team(team_arc.clone(), agent_id.clone());

            // 通过 SubAgentPromptContext 构建 system prompt
            let base_config = crate::prompt::SystemPromptConfig::from_configs(
                self.engine.models_config(),
                self.engine.agent_config(),
            );
            let team_roster = format_team_roster(team_arc);
            let ctx = crate::prompt::SubAgentPromptContext::new(
                &base_config,
                &system_prompt,
                &team_roster,
            );
            child_session.system_prompt_message = Some(ctx.build(&child_session));
            // 子 agent 经 ReactEngine::new(self.engine.clone()) 共享父 engine 的
            // RuntimeEngine，自动继承其 tool_overrides（含 memory/index 等插件注册的
            // handler），故子 agent 的 recall_memory / index_search 能直接路由到同一插件实例。

            let (sub_cmd_tx, mut sub_cmd_rx) = tokio_mpsc::unbounded_channel();
            // 子 agent 的 check_cancel 需要 cmd_tx 回灌非控制命令，注入子通道发送端。
            sub_engine = sub_engine.with_cmd_tx(sub_cmd_tx.clone());
            let _ = sub_cmd_tx.send(Command::Message {
                content: combined_content,
                message_id: Some(scru128::new().to_string()),
                media: Vec::new(),
            });
            if let Ok(mut team) = team_arc.lock() {
                team.register_active_agent(agent_id.clone(), sub_cmd_tx.clone());
            }
            active_sub_agents.push((
                agent_id.clone(),
                agent_role.clone(),
                agent_label.clone(),
                sub_cmd_tx.clone(),
            ));

            let stream_tx_clone = stream_tx.clone();
            let id = agent_id;
            let label = agent_label;
            let role = agent_role;
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
                    .execute_turn(&mut child_session, "", &child_stream_tx, &mut sub_cmd_rx)
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

        true
    }

    /// 轮询所有活跃 Sub Agent 的收件箱，为有待处理消息的 Agent 执行 ReactEngine。
    /// 返回 Sub Agent 执行消耗的 token 总量。
    pub(super) async fn drain_sub_agent_inboxes(
        &mut self,
        parent_session: &mut Session,
        stream_tx: &StdSender<StreamEvent>,
        cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
    ) -> SubAgentDrainResult {
        let mut result = SubAgentDrainResult::default();

        let Some(team_arc) = self.team.clone() else {
            return result;
        };

        let max_concurrent = crate::agent_team::tools::MAX_CONCURRENT_SUB_AGENTS;
        let token_budget = crate::agent_team::tools::SUB_AGENT_TOTAL_TOKEN_BUDGET;
        let sub_max_rounds = crate::agent_team::tools::SUB_AGENT_MAX_TOOL_ROUNDS;

        let (dispatch_wake_tx, mut dispatch_wake_rx) = tokio_mpsc::unbounded_channel();
        {
            let Ok(mut team) = team_arc.lock() else {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: "团队状态锁定失败".to_string(),
                });
                return result;
            };
            team.set_dispatch_waker(dispatch_wake_tx);
        }

        // 并行执行所有待执行的 Sub Agent（协作并发）
        let mut futures: FuturesUnordered<SubAgentFuture> = FuturesUnordered::new();
        let mut active_sub_agents: Vec<ActiveSubAgent> = Vec::new();
        if self.spawn_ready_sub_agents(
            &team_arc,
            stream_tx,
            &mut futures,
            &mut active_sub_agents,
            result.usage.total_tokens,
            max_concurrent,
            token_budget,
            sub_max_rounds,
        ) {
            result.ran = true;
        }

        if futures.is_empty() {
            if let Ok(mut team) = team_arc.lock() {
                team.clear_dispatch_waker();
            }
            return result;
        }

        let mut results = Vec::new();
        while !futures.is_empty() {
            tokio::select! {
                maybe_result = futures.next() => {
                    if let Some(sub_result) = maybe_result {
                        active_sub_agents.retain(|(agent_id, _, _, _)| agent_id != &sub_result.0);
                        if let Ok(mut team) = team_arc.lock() {
                            team.unregister_active_agent(&sub_result.0);
                        }
                        results.push(sub_result);
                        if self.spawn_ready_sub_agents(
                            &team_arc,
                            stream_tx,
                            &mut futures,
                            &mut active_sub_agents,
                            result.usage.total_tokens,
                            max_concurrent,
                            token_budget,
                            sub_max_rounds,
                        ) {
                            result.ran = true;
                        }
                    }
                }
                maybe_wake = dispatch_wake_rx.recv() => {
                    if maybe_wake.is_some()
                        && self.spawn_ready_sub_agents(
                            &team_arc,
                            stream_tx,
                            &mut futures,
                            &mut active_sub_agents,
                            result.usage.total_tokens,
                            max_concurrent,
                            token_budget,
                            sub_max_rounds,
                        )
                    {
                        result.ran = true;
                    }
                }
                maybe_cmd = cmd_rx.recv() => {
                    match maybe_cmd {
                        Some(Command::Cancel) | Some(Command::Shutdown) => {
                            result.cancelled = true;
                            let _ = stream_tx.send(StreamEvent::Error {
                                message: "已取消所有 Agent".to_string(),
                            });
                            for (_, _, _, tx) in &active_sub_agents {
                                let _ = tx.send(Command::Cancel);
                            }
                        }
                        Some(Command::CancelAgent { role }) => {
                            let mut matched = false;
                            for (agent_id, agent_role, agent_label, tx) in &active_sub_agents {
                                if agent_role == &role {
                                    matched = true;
                                    let _ = tx.send(Command::Cancel);
                                    let _ = stream_tx.send(StreamEvent::AgentStatusChanged {
                                        agent_id: agent_id.clone(),
                                        label: agent_label.clone(),
                                        status: "idle".to_string(),
                                    });
                                    let _ = stream_tx.send(StreamEvent::AgentNotification {
                                        agent_id: agent_id.clone(),
                                        agent_label: agent_label.clone(),
                                        content: "已请求停止当前执行".to_string(),
                                        level: "warning".to_string(),
                                    });
                                }
                            }
                            if !matched {
                                let _ = stream_tx.send(StreamEvent::AgentNotification {
                                    agent_id: role.clone(),
                                    agent_label: role,
                                    content: "未找到正在执行的 Agent".to_string(),
                                    level: "warning".to_string(),
                                });
                            }
                        }
                        Some(Command::Message { content, message_id, media }) => {
                            let message_id = append_or_reuse_user_message(
                                parent_session,
                                &content,
                                message_id,
                                media,
                            );
                            let media = parent_session
                                .messages
                                .iter()
                                .find(|message| message.id == message_id)
                                .map(|message| message.extract_media_assets())
                                .unwrap_or_default();
                            let _ = stream_tx.send(StreamEvent::UserMessage {
                                message_id,
                                content: content.clone(),
                                media: media.clone(),
                            });
                            if let Ok(mut team) = team_arc.lock() {
                                let _ = crate::agent_team::lifecycle::route_user_mentions_with_media(
                                    &mut team,
                                    &content,
                                    media,
                                    stream_tx,
                                );
                            }
                        }
                        Some(Command::UpdateCwd { cwd }) => {
                            parent_session.cwd = cwd;
                            crate::core::apply_session_cwd(parent_session);
                        }
                        Some(Command::CompressContext) => {
                            crate::core::compress_context_for_session(
                                parent_session,
                                &self.engine,
                                stream_tx,
                            );
                        }
                        Some(Command::ResetContext) => {
                            crate::core::reset_context_for_session(parent_session, stream_tx, &self.engine);
                        }
                        Some(Command::ReloadConfig) | Some(Command::Approval { .. }) => {}
                        Some(Command::InjectTool { tool_name, payload }) => {
                            crate::react::message::inject_tool_to_session(
                                parent_session,
                                stream_tx,
                                &tool_name,
                                &payload,
                            );
                        }
                        Some(Command::EmitStreamEvent(ev)) => {
                            let _ = stream_tx.send(ev);
                        }
                        None => break,
                    }
                }
            }
        }

        if let Ok(mut team) = team_arc.lock() {
            team.clear_dispatch_waker();
        }

        // 处理结果
        for (agent_id, agent_label, _agent_role, child_session, _output_messages, usage) in results
        {
            // 检查 Sub Agent 是否有错误输出。
            // 错误由 persist_error 经 inject_tool_to_messages 注入（plugin_injection
            // 消息对，渲染文本含 "数据来源：react_loop_error"）。
            let is_error_message = |m: &Message| {
                m.tool_name.as_deref() == Some(crate::react::message::INJECTION_TOOL_NAME)
                    && m.text_content().contains("数据来源：react_loop_error")
            };
            let has_error = child_session.messages.iter().any(is_error_message);

            let completion_content = if has_error {
                let error_content = child_session
                    .messages
                    .iter()
                    .filter(|m: &&Message| is_error_message(m))
                    .map(|m| m.text_content())
                    .collect::<Vec<_>>()
                    .join("; ");
                format!("[{agent_label}] 执行出错：{error_content}")
            } else {
                // 优先收集总结阶段（Summary phase）的最终回复作为汇报内容；
                // 若无（旧架构或快速路径未标记），回退到所有 Assistant 消息。
                let summary = {
                    let summaries: Vec<String> = child_session
                        .messages
                        .iter()
                        .filter(|m| {
                            m.role == MessageRole::Assistant
                                && m.phase == crate::session::MessagePhase::Summary
                        })
                        .filter_map(|m| {
                            let c = m.text_content().trim().to_string();
                            if c.is_empty() { None } else { Some(c) }
                        })
                        .collect();
                    if summaries.is_empty() {
                        child_session
                            .messages
                            .iter()
                            .filter(|m| m.role == MessageRole::Assistant)
                            .filter_map(|m| {
                                let c = m.text_content().trim().to_string();
                                if c.is_empty() { None } else { Some(c) }
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    } else {
                        summaries.join("\n")
                    }
                };

                if summary.is_empty() {
                    format!("[{agent_label}] 已完成本轮工作，但没有生成文本输出。")
                } else {
                    let brief = if summary.chars().count() > 500 {
                        format!("{}...", summary.chars().take(500).collect::<String>())
                    } else {
                        summary
                    };
                    format!("[{agent_label}] 执行完成\n{brief}")
                }
            };

            result.usage.accumulate(&usage);

            // 临时 Agent 执行完毕后自动销毁
            let Ok(mut team) = team_arc.lock() else {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: "团队状态锁定失败".to_string(),
                });
                continue;
            };
            team.deliver_main_message(crate::agent_team::message_bus::AgentMessage {
                id: scru128::new().to_string(),
                from: agent_id.clone(),
                to: "main".to_string(),
                content: completion_content.clone(),
                priority: crate::agent_team::message_bus::MessagePriority::Normal,
                created_at: now_text(),
            });
            let _ = stream_tx.send(StreamEvent::AgentMessage {
                from_agent_id: agent_id.clone(),
                from_agent_label: agent_label.clone(),
                to_agent_id: "main".to_string(),
                to_agent_label: "Main Agent".to_string(),
                content: completion_content.clone(),
            });
            append_runtime_tool_message(
                parent_session,
                &format!("sub_agent_{agent_label}"),
                completion_content,
            );

            let status = if has_error { "error" } else { "idle" };
            let _ = stream_tx.send(StreamEvent::AgentStatusChanged {
                agent_id: agent_id.clone(),
                label: agent_label.clone(),
                status: status.to_string(),
            });
            team.registry.set_session(&agent_id, child_session);
            team.registry
                .update_status(&agent_id, crate::agent_team::descriptor::AgentStatus::Idle);

            // 持久化 child_session 到磁盘
            if let Some(child) = team.registry.get_session(&agent_id) {
                crate::agent_team::lifecycle::persist_child_session(
                    parent_session,
                    &agent_id,
                    child,
                );
            }
        }

        result
    }

    /// 从共享团队上下文中取出主 Agent（main）收件箱里的待处理消息。
    pub(super) fn drain_main_agent_messages(
        &self,
    ) -> Vec<crate::agent_team::message_bus::AgentMessage> {
        self.team
            .as_ref()
            .and_then(|team| team.lock().ok().map(|mut team| team.drain_main_inbox()))
            .unwrap_or_default()
    }
}
