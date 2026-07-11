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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver as StdReceiver, Sender as StdSender};
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use tokio::sync::mpsc as tokio_mpsc;

use crate::agent_team::lifecycle::{TeamContext, prepared_agent_message_for_prompt};
use crate::core::command::Command;
use crate::model::TokenUsage;
use crate::react::message::{
    RuntimeMessageDisposition, accept_runtime_user_message, append_runtime_tool_message,
};
use crate::session::{Message, MessageRole, Session, now_text};
use tiangong_types::{PreparedUserMessage, StreamEvent};

use super::engine::ReactEngine;

/// 子 Agent 收件箱轮询结果。
#[derive(Default)]
pub(super) struct SubAgentDrainResult {
    pub usage: TokenUsage,
    pub ran: bool,
    pub cancelled: bool,
    pub current_agent_input: Option<String>,
    pub approval_responses: Vec<(String, bool)>,
}

/// 子 Agent 执行 future 的输出：
/// `(agent_id, agent_label, agent_role, child_session, new_messages, usage)`。
pub(super) type SubResult = (
    String,
    String,
    String,
    bool,
    Vec<String>,
    Session,
    Vec<Message>,
    TokenUsage,
);
pub(super) type SubAgentFuture = Pin<Box<dyn Future<Output = SubResult>>>;
pub(super) type ActiveSubAgent = (
    String,
    String,
    String,
    tokio_mpsc::UnboundedSender<Command>,
    Arc<AtomicBool>,
);

fn forward_approval_to_active_agents(
    active_sub_agents: &[ActiveSubAgent],
    request_id: &str,
    approved: bool,
) {
    for (_, _, _, tx, _) in active_sub_agents {
        let _ = tx.send(Command::Approval {
            request_id: request_id.to_string(),
            approved,
        });
    }
}

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
        model_excluded: false,
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
                    request_id,
                    tool_name,
                    args_summary,
                } => {
                    let _ = parent_tx.send(StreamEvent::ApprovalNeeded {
                        request_id: request_id.clone(),
                        tool_name: tool_name.clone(),
                        args_summary: args_summary.clone(),
                    });
                    send_sub_agent_output(
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
                    );
                }
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
        terminal_failure: &mut bool,
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

        type PendingInput = (PreparedUserMessage, String);
        type PendingAgent = (
            String,
            String,
            String,
            bool,
            String,
            Vec<String>,
            Vec<PendingInput>,
            Vec<String>,
            Session,
        );
        let mut pending: Vec<PendingAgent> = Vec::new();
        {
            let Ok(mut team) = team_arc.lock() else {
                let _ = stream_tx.send(StreamEvent::Error {
                    message: "团队状态锁定失败".to_string(),
                });
                *terminal_failure = true;
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
                let mut messages = team.registry.drain_inbox(&agent_id);
                if messages.is_empty() {
                    continue;
                }
                let direct_user = messages[0].session_message_id.is_some();
                let split_at = messages
                    .iter()
                    .position(|entry| entry.session_message_id.is_some() != direct_user)
                    .unwrap_or(messages.len());
                let remaining = messages.split_off(split_at);
                for entry in remaining {
                    team.registry.deliver_inbox_entry(&agent_id, entry);
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
                let delivery_ids = messages
                    .iter()
                    .filter(|entry| entry.session_message_id.is_some())
                    .map(|entry| entry.message.id.clone())
                    .collect::<Vec<_>>();
                let pending_inputs = messages
                    .into_iter()
                    .map(|entry| {
                        let message_id = entry
                            .session_message_id
                            .unwrap_or_else(|| entry.message.id.clone());
                        let prepared = prepared_agent_message_for_prompt(
                            &entry.message,
                            entry.additional_content,
                        );
                        (prepared, message_id)
                    })
                    .collect();
                pending.push((
                    agent_id,
                    agent_label,
                    agent_role,
                    direct_user,
                    system_prompt,
                    tool_names,
                    pending_inputs,
                    delivery_ids,
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
            direct_user,
            system_prompt,
            tool_names,
            pending_inputs,
            delivery_ids,
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
            // 每个子 Agent 持有独立取消信号，CancelAgent 可立即中断其挂起工具而
            // 不影响同批其他 Agent；全队 Cancel 会逐个设置这些信号。
            let sub_cancel_flag = Arc::new(AtomicBool::new(false));
            sub_engine = sub_engine.with_cancel_flag(sub_cancel_flag.clone());
            if let Some(flag) = &self.shutdown_flag {
                sub_engine = sub_engine.with_shutdown_flag(flag.clone());
            }
            for (prepared, message_id) in pending_inputs {
                let _ = sub_cmd_tx.send(Command::Message {
                    prepared,
                    message_id: Some(message_id),
                    persistence_ack: None,
                });
            }
            if let Ok(mut team) = team_arc.lock() {
                team.register_active_agent(agent_id.clone(), sub_cmd_tx.clone());
            }
            active_sub_agents.push((
                agent_id.clone(),
                agent_role.clone(),
                agent_label.clone(),
                sub_cmd_tx.clone(),
                sub_cancel_flag,
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
                    .execute_turn(&mut child_session, None, &child_stream_tx, &mut sub_cmd_rx)
                    .await;
                for (tool_call_id, tool_name, output) in
                    crate::react::message::close_unfinished_tool_calls_for_turn(&mut child_session)
                {
                    let _ = child_stream_tx.send(StreamEvent::ToolResult {
                        name: tool_name,
                        tool_call_id: Some(tool_call_id),
                        ok: false,
                        output,
                        full_output: None,
                        duration_ms: None,
                    });
                }
                // 图片数据只服务当前请求；子会话与主会话遵循相同的稳定持久化边界。
                child_session.clear_transient_content();
                drop(child_stream_tx);
                let _ = forwarder.join();
                let new_messages = child_session
                    .messages
                    .iter()
                    .skip(start_message_len)
                    .cloned()
                    .collect::<Vec<_>>();
                (
                    id,
                    label,
                    role,
                    direct_user,
                    delivery_ids,
                    child_session,
                    new_messages,
                    usage,
                )
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
                result.cancelled = true;
                return result;
            };
            team.set_dispatch_waker(dispatch_wake_tx);
        }

        // 并行执行所有待执行的 Sub Agent（协作并发）
        let mut futures: FuturesUnordered<SubAgentFuture> = FuturesUnordered::new();
        let mut active_sub_agents: Vec<ActiveSubAgent> = Vec::new();
        let mut cancelled_agents = std::collections::HashSet::new();
        let mut cancel_all_requested = false;
        let mut shutdown_requested = false;
        let mut terminal_error: Option<String> = None;
        if self.spawn_ready_sub_agents(
            &team_arc,
            stream_tx,
            &mut futures,
            &mut active_sub_agents,
            result.usage.total_tokens,
            max_concurrent,
            token_budget,
            sub_max_rounds,
            &mut result.cancelled,
        ) {
            result.ran = true;
        }

        if futures.is_empty() {
            if let Ok(mut team) = team_arc.lock() {
                team.clear_dispatch_waker();
            }
            return result;
        }

        loop {
            let mut results = Vec::new();
            while !futures.is_empty() {
                tokio::select! {
                maybe_result = futures.next() => {
                    if let Some(sub_result) = maybe_result {
                        active_sub_agents.retain(|(agent_id, _, _, _, _)| agent_id != &sub_result.0);
                        if let Ok(mut team) = team_arc.lock() {
                            team.unregister_active_agent(&sub_result.0);
                        }
                        results.push(sub_result);
                    }
                }
                maybe_wake = dispatch_wake_rx.recv(), if !result.cancelled => {
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
                            &mut result.cancelled,
                        )
                    {
                        result.ran = true;
                    }
                }
                maybe_cmd = cmd_rx.recv(), if !result.cancelled => {
                    match maybe_cmd {
                        Some(Command::Shutdown) => {
                            self.request_shutdown();
                            result.cancelled = true;
                            cancel_all_requested = true;
                            shutdown_requested = true;
                            terminal_error = Some("会话已关闭，Agent 执行已取消".to_string());
                            for (_, _, _, tx, cancel_flag) in &active_sub_agents {
                                cancel_flag.store(true, Ordering::Release);
                                let _ = tx.send(Command::Shutdown);
                            }
                            cancelled_agents.extend(
                                active_sub_agents
                                    .iter()
                                    .map(|(agent_id, _, _, _, _)| agent_id.clone()),
                            );
                        }
                        Some(Command::Cancel) => {
                            result.cancelled = true;
                            cancel_all_requested = true;
                            terminal_error = Some("已取消所有 Agent".to_string());
                            for (_, _, _, tx, cancel_flag) in &active_sub_agents {
                                cancel_flag.store(true, Ordering::Release);
                                let _ = tx.send(Command::Cancel);
                            }
                            cancelled_agents.extend(
                                active_sub_agents
                                    .iter()
                                    .map(|(agent_id, _, _, _, _)| agent_id.clone()),
                            );
                        }
                        Some(Command::CancelAgent { role }) => {
                            let mut matched = false;
                            for (agent_id, agent_role, agent_label, tx, cancel_flag) in &active_sub_agents {
                                if agent_role == &role {
                                    matched = true;
                                    cancelled_agents.insert(agent_id.clone());
                                    cancel_flag.store(true, Ordering::Release);
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
                        Some(Command::Message {
                            prepared,
                            message_id,
                            persistence_ack,
                        }) => {
                            match accept_runtime_user_message(
                                &self.agent_id,
                                Some(&team_arc),
                                parent_session,
                                stream_tx,
                                message_id,
                                prepared,
                                persistence_ack,
                            ) {
                                Ok(RuntimeMessageDisposition::CurrentAgentInput(text)) => {
                                    result.current_agent_input = Some(text);
                                }
                                Ok(RuntimeMessageDisposition::RoutedToAgent) => {}
                                Err(err) => tracing::warn!(
                                    error = %err,
                                    "团队执行期间追加用户消息持久化失败"
                                ),
                            }
                        }
                        Some(Command::UpdateCwd { cwd }) => {
                            parent_session.cwd = cwd;
                            crate::core::apply_session_cwd(parent_session);
                            result.cancelled = true;
                            cancel_all_requested = true;
                            terminal_error = Some(
                                "工作目录已更新，本轮已安全中断，请重新发送消息".to_string(),
                            );
                            for (_, _, _, tx, cancel_flag) in &active_sub_agents {
                                cancel_flag.store(true, Ordering::Release);
                                let _ = tx.send(Command::Cancel);
                            }
                            cancelled_agents.extend(
                                active_sub_agents
                                    .iter()
                                    .map(|(agent_id, _, _, _, _)| agent_id.clone()),
                            );
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
                            crate::core::reset_context_for_session(parent_session, stream_tx, &self.engine);
                        }
                        Some(Command::Approval {
                            request_id,
                            approved,
                        }) => {
                            forward_approval_to_active_agents(
                                &active_sub_agents,
                                &request_id,
                                approved,
                            );
                            result.approval_responses.push((request_id, approved));
                        }
                        Some(Command::ReloadConfig) => {}
                        Some(Command::InjectTool { tool_name, payload }) => {
                            self.defer_tool_injections(
                                parent_session,
                                stream_tx,
                                std::iter::once((tool_name, payload)),
                            );
                        }
                        Some(Command::EmitStreamEvent(ev)) => {
                            let ev = *ev;
                            let _ = stream_tx.send(ev);
                        }
                        None => break,
                    }
                }
                }
            }

            // 先原子式完成整批子会话落盘和状态回收，随后才允许写父会话完成记录。
            // 这样任一子会话持久化失败时，不会留下“部分 Agent 已完成、其余仍 Running”
            // 或父会话已经确认但子会话尚未保存的崩溃窗口。
            let batch_persist_error = if results.is_empty() {
                None
            } else {
                match team_arc.lock() {
                    Ok(mut team) => {
                        for (agent_id, _, _, _, _, child_session, _, _) in &results {
                            team.registry.set_session(agent_id, child_session.clone());
                            team.registry.update_status(
                                agent_id,
                                crate::agent_team::descriptor::AgentStatus::Idle,
                            );
                        }
                        let mut error = None;
                        for (agent_id, _, _, _, _, _, _, _) in &results {
                            let persist_result = team
                                .registry
                                .get_session(agent_id)
                                .ok_or_else(|| "Agent 子会话不存在".to_string())
                                .and_then(|child| {
                                    crate::agent_team::lifecycle::persist_child_session(
                                        parent_session,
                                        agent_id,
                                        child,
                                    )
                                });
                            if let Err(persist_error) = persist_result {
                                error = Some(format!(
                                    "持久化 Agent 子会话失败（{agent_id}）：{persist_error}"
                                ));
                                break;
                            }
                        }
                        if error.is_some() {
                            crate::agent_team::lifecycle::restore_pending_agent_deliveries(
                                &mut team,
                                parent_session,
                            );
                        }
                        error
                    }
                    Err(_) => Some("团队状态锁定失败".to_string()),
                }
            };
            if let Some(error) = batch_persist_error {
                result.cancelled = true;
                let _ = stream_tx.send(StreamEvent::Error { message: error });
                results.clear();
            }

            // 子会话批次已全部安全落盘，开始提交父会话的完成记录。
            'result_batch: for (
                agent_id,
                agent_label,
                agent_role,
                direct_user,
                delivery_ids,
                _child_session,
                output_messages,
                usage,
            ) in results
            {
                let run_cancelled = cancelled_agents.remove(&agent_id);
                // 检查 Sub Agent 是否有错误输出。
                // 错误由 persist_error 经 inject_tool_to_messages 注入（plugin_injection
                // 消息对，渲染文本含 "数据来源：react_loop_error"）。
                let is_error_message = |m: &Message| {
                    m.tool_name.as_deref() == Some(crate::react::message::INJECTION_TOOL_NAME)
                        && m.text_content().contains("数据来源：react_loop_error")
                };
                let has_error = output_messages.iter().any(is_error_message);
                if direct_user && run_cancelled && !cancel_all_requested {
                    terminal_error = Some(format!("{agent_label} 执行已取消"));
                } else if direct_user && has_error && !cancel_all_requested {
                    terminal_error = Some(format!("{agent_label} 执行失败"));
                }
                let direct_assistant_message = if direct_user && !run_cancelled && !has_error {
                    output_messages
                        .iter()
                        .rev()
                        .find(|message| {
                            message.role == MessageRole::Assistant
                                && message.phase == crate::session::MessagePhase::Summary
                                && (!message.content.is_empty()
                                    || !message.reasoning_content.is_empty())
                        })
                        .or_else(|| {
                            output_messages.iter().rev().find(|message| {
                                message.role == MessageRole::Assistant
                                    && (!message.content.is_empty()
                                        || !message.reasoning_content.is_empty())
                            })
                        })
                        .cloned()
                } else {
                    None
                };

                let completion_content = if run_cancelled {
                    format!("[{agent_label}] 执行已取消。")
                } else if has_error {
                    let error_content = output_messages
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
                        let summaries: Vec<String> = output_messages
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
                            output_messages
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
                        let brief = if !direct_user && summary.chars().count() > 500 {
                            format!("{}...", summary.chars().take(500).collect::<String>())
                        } else {
                            summary
                        };
                        format!("[{agent_label}] 执行完成\n{brief}")
                    }
                };

                result.usage.accumulate(&usage);

                // 子会话已在批次预提交阶段落盘，此处只处理父会话与消息总线。
                let Ok(mut team) = team_arc.lock() else {
                    let _ = stream_tx.send(StreamEvent::Error {
                        message: "团队状态锁定失败".to_string(),
                    });
                    result.cancelled = true;
                    continue;
                };
                let completion_message_id = scru128::new().to_string();
                let completion_message = crate::agent_team::message_bus::AgentMessage {
                    id: completion_message_id.clone(),
                    from: agent_id.clone(),
                    to: "main".to_string(),
                    content: completion_content.clone(),
                    priority: crate::agent_team::message_bus::MessagePriority::Normal,
                    created_at: now_text(),
                };
                let parent_before = parent_session.clone();
                let shutting_down = self
                    .shutdown_flag
                    .as_ref()
                    .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire));
                if result.cancelled {
                    // 整个团队执行已取消，不生成伪完成消息。
                } else if direct_user {
                    if let Some(mut message) = direct_assistant_message {
                        message.id = completion_message_id.clone();
                        message.worker_id = Some(format!("agent:{agent_role}:{agent_id}"));
                        message.model_excluded = true;
                        parent_session.messages.push(message);
                    } else {
                        parent_session.append_worker_message(
                            MessageRole::Assistant,
                            completion_content.clone(),
                            &format!("agent:{agent_role}:{agent_id}"),
                        );
                        if let Some(message) = parent_session.messages.last_mut() {
                            message.id = completion_message_id.clone();
                            message.model_excluded = true;
                        }
                    }
                } else {
                    team.deliver_main_message(completion_message);
                }
                append_runtime_tool_message(
                    parent_session,
                    &format!("sub_agent_{agent_label}"),
                    completion_content.clone(),
                );
                if !shutting_down {
                    parent_session.complete_pending_agent_deliveries(&delivery_ids);
                }
                let parent_changed = (!result.cancelled && direct_user)
                    || (!shutting_down && !delivery_ids.is_empty());
                let parent_update_persisted = if parent_changed {
                    match parent_session.try_persist_to_disk() {
                        Ok(()) => true,
                        Err(error) => {
                            *parent_session = parent_before;
                            let _ = stream_tx.send(StreamEvent::Error {
                                message: format!("持久化 Agent 完成结果失败：{error}"),
                            });
                            crate::agent_team::lifecycle::restore_pending_agent_deliveries(
                                &mut team,
                                parent_session,
                            );
                            result.cancelled = true;
                            false
                        }
                    }
                } else {
                    true
                };
                if !parent_update_persisted {
                    break 'result_batch;
                }
                {
                    if !result.cancelled && direct_user {
                        crate::react::message::emit_session_message_upsert_with_state(
                            parent_session,
                            stream_tx,
                            &completion_message_id,
                            true,
                            false,
                        );
                    }
                    // 正常直达完成由上面的单个 Upsert 原子同步消息和待投递列表；
                    // 显式取消没有完成消息，单独同步列表清理。
                    if result.cancelled && !shutting_down && !delivery_ids.is_empty() {
                        crate::react::message::emit_pending_agent_deliveries_changed(
                            parent_session,
                            stream_tx,
                        );
                    }
                    if !result.cancelled {
                        let _ = stream_tx.send(StreamEvent::AgentMessage {
                            from_agent_id: agent_id.clone(),
                            from_agent_label: agent_label.clone(),
                            to_agent_id: "main".to_string(),
                            to_agent_label: "Main Agent".to_string(),
                            content: completion_content.clone(),
                        });
                    }
                }

                let status = if has_error && !run_cancelled {
                    "error"
                } else {
                    "idle"
                };
                let _ = stream_tx.send(StreamEvent::AgentStatusChanged {
                    agent_id: agent_id.clone(),
                    label: agent_label.clone(),
                    status: status.to_string(),
                });
            }

            if result.cancelled
                || !self.spawn_ready_sub_agents(
                    &team_arc,
                    stream_tx,
                    &mut futures,
                    &mut active_sub_agents,
                    result.usage.total_tokens,
                    max_concurrent,
                    token_budget,
                    sub_max_rounds,
                    &mut result.cancelled,
                )
            {
                break;
            }
            result.ran = true;
        }

        if let Ok(mut team) = team_arc.lock() {
            team.clear_dispatch_waker();

            // 显式 Cancel 要在所有活跃 Agent 收尾后，原子清理尚未启动和已取消的
            // 直达投递；Shutdown 保留它们供重启恢复。
            if cancel_all_requested && !shutdown_requested {
                team.registry.clear_pending_inboxes();
                let delivery_ids = parent_session
                    .pending_agent_deliveries
                    .iter()
                    .map(|delivery| delivery.delivery_id.clone())
                    .collect::<Vec<_>>();
                let source_message_ids = parent_session
                    .pending_agent_deliveries
                    .iter()
                    .map(|delivery| delivery.source_message_id.clone())
                    .collect::<std::collections::HashSet<_>>();
                for source_message_id in source_message_ids {
                    team.registry
                        .remove_pending_source_message(&source_message_id);
                }
                parent_session.complete_pending_agent_deliveries(&delivery_ids);
                if let Err(error) = parent_session.try_persist_to_disk() {
                    terminal_error = Some(format!("持久化 Agent 取消状态失败：{error}"));
                } else if !delivery_ids.is_empty() {
                    crate::react::message::emit_pending_agent_deliveries_changed(
                        parent_session,
                        stream_tx,
                    );
                }
            }
        }

        if terminal_error.is_some() {
            result.cancelled = true;
        }

        if result.cancelled {
            let _ = stream_tx.send(StreamEvent::Error {
                message: terminal_error.unwrap_or_else(|| "Agent 执行已中断".to_string()),
            });
        }

        result
    }

    /// 把 Sub Agent 明确发给 Main Agent 的消息注入主模型上下文。
    /// 用户直达 Agent 的最终回复已在完成时直接写入父会话，不进入 main inbox。
    pub(super) fn inject_main_agent_messages(
        &self,
        parent_session: &mut Session,
        stream_tx: &StdSender<StreamEvent>,
    ) -> Option<String> {
        let messages = self
            .team
            .as_ref()
            .and_then(|team| {
                team.lock().ok().map(|mut team| {
                    team.drain_main_inbox()
                        .into_iter()
                        .map(|message| {
                            let role = team
                                .registry
                                .get(&message.from)
                                .map(|agent| agent.role.clone())
                                .unwrap_or_else(|| "unknown".to_string());
                            (message, role)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .unwrap_or_default();

        let mut latest = None;
        for (message, role) in messages {
            let content = format!(
                "[from:{} at {}]\n{}",
                message.from, message.created_at, message.content
            );
            latest = Some(content.clone());
            let mut session_message = Message::new(MessageRole::User, content);
            session_message.id = message.id;
            session_message.worker_id = Some(format!("agent:{role}:{}", message.from));
            let message_id = session_message.id.clone();
            parent_session.messages.push(session_message);
            crate::react::message::emit_session_message_upsert(
                parent_session,
                stream_tx,
                &message_id,
            );
        }
        latest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sub_agent_approval_event_keeps_request_id() {
        let (parent_tx, parent_rx) = std::sync::mpsc::channel();
        let (child_tx, child_rx) = std::sync::mpsc::channel();
        let handle = spawn_sub_agent_stream_forwarder(
            "agent-1".to_string(),
            "dev".to_string(),
            "Developer".to_string(),
            parent_tx,
            child_rx,
        );

        child_tx
            .send(StreamEvent::ApprovalNeeded {
                request_id: "approval-1".to_string(),
                tool_name: "write_file".to_string(),
                args_summary: "path=a.txt".to_string(),
            })
            .unwrap();
        drop(child_tx);
        handle.join().unwrap();

        assert!(parent_rx.try_iter().any(|event| matches!(
            event,
            StreamEvent::ApprovalNeeded { request_id, .. } if request_id == "approval-1"
        )));
    }

    #[test]
    fn approval_response_is_forwarded_to_active_sub_agents() {
        let (tx, mut rx) = tokio_mpsc::unbounded_channel();
        let active = vec![(
            "agent-1".to_string(),
            "dev".to_string(),
            "Developer".to_string(),
            tx,
            Arc::new(AtomicBool::new(false)),
        )];

        forward_approval_to_active_agents(&active, "approval-1", true);

        assert!(matches!(
            rx.try_recv(),
            Ok(Command::Approval { request_id, approved })
                if request_id == "approval-1" && approved
        ));
    }
}
