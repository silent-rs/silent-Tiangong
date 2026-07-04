//! ReactEngine: 单个 agent 的 async ReAct 循环
//!
//! 所有执行路径统一经过 `ReactEngine::execute_turn`，消除 sync/async 双版本。

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender as StdSender;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc as tokio_mpsc;

use crate::agents::execution_mcp_agent::McpFunctionTarget;
use crate::app_state::formatting::{format_llm_output_message, format_tool_trace_message};
use crate::context::assembler::filter_background_task_tools;
use crate::core::command::{Command, PendingCommandEffect};
use crate::model::{ModelRequest, TokenUsage, ToolSpec};
use crate::observe::{audit_permission_with_context, audit_tool_execution};
use crate::permission::{
    PermissionDecision, evaluate_tool_permission, format_call_args_summary, infer_audit_target,
    normalize_permission_target,
};
use crate::react::context::{
    emit_token_usage, handle_plugin_usage, maybe_update_context_summary, persist_error,
    select_client_for_request,
};
use crate::react::message::*;
use crate::runtime::LlmOutputRecord;
use crate::session::{Message, MessageRole, Session, now_text};
use crate::stream_throttle::ThrottledStreamSink;
use tiangong_types::{StreamEvent, StreamToolCall};

use crate::agent_team::lifecycle::TeamContext;

use super::cancel::{CancelSignal, CancelStrategy, emit_cancel_usage};
use super::helpers::{
    check_cancel, drain_pending_commands_async, looks_like_final_answer,
    maybe_inject_browser_update,
};
use super::summary::{ForceFinalReason, SummaryPhaseResult};

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

/// 单个 agent 的 async ReAct 执行引擎
pub(crate) struct ReactEngine {
    pub(super) engine: crate::runtime::RuntimeEngine,
    pub(super) tools: Vec<ToolSpec>,
    pub(super) mcp_targets: HashMap<String, McpFunctionTarget>,
    /// 单次工具执行阶段（ReAct Loop 内层）的最大轮次。
    pub(super) max_tool_rounds: usize,
    /// 总结阶段后重新进入工具执行阶段的最大次数。
    pub(super) max_outer_iterations: u32,
    pub(super) team: Option<Arc<Mutex<TeamContext>>>,
    pub(super) agent_id: String,
}

impl ReactEngine {
    pub(crate) fn new(
        engine: crate::runtime::RuntimeEngine,
        tools: Vec<ToolSpec>,
        mcp_targets: HashMap<String, McpFunctionTarget>,
        max_tool_rounds: usize,
        max_outer_iterations: u32,
    ) -> Self {
        Self {
            engine,
            tools,
            mcp_targets,
            max_tool_rounds,
            max_outer_iterations,
            team: None,
            agent_id: "main".to_string(),
        }
    }

    fn build_thinking_config(
        &self,
    ) -> (
        Option<crate::model::ThinkingConfig>,
        Option<crate::model::ReasoningEffort>,
        bool,
    ) {
        let effort_str = self
            .engine
            .agent_config()
            .reasoning_effort
            .trim()
            .to_lowercase();
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
    #[allow(clippy::too_many_arguments, unreachable_code)]
    pub(crate) async fn execute_turn(
        &mut self,
        session: &mut Session,
        user_input: &str,
        stream_tx: &StdSender<StreamEvent>,
        cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
    ) -> TokenUsage {
        let mut round = 0usize;
        let mut outer_iteration = 0u32;
        let mut accumulated_usage = TokenUsage::default();
        let mut successful_tool_call_keys = HashSet::new();
        let mut failed_tool_call_keys: HashMap<String, String> = HashMap::new();
        let mut failed_tool_names = HashSet::new();
        let mut last_browser_snapshot: Option<crate::browser_trait::PageSnapshot> = None;
        let mut last_browser_check: Option<std::time::Instant> = None;

        if self.agent_id == "main" {
            let routed = self
                .team
                .as_ref()
                .and_then(|team| {
                    team.lock().ok().map(|mut team| {
                        crate::agent_team::lifecycle::route_user_mentions(
                            &mut team, user_input, stream_tx,
                        )
                    })
                })
                .unwrap_or(false);
            if routed {
                let sub_result = self
                    .drain_sub_agent_inboxes(session, stream_tx, cmd_rx)
                    .await;
                accumulated_usage.accumulate(&sub_result.usage);
                if sub_result.cancelled {
                    session.persist_to_disk();
                    return accumulated_usage;
                }
                session.persist_to_disk();
                let _ = stream_tx.send(StreamEvent::Done {
                    usage: Some(accumulated_usage.clone()),
                });
                return accumulated_usage;
            }
        }

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
                // 首轮：始终重建 system prompt，确保规则段（如「文档附件解析规则」）
                // 与当前代码版本一致。旧 session 持久化的 system_prompt_message 可能
                // 是旧版本生成、缺少新增的规则段，必须重建。
                if round == 0 {
                    crate::react::context::rebuild_system_prompt(session, &self.engine);
                }
                match drain_pending_commands_async(
                    session,
                    &self.engine,
                    stream_tx,
                    cmd_rx,
                    &mut accumulated_usage,
                ) {
                    PendingCommandEffect::Terminate => return accumulated_usage,
                    PendingCommandEffect::MessageInjected => {
                        successful_tool_call_keys.clear();
                        failed_tool_call_keys.clear();
                        failed_tool_names.clear();
                    }
                    PendingCommandEffect::None => {}
                }

                // 首轮：主动获取浏览器当前状态并注入上下文
                if round == 0 {
                    maybe_inject_browser_update(
                        &self.engine,
                        session,
                        stream_tx,
                        &mut last_browser_snapshot,
                        &mut last_browser_check,
                        false,
                    )
                    .await;
                }

                // 内层工具执行阶段轮次上限：达到即结束工具阶段，进入总结。
                // 以本次外层迭代的起始轮次为基准计算，避免重入 Loop 时累计。
                if round.saturating_sub(iteration_start_round) >= self.max_tool_rounds {
                    break 'react_loop;
                }

                let request_tools = tools_for_current_turn(&self.tools, session, user_input);

                let (thinking, reasoning_effort, thinking_disabled) = self.build_thinking_config();
                let req = ModelRequest {
                    session_title: session.title.clone(),
                    // 当前用户消息已在 Command::Message 入口写入 session.messages。
                    // ReAct 多轮继续请求时不能再次追加 user_input，否则模型会把同一请求
                    // 误认为新的用户消息，反复从第一步重新开始。
                    user_input: String::new(),
                    context: session.context(),
                    thinking,
                    reasoning_effort,
                    thinking_disabled,
                    include_media: self.engine.chat_is_multimodal(),
                };

                let pending_msg_id = scru128::new().to_string();
                // 工具执行阶段的流式文本作为过程性输出（ReactText），前端紧凑展示。
                let sink = ThrottledStreamSink::with_text_kind(
                    pending_msg_id.clone(),
                    stream_tx.clone(),
                    crate::stream_throttle::StreamTextKind::React,
                );

                // async 流式调用 + select! 取消
                let (chunk_tx, mut chunk_rx) =
                    tokio_mpsc::unbounded_channel::<crate::model::ModelStreamChunk>();
                let client = select_client_for_request(&self.engine, &req).clone();
                let req_clone = req.clone();
                let tools_clone = request_tools.clone();
                let mut llm_fut = Some(tokio::task::spawn(async move {
                    client
                        .stream_function_calls_with_tool_choice(
                            req_clone,
                            tools_clone,
                            None,
                            chunk_tx,
                        )
                        .await
                }));
                let mut user_message_injected_during_stream = false;
                let mut streaming_usage = tiangong_types::TokenUsage::default();
                let cancel_strategy =
                    CancelStrategy::from_protocol(self.engine.client().protocol());
                let response_result: anyhow::Result<crate::model::ModelFunctionResponse> = loop {
                    tokio::select! {
                        biased;
                        cmd_opt = cmd_rx.recv() => {
                            match cmd_opt {
                                Some(Command::Cancel) | Some(Command::Shutdown) | None => {
                                    match cancel_strategy {
                                        CancelStrategy::AbortWithStreamingUsage => {
                                            break Err(anyhow::Error::new(CancelSignal::Abort));
                                        }
                                        CancelStrategy::WaitForUsage => {
                                            break Err(anyhow::Error::new(CancelSignal::WaitForUsage));
                                        }
                                    }
                                }
                                Some(Command::Message { content, message_id, media }) => {
                                    let mid = append_or_reuse_user_message(
                                        session, &content, message_id, media,
                                    );
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
                                    user_message_injected_during_stream = true;
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
                                Some(Command::EmitStreamEvent(ev)) => {
                                    let _ = stream_tx.send(ev);
                                }
                                Some(Command::ReportPluginUsage {
                                    usage,
                                    source,
                                    agent_id,
                                }) => {
                                    handle_plugin_usage(
                                        &mut accumulated_usage,
                                        stream_tx,
                                        self.engine.context_limit,
                                        &usage,
                                        &source,
                                        agent_id.as_deref(),
                                    );
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
                                    sink.push_chunk(&chunk)
                                }
                                None => {
                                    let response_result = match llm_fut.take().unwrap().await {
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
                        if let Some(signal) = CancelSignal::from_error(&err) {
                            match signal {
                                CancelSignal::Abort => {
                                    if let Some(handle) = llm_fut.take() {
                                        handle.abort();
                                    }
                                    accumulated_usage.accumulate(&streaming_usage);
                                    emit_cancel_usage(
                                        stream_tx,
                                        &accumulated_usage,
                                        self.engine.context_limit,
                                    );
                                    return accumulated_usage;
                                }
                                CancelSignal::WaitForUsage => {
                                    accumulated_usage.accumulate(&streaming_usage);
                                    // 先上报已知的 streaming_usage 作为保底
                                    if streaming_usage.total_tokens > 0 {
                                        emit_token_usage(
                                            stream_tx,
                                            &streaming_usage,
                                            None,
                                            self.engine.context_limit,
                                            "cancelled",
                                            None,
                                        );
                                    }
                                    // 立即通知前端取消已完成
                                    let _ = stream_tx.send(StreamEvent::Error {
                                        message: "已取消".into(),
                                    });
                                    // 后台等待 LLM 自然完成以补充完整 usage
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
                                                    "cancelled_background",
                                                    None,
                                                );
                                            }
                                        });
                                    }
                                    return accumulated_usage;
                                }
                            }
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
                            crate::react::context::maybe_update_context_summary(
                                session,
                                &self.engine,
                                &tiangong_types::TokenUsage {
                                    prompt_tokens: self.engine.context_limit,
                                    completion_tokens: 0,
                                    total_tokens: self.engine.context_limit,
                                    prompt_cache_hit_tokens: None,
                                    prompt_cache_miss_tokens: None,
                                },
                                stream_tx,
                            );
                            if session.summary_up_to > before_summary_up_to {
                                continue 'react_loop;
                            }
                        }
                        let _ = stream_tx.send(StreamEvent::Error {
                            message: err_msg.clone(),
                        });
                        // 持久化错误到 session，避免前端 Error 事件时序丢失导致中断无痕迹。
                        persist_error(session, format!("ReAct 循环请求失败：{err_msg}"));
                        return accumulated_usage;
                    }
                };

                accumulated_usage.accumulate(&response.usage);
                emit_token_usage(
                    stream_tx,
                    &response.usage,
                    Some(response.usage.prompt_tokens.max(session.current_tokens)),
                    self.engine.context_limit,
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
                    session.append_message_with_id(
                        pending_msg_id,
                        MessageRole::Assistant,
                        response.text.clone(),
                        response.reasoning_content.clone(),
                    );
                    if let Some(message) = session.messages.last_mut() {
                        message.reasoning_signature = response.reasoning_signature.clone();
                        // 过程性回复标记为 React 阶段：避免与总结阶段的最终回复混淆，
                        // 前端据此将其折叠为过程内容，只展示总结阶段的最终回复。
                        message.phase = crate::session::MessagePhase::React;
                    }
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
                    maybe_update_context_summary(session, &self.engine, &response.usage, stream_tx);

                    if user_message_injected_during_stream {
                        successful_tool_call_keys.clear();
                        failed_tool_call_keys.clear();
                        failed_tool_names.clear();
                        continue 'react_loop;
                    }

                    // 简单问答快速路径：首轮、本轮未执行工具、未注入新消息且 LLM 已给出
                    // 实质文本回复时，直接将该回复提升为最终回复（Summary），跳过总结阶段
                    // 的额外模型调用，满足"简单问答零额外开销"。
                    if outer_iteration == 0
                        && !executed_tool_in_iteration
                        && !response.text.trim().is_empty()
                    {
                        if let Some(message) = session.messages.last_mut() {
                            // 该消息此前已标记为 phase=React（上方无工具调用分支），
                            // 此处提升为 Summary：必须重新落盘，否则会话恢复后 phase 仍为
                            // React，前端会把最终回复折叠为过程内容（双总结问题的根因）。
                            message.phase = crate::session::MessagePhase::Summary;
                        }
                        session.persist_to_disk();
                        let _ = stream_tx.send(StreamEvent::Done {
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
                        if let Some(message) = session.messages.last_mut() {
                            message.phase = crate::session::MessagePhase::Summary;
                        }
                        session.persist_to_disk();
                        let _ = stream_tx.send(StreamEvent::Done {
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
                let tool_names: Vec<String> =
                    executable_calls.iter().map(|c| c.name.clone()).collect();
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
                let mut need_failure_recovery_prompt = false;
                for call in executable_calls {
                    // 本轮已尝试执行工具调用（无论成功/失败/跳过），标记以阻止
                    // 简单问答快速路径把后续无 tool_calls 的回复误判为直接回复。
                    executed_tool_in_iteration = true;
                    match drain_pending_commands_async(
                        session,
                        &self.engine,
                        stream_tx,
                        cmd_rx,
                        &mut accumulated_usage,
                    ) {
                        PendingCommandEffect::Terminate => return accumulated_usage,
                        PendingCommandEffect::MessageInjected => {
                            successful_tool_call_keys.clear();
                            failed_tool_call_keys.clear();
                            failed_tool_names.clear();
                            session.persist_to_disk();
                            continue 'react_loop;
                        }
                        PendingCommandEffect::None => {}
                    }

                    // 工具执行间隙：检测浏览器状态变化
                    maybe_inject_browser_update(
                        &self.engine,
                        session,
                        stream_tx,
                        &mut last_browser_snapshot,
                        &mut last_browser_check,
                        false,
                    )
                    .await;

                    if let Some(parse_error) = call
                        .arguments
                        .get("__parse_error")
                        .and_then(serde_json::Value::as_str)
                    {
                        let message = parse_error.to_string();
                        let failure = ToolFailureRecord::new(
                            &call.name,
                            &call.id,
                            format_call_args_summary(call),
                            ToolFailureKind::Argument,
                            message.clone(),
                        );
                        let provider_text = structured_tool_failure_provider_text(&failure);
                        let _ = stream_tx.send(StreamEvent::ToolResult {
                            name: call.name.clone(),
                            tool_call_id: Some(call.id.clone()),
                            ok: false,
                            output: message.clone(),
                            full_output: Some(message.clone()),
                            media: vec![],
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
                            media: vec![],
                        });
                        append_tool_result_message(
                            session,
                            &call.id,
                            &call.name,
                            if result.ok {
                                tool_result_provider_text(&call.name, &result, false)
                            } else {
                                structured_tool_failure_provider_text(&ToolFailureRecord::new(
                                    &call.name,
                                    &call.id,
                                    args_summary.clone(),
                                    classify_tool_result_failure(&result),
                                    tool_result_full_output(&result),
                                ))
                            },
                            !result.ok,
                        );
                        if result.ok {
                            let tool_call_key = tool_call_dedupe_key(&call.name, &call.arguments);
                            successful_tool_call_keys.insert(tool_call_key);
                        } else {
                            let tool_call_key = tool_call_dedupe_key(&call.name, &call.arguments);
                            failed_tool_call_keys.insert(
                                tool_call_key,
                                structured_tool_failure_provider_text(&ToolFailureRecord::new(
                                    &call.name,
                                    &call.id,
                                    args_summary.clone(),
                                    classify_tool_result_failure(&result),
                                    tool_result_full_output(&result),
                                )),
                            );
                            failed_tool_names.insert(call.name.clone());
                            need_failure_recovery_prompt = true;
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
                    if let Some(original_error) = failed_tool_call_keys.get(&tool_call_key).cloned()
                    {
                        let repeated_failure = ToolFailureRecord::repeated(
                            &call.name,
                            &call.id,
                            args_summary.clone(),
                            original_error,
                        );
                        append_repeated_failed_tool_result(
                            session,
                            stream_tx,
                            &call.id,
                            &call.name,
                            &structured_tool_failure_provider_text(&repeated_failure),
                        );
                        failed_tool_names.insert(call.name.clone());
                        need_failure_recovery_prompt = true;
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
                                media: vec![],
                            });
                            append_tool_result_message(
                                session,
                                &call.id,
                                &call.name,
                                structured_tool_failure_provider_text(&ToolFailureRecord::new(
                                    &call.name,
                                    &call.id,
                                    args_summary.clone(),
                                    ToolFailureKind::PermissionDenied,
                                    format!("权限拒绝：{reason}"),
                                )),
                                true,
                            );
                            failed_tool_call_keys.insert(
                                tool_call_key,
                                structured_tool_failure_provider_text(&ToolFailureRecord::new(
                                    &call.name,
                                    &call.id,
                                    args_summary.clone(),
                                    ToolFailureKind::PermissionDenied,
                                    format!("权限拒绝：{reason}"),
                                )),
                            );
                            failed_tool_names.insert(call.name.clone());
                            need_failure_recovery_prompt = true;
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
                                        let mid = append_or_reuse_user_message(
                                            session, &content, message_id, media,
                                        );
                                        let msg_media = session
                                            .messages
                                            .iter()
                                            .find(|message| message.id == mid)
                                            .map(|message| message.media.clone())
                                            .unwrap_or_default();
                                        let _ = stream_tx.send(StreamEvent::UserMessage {
                                            message_id: mid,
                                            content: content.clone(),
                                            media: msg_media,
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
                                            session, stream_tx, &tool_name, &payload,
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
                                    Some(Command::EmitStreamEvent(ev)) => {
                                        let _ = stream_tx.send(ev);
                                    }
                                    Some(Command::ReportPluginUsage {
                                        usage,
                                        source,
                                        agent_id,
                                    }) => {
                                        handle_plugin_usage(
                                            &mut accumulated_usage,
                                            stream_tx,
                                            self.engine.context_limit,
                                            &usage,
                                            &source,
                                            agent_id.as_deref(),
                                        );
                                    }
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
                                    structured_tool_failure_provider_text(&ToolFailureRecord::new(
                                        &call.name,
                                        &call.id,
                                        args_summary.clone(),
                                        ToolFailureKind::UserRejected,
                                        "用户拒绝执行",
                                    )),
                                    true,
                                );
                                session.persist_to_disk();
                                let _ = stream_tx.send(StreamEvent::ToolResult {
                                    name: call.name.clone(),
                                    tool_call_id: Some(call.id.clone()),
                                    ok: false,
                                    output: "用户拒绝执行".to_string(),
                                    full_output: None,
                                    media: vec![],
                                });
                                let _ = stream_tx.send(StreamEvent::Done {
                                    usage: Some(accumulated_usage.clone()),
                                });
                                return accumulated_usage;
                            }
                        }
                    }

                    if check_cancel(session, &self.engine, cmd_rx, stream_tx) {
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
                                    team.file_locks.ensure_can_write(
                                        &path_buf,
                                        &self.agent_id,
                                        &now,
                                    )
                                });
                            if let Err(message) = lock_error {
                                let failure =
                                    structured_tool_failure_provider_text(&ToolFailureRecord::new(
                                        &call.name,
                                        &call.id,
                                        args_summary.clone(),
                                        ToolFailureKind::PermissionDenied,
                                        message.clone(),
                                    ));
                                let _ = stream_tx.send(StreamEvent::ToolResult {
                                    name: call.name.clone(),
                                    tool_call_id: Some(call.id.clone()),
                                    ok: false,
                                    output: message.clone(),
                                    full_output: None,
                                    media: vec![],
                                });
                                append_tool_result_message(
                                    session,
                                    &call.id,
                                    &call.name,
                                    failure.clone(),
                                    true,
                                );
                                failed_tool_call_keys.insert(tool_call_key, failure);
                                failed_tool_names.insert(call.name.clone());
                                need_failure_recovery_prompt = true;
                                continue;
                            }
                        }
                    }

                    let _ = stream_tx.send(StreamEvent::ToolStart {
                        name: call.name.clone(),
                        args_summary: args_summary.clone(),
                    });

                    let (result, tool_llm_usage, allow_memory_context, usage_source) = {
                        // analyze_attachment 已迁移至独立插件（tiangong-plugin-analyze-
                        // attachment），经 execute_tool_call → tool_overrides 统一分发，
                        // 其 multimodal 子调用的 token 用量由插件经反馈通道上报。
                        let (mut result, tool_llm_usage, allow_memory_context, usage_source) = {
                            (
                                self.engine
                                    .execute_tool_call(
                                        call,
                                        &self.mcp_targets,
                                        &self.engine.agent_config().mcp,
                                        session,
                                    )
                                    .await,
                                tiangong_types::TokenUsage::default(),
                                false,
                                "",
                            )
                        };
                        crate::tool::media::localize_tool_result_images(&call.name, &mut result);
                        (result, tool_llm_usage, allow_memory_context, usage_source)
                    };
                    accumulated_usage.accumulate(&tool_llm_usage);
                    emit_token_usage(
                        stream_tx,
                        &tool_llm_usage,
                        None,
                        self.engine.context_limit,
                        usage_source,
                        None,
                    );

                    audit_tool_execution(
                        &session.id,
                        &call.name,
                        result.ok,
                        (!args_summary.is_empty()).then_some(args_summary.as_str()),
                        target_scope.as_deref(),
                        normalized_target.as_deref().or(target_summary.as_deref()),
                        &result.summary,
                    );
                    let tool_media = if result.ok {
                        crate::tool::media::parse_media_assets_from_tool_result(
                            &call.name,
                            &result.stdout,
                            &result.summary,
                        )
                    } else {
                        Vec::new()
                    };
                    let _ = stream_tx.send(StreamEvent::ToolResult {
                        name: call.name.clone(),
                        tool_call_id: Some(call.id.clone()),
                        ok: result.ok,
                        output: tool_result_stream_output(&result),
                        full_output: Some(tool_result_full_output(&result)),
                        media: tool_media.clone(),
                    });
                    // 媒体工具成功时，立即创建一条携带媒体的 assistant 消息，前端可实时渲染
                    if !tool_media.is_empty() {
                        session.append_message_with_media(
                            MessageRole::Assistant,
                            String::new(),
                            tool_media.clone(),
                        );
                    }
                    append_tool_result_message(
                        session,
                        &call.id,
                        &call.name,
                        if result.ok {
                            tool_result_provider_text(&call.name, &result, allow_memory_context)
                        } else {
                            structured_tool_failure_provider_text(&ToolFailureRecord::new(
                                &call.name,
                                &call.id,
                                args_summary.clone(),
                                classify_tool_result_failure(&result),
                                tool_result_full_output(&result),
                            ))
                        },
                        !result.ok,
                    );
                    append_runtime_tool_message(
                        session,
                        &call.name,
                        format_tool_trace_message(&result),
                    );
                    if !result.ok && check_cancel(session, &self.engine, cmd_rx, stream_tx) {
                        return accumulated_usage;
                    }

                    if result.ok {
                        failed_tool_call_keys.remove(&tool_call_key);
                        failed_tool_names.remove(&call.name);
                        successful_tool_call_keys.insert(tool_call_key);
                    } else {
                        let error_summary =
                            structured_tool_failure_provider_text(&ToolFailureRecord::new(
                                &call.name,
                                &call.id,
                                args_summary.clone(),
                                classify_tool_result_failure(&result),
                                tool_result_full_output(&result),
                            ));
                        failed_tool_call_keys.insert(tool_call_key, error_summary);
                        failed_tool_names.insert(call.name.clone());
                        need_failure_recovery_prompt = true;
                    }

                    if check_cancel(session, &self.engine, cmd_rx, stream_tx) {
                        return accumulated_usage;
                    }
                    // 记忆候选评估已下沉到 memory 插件 on_turn_finished（从 session 重建候选，
                    // 统一提交给 actor 的 pending list，反刍时自动合并）。
                    maybe_update_context_summary(session, &self.engine, &response.usage, stream_tx);

                    match drain_pending_commands_async(
                        session,
                        &self.engine,
                        stream_tx,
                        cmd_rx,
                        &mut accumulated_usage,
                    ) {
                        PendingCommandEffect::Terminate => return accumulated_usage,
                        PendingCommandEffect::MessageInjected => {
                            successful_tool_call_keys.clear();
                            failed_tool_call_keys.clear();
                            failed_tool_names.clear();
                            session.persist_to_disk();
                            continue 'react_loop;
                        }
                        PendingCommandEffect::None => {}
                    }

                    // 工具执行后：检测浏览器状态变化
                    maybe_inject_browser_update(
                        &self.engine,
                        session,
                        stream_tx,
                        &mut last_browser_snapshot,
                        &mut last_browser_check,
                        true,
                    )
                    .await;
                }

                if need_failure_recovery_prompt {
                    let mut failed_tools = failed_tool_names.iter().cloned().collect::<Vec<_>>();
                    failed_tools.sort();
                    let collaboration_hint = if self.agent_id == "main"
                        && request_tools.iter().any(|tool| tool.name == "create_agent")
                    {
                        "如果问题适合并行排查或需要第二视角，请创建 temporary Sub Agent 协作处理，并把失败工具、失败原因、已尝试方案和用户目标一并分配给它。"
                    } else {
                        "如果当前 Agent 无法继续独立推进，请向用户说明需要的外部条件、凭据、授权、环境调整或人工确认。"
                    };
                    let recall_hint = if request_tools
                        .iter()
                        .any(|tool| tool.name == "recall_memory")
                    {
                        "优先调用 recall_memory，充分使用 Memory 系统查询这个工具以前成功调用时使用的参数、环境前置条件、配置方式、替代步骤和相关经验；只有回忆不足以解决时，再切换工具、创建子 Agent 或请求用户协作。"
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

                // 执行有待处理任务的 Sub Agent
                let sub_result = self
                    .drain_sub_agent_inboxes(session, stream_tx, cmd_rx)
                    .await;
                accumulated_usage.accumulate(&sub_result.usage);
                if sub_result.cancelled {
                    session.persist_to_disk();
                    return accumulated_usage;
                }

                let main_messages = self.drain_main_agent_messages();
                if !main_messages.is_empty() {
                    for message in main_messages {
                        session.messages.push(Message::new(
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

            // ── 总结阶段 ──
            // 内层工具执行阶段结束后，由主模型独立判断任务完成度并输出最终回复。
            let summary_result = self
                .run_summary_phase(session, stream_tx, cmd_rx, outer_iteration + 1)
                .await;
            match summary_result {
                SummaryPhaseResult::Completed(usage) => {
                    accumulated_usage.accumulate(&usage);
                    let _ = stream_tx.send(StreamEvent::Done {
                        usage: Some(accumulated_usage.clone()),
                    });
                    return accumulated_usage;
                }
                SummaryPhaseResult::NeedMoreWork { reason, usage } => {
                    accumulated_usage.accumulate(&usage);
                    outer_iteration += 1;
                    if outer_iteration >= self.max_outer_iterations {
                        // 重入次数已达上限，强制输出最终回复。
                        self.force_final_response(session, stream_tx, ForceFinalReason::OuterLimit);
                        return accumulated_usage;
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
                    return accumulated_usage;
                }
                SummaryPhaseResult::Failed { message, usage } => {
                    accumulated_usage.accumulate(&usage);
                    let _ = stream_tx.send(StreamEvent::Error {
                        message: format!("总结阶段失败：{message}"),
                    });
                    persist_error(session, format!("总结阶段失败：{message}"));
                    self.force_final_response(session, stream_tx, ForceFinalReason::SummaryError);
                    return accumulated_usage;
                }
            }
        }

        accumulated_usage
    }
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
            .find(|message| message.role == MessageRole::User)
            .map(|message| message.text_content())
            .unwrap_or_default()
    } else {
        user_input.to_string()
    };
    filter_background_task_tools(tools.to_vec(), &intent_text)
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
