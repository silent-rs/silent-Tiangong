//! ReactEngine: 单个 agent 的 async ReAct 循环
//!
//! 所有执行路径统一经过 `ReactEngine::execute_turn`，消除 sync/async 双版本。

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender as StdSender;

use tokio::sync::mpsc as tokio_mpsc;

use crate::context::assembler::filter_background_task_tools;
use crate::core::command::{Command, PendingCommandEffect};
use crate::formatting::{format_llm_output_message, format_tool_trace_message};
use crate::model::{ModelRequest, TokenUsage, ToolSpec};
use crate::observe::{audit_permission_with_context, audit_tool_execution};
use crate::permission::{
    PermissionDecision, evaluate_tool_permission, format_call_args_summary, infer_audit_target,
    normalize_permission_target,
};
use crate::react::context::{
    emit_token_usage, maybe_update_context_summary, persist_error, select_client_for_request,
};
use crate::react::message::*;
use crate::runtime::LlmOutputRecord;
use crate::session::{Message, MessageRole, Session, now_text};
use crate::stream_throttle::ThrottledStreamSink;
use tiangong_types::{ContentBlock, StreamEvent, StreamToolCall, content_blocks_text};

use super::cancel::{
    CancelSignal, abort_and_join, emit_cancel_usage, emit_cancelled, wait_for_abort_signal,
};
use super::helpers::{
    check_cancel, drain_pending_commands_async, looks_like_final_answer, process_buffered_commands,
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
pub struct ReactEngine {
    /// 共享的 RuntimeEngine（嵌套执行经 clone 继承父引擎的 tool_overrides）。
    pub engine: crate::runtime::RuntimeEngine,
    /// 当前执行单元可用的工具集。
    pub tools: Vec<ToolSpec>,
    /// 单次工具执行阶段（ReAct Loop 内层）的最大轮次。
    pub max_tool_rounds: usize,
    /// 总结阶段后重新进入工具执行阶段的最大次数。
    pub max_outer_iterations: u32,
    /// 当前执行单元身份。
    pub agent_id: String,
    /// 调用方提供的完整 system prompt；用于插件启动的嵌套 Agent。
    system_prompt_override: Option<Message>,
    /// 取消信号（独立于命令队列）：check_cancel 读取此标志判断是否取消，
    /// 不排空命令队列，避免乱序。
    pub(super) cancel_flag: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub(super) shutdown_flag: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Core 单轮信任模式控制器；插件直接构造的嵌套 Agent 不需要。
    pub(super) turn_trust_mode: Option<crate::core::TurnTrustModeController>,
}

impl ReactEngine {
    pub fn new(
        engine: crate::runtime::RuntimeEngine,
        tools: Vec<ToolSpec>,
        max_tool_rounds: usize,
        max_outer_iterations: u32,
    ) -> Self {
        Self {
            engine,
            tools,
            max_tool_rounds,
            max_outer_iterations,
            agent_id: "main".to_string(),
            system_prompt_override: None,
            cancel_flag: None,
            shutdown_flag: None,
            turn_trust_mode: None,
        }
    }

    pub(super) fn defer_tool_injections(
        &mut self,
        session: &mut Session,
        stream_tx: &StdSender<StreamEvent>,
        injections: impl IntoIterator<Item = (String, serde_json::Value)>,
    ) {
        for (tool_name, payload) in injections {
            crate::react::message::defer_tool_injection(session, stream_tx, tool_name, payload);
        }
    }

    pub(super) fn flush_deferred_tool_injections(
        &mut self,
        session: &mut Session,
        stream_tx: &StdSender<StreamEvent>,
    ) {
        crate::react::message::flush_deferred_tool_injections(session, stream_tx);
    }

    /// 注入取消信号，供 check_cancel 读取（独立于命令队列，不排空队列）。
    pub fn with_cancel_flag(
        mut self,
        cancel_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        self.cancel_flag = Some(cancel_flag);
        self
    }

    pub fn with_shutdown_flag(
        mut self,
        shutdown_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        self.shutdown_flag = Some(shutdown_flag);
        self
    }

    pub(crate) fn with_turn_trust_mode_controller(
        mut self,
        controller: crate::core::TurnTrustModeController,
    ) -> Self {
        self.turn_trust_mode = Some(controller);
        self
    }

    pub(super) fn apply_message_trust_mode_override(
        &self,
        trust_mode_override: Option<crate::permission::TrustMode>,
    ) {
        if let Some(controller) = &self.turn_trust_mode {
            controller.apply_message_override(trust_mode_override);
        }
    }

    /// 只有 Core 的主执行单元负责把宿主控制命令交给插件。
    ///
    /// 插件启动的嵌套 ReactEngine 共享同一份 RuntimeEngine；若子执行单元把自己
    /// 收到的 Approval/Cancel/Shutdown 再广播回插件，Agent Team 会把命令重新投递
    /// 给包括当前子 Agent 在内的所有执行单元，形成命令回声，定向取消还会升级为
    /// 全团队取消。
    pub(super) fn forward_plugin_runtime_command(&self, command: &Command) -> bool {
        if self.agent_id != "main" {
            return false;
        }
        self.engine.handle_plugin_runtime_command(command)
    }

    pub(super) fn request_shutdown(&self) {
        if let Some(flag) = &self.shutdown_flag {
            flag.store(true, std::sync::atomic::Ordering::Release);
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

    /// 设置当前执行单元身份，供插件策略与工具处理器识别调用方。
    pub fn with_agent_id(mut self, agent_id: String) -> Self {
        self.agent_id = agent_id;
        self
    }

    /// 为嵌套 Agent 固定专属 system prompt，避免首轮按主 Agent 配置重建覆盖。
    pub fn with_system_prompt(mut self, system_prompt: Message) -> Self {
        self.system_prompt_override = Some(system_prompt);
        self
    }

    /// 执行一个完整的对话轮次（可能多轮工具调用），async 版
    ///
    /// 每轮之间检查 cmd_rx：新消息注入上下文，cancel 立即生效。
    #[allow(clippy::too_many_arguments, unreachable_code)]
    pub async fn execute_turn(
        &mut self,
        session: &mut Session,
        initial_user_message: Option<(&str, &[ContentBlock])>,
        stream_tx: &StdSender<StreamEvent>,
        cmd_rx: &mut tokio_mpsc::UnboundedReceiver<Command>,
    ) -> TokenUsage {
        let mut round = 0usize;
        let mut outer_iteration = 0u32;
        let mut accumulated_usage = TokenUsage::default();
        // 绑定本轮 turn-scoped 插件 usage sink：插件经 PluginFeedbackTx.report_token_usage
        // 即时累加到本轮并立即发送 StreamEvent::TokenUsage（不走命令队列，避免被
        // check_cancel 等 drain 吞掉）。_usage_guard drop 时自动解绑，迟到的 usage 不会
        // 计入下一轮。每个 return accumulated_usage 前先 merge_plugin_usage 折算插件用量。
        let usage_sink = self.engine.turn_usage_sink().clone();
        let _usage_guard = usage_sink.bind(stream_tx.clone(), self.engine.context_limit);
        // 把本轮插件累计的 usage 折算进 accumulated_usage（在每个返回点调用）。
        // 注意：捕获 usage_sink 的 clone，而非 self，避免与循环内 &mut self 借用冲突。
        let merge_plugin_usage = |acc: &mut TokenUsage| {
            acc.accumulate(&usage_sink.take_usage());
        };
        let mut successful_tool_call_keys = HashSet::new();
        let mut failed_tool_call_keys: HashMap<String, String> = HashMap::new();
        let mut failed_tool_names = HashSet::new();
        let mut user_input = initial_user_message
            .map(|(_, prepared)| content_blocks_text(prepared))
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
                // 首轮始终重建 system prompt，确保规则段与当前代码版本一致。
                // 旧 session 持久化的 system_prompt_message 可能缺少新增规则。
                if round == 0 {
                    if let Some(system_prompt) = &self.system_prompt_override {
                        session.system_prompt_message = Some(system_prompt.clone());
                    } else {
                        crate::react::context::rebuild_system_prompt(session, &self.engine);
                    }
                }
                match drain_pending_commands_async(
                    session,
                    &self.engine,
                    &self.agent_id,
                    stream_tx,
                    cmd_rx,
                    self.turn_trust_mode.as_ref(),
                ) {
                    PendingCommandEffect::Terminate => {
                        merge_plugin_usage(&mut accumulated_usage);
                        return accumulated_usage;
                    }
                    PendingCommandEffect::Shutdown => {
                        self.request_shutdown();
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
                self.flush_deferred_tool_injections(session, stream_tx);

                // 工具执行完进入下一轮模型请求前，通知前端"正在分析工具结果"，
                // 避免前端把模型等待时间算到最后一个工具上。
                if round > 0 {
                    let _ = stream_tx.send(StreamEvent::PhaseChanged {
                        phase: "analyzing".to_string(),
                        iteration: (round + 1) as u32,
                    });
                }

                // 内层工具执行阶段轮次上限：达到即结束工具阶段，进入总结。
                // 以本次外层迭代的起始轮次为基准计算，避免重入 Loop 时累计。
                if round.saturating_sub(iteration_start_round) >= self.max_tool_rounds {
                    break 'react_loop;
                }

                let request_tools = tools_for_current_turn(&self.tools, session, &user_input);

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
                                        crate::react::message::upsert_assistant_text_message(
                                            session,
                                            &pending_msg_id,
                                            &streamed_text,
                                            &streamed_reasoning,
                                            crate::session::MessagePhase::React,
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
                                            self.apply_message_trust_mode_override(
                                                trust_mode_override,
                                            );
                                            stream_interruption = Some(input);
                                            if let Some(handle) = llm_fut.take() {
                                                abort_and_join(handle).await;
                                            }
                                            break Err(anyhow::anyhow!(
                                                "模型响应已被新的用户消息中断"
                                            ));
                                        }
                                        Ok(RuntimeMessageDisposition::RoutedToPlugin) => {}
                                        Err(err) => {
                                            session
                                                .messages
                                                .truncate(message_count_before_interruption);
                                            tracing::warn!(
                                                error = %err,
                                                "流式阶段追加用户消息持久化失败"
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
                                    let trust_mode =
                                        self.engine.permission_gate().trust_mode_handle();
                                    if let Err(error) = crate::core::apply_session_metadata_update(
                                        session,
                                        &trust_mode,
                                        update,
                                        persistence_ack,
                                    ) {
                                        tracing::warn!(%error, "流式阶段更新会话元数据失败");
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
                                }                            }
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
                                            emit_cancel_usage(
                                                stream_tx,
                                                &streaming_usage,
                                                self.engine.context_limit,
                                            );
                                            merge_plugin_usage(&mut accumulated_usage);
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

                if !streamed_text.trim().is_empty() || !streamed_reasoning.trim().is_empty() {
                    crate::react::message::upsert_assistant_text_message(
                        session,
                        &pending_msg_id,
                        &streamed_text,
                        &streamed_reasoning,
                        crate::session::MessagePhase::React,
                    );
                    crate::react::message::emit_session_message_upsert(
                        session,
                        stream_tx,
                        &pending_msg_id,
                    );
                }

                if let Some(input) = stream_interruption {
                    accumulated_usage.accumulate(&streaming_usage);
                    if streaming_usage.total_tokens > 0 {
                        emit_token_usage(
                            stream_tx,
                            &streaming_usage,
                            None,
                            self.engine.context_limit,
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
                            emit_cancel_usage(
                                stream_tx,
                                &streaming_usage,
                                self.engine.context_limit,
                            );
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
                                    &self.engine,
                                    &tiangong_types::TokenUsage {
                                        prompt_tokens: self.engine.context_limit,
                                        completion_tokens: 0,
                                        total_tokens: self.engine.context_limit,
                                        prompt_cache_hit_tokens: None,
                                        prompt_cache_miss_tokens: None,
                                    },
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
                                merge_plugin_usage(&mut accumulated_usage);
                                return accumulated_usage;
                            }
                            if session.summary_up_to > before_summary_up_to {
                                continue 'react_loop;
                            }
                        }
                        let _ = stream_tx.send(StreamEvent::Error {
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
                    crate::react::message::emit_session_message_upsert(
                        session,
                        stream_tx,
                        &pending_msg_id,
                    );
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
                    let compression_cancelled = maybe_update_context_summary(
                        session,
                        &self.engine,
                        &response.usage,
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
                            stream_tx,
                            &pending_msg_id,
                        );
                        session.persist_to_disk();
                        merge_plugin_usage(&mut accumulated_usage);
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
                        if let Some(message) = session
                            .messages
                            .iter_mut()
                            .find(|message| message.id == pending_msg_id)
                        {
                            message.phase = crate::session::MessagePhase::Summary;
                        }
                        crate::react::message::emit_session_message_upsert(
                            session,
                            stream_tx,
                            &pending_msg_id,
                        );
                        session.persist_to_disk();
                        merge_plugin_usage(&mut accumulated_usage);
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
                crate::react::message::emit_session_message_upsert(
                    session,
                    stream_tx,
                    &pending_msg_id,
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
                        &self.agent_id,
                        stream_tx,
                        cmd_rx,
                        self.turn_trust_mode.as_ref(),
                    ) {
                        PendingCommandEffect::Terminate => {
                            merge_plugin_usage(&mut accumulated_usage);
                            return accumulated_usage;
                        }
                        PendingCommandEffect::Shutdown => {
                            self.request_shutdown();
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
                                duration_ms: None,
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
                                        self.request_shutdown();
                                        crate::approval_store::remove_pending(
                                            &session.id,
                                            &request_id,
                                        );
                                        let _ = stream_tx.send(StreamEvent::Error {
                                            message: "已取消".into(),
                                        });
                                        merge_plugin_usage(&mut accumulated_usage);
                                        return accumulated_usage;
                                    }
                                    Some(Command::Cancel) | None => {
                                        crate::approval_store::remove_pending(
                                            &session.id,
                                            &request_id,
                                        );
                                        let _ = stream_tx.send(StreamEvent::Error {
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
                                        trust_mode_override,
                                        persistence_ack,
                                    }) => {
                                        match accept_runtime_user_message(
                                            &self.engine,
                                            &self.agent_id,
                                            session,
                                            stream_tx,
                                            message_id,
                                            prepared,
                                            persistence_ack,
                                        ) {
                                            Ok(RuntimeMessageDisposition::CurrentAgentInput(
                                                input,
                                            )) => {
                                                self.apply_message_trust_mode_override(
                                                    trust_mode_override,
                                                );
                                                break ApprovalWaitOutcome::CurrentInput(input);
                                            }
                                            Ok(RuntimeMessageDisposition::RoutedToPlugin) => {}
                                            Err(err) => tracing::warn!(
                                                error = %err,
                                                "审批等待阶段追加用户消息持久化失败"
                                            ),
                                        }
                                    }
                                    Some(Command::UpdateCwd { cwd }) => {
                                        session.cwd = cwd;
                                        crate::core::apply_session_cwd(session);
                                        crate::approval_store::remove_pending(
                                            &session.id,
                                            &request_id,
                                        );
                                        let _ = stream_tx.send(StreamEvent::Error {
                                            message:
                                                "工作目录已更新，本轮已安全中断，请重新发送消息"
                                                    .to_string(),
                                        });
                                        merge_plugin_usage(&mut accumulated_usage);
                                        return accumulated_usage;
                                    }
                                    Some(Command::UpdateSessionMetadata {
                                        update,
                                        persistence_ack,
                                    }) => {
                                        let trust_mode =
                                            self.engine.permission_gate().trust_mode_handle();
                                        if let Err(error) =
                                            crate::core::apply_session_metadata_update(
                                                session,
                                                &trust_mode,
                                                update,
                                                persistence_ack,
                                            )
                                        {
                                            tracing::warn!(%error, "审批等待阶段更新会话元数据失败");
                                        }
                                    }
                                    Some(Command::ReloadConfig) => {}
                                    Some(command @ Command::Approval { .. })
                                    | Some(command @ Command::PluginControl { .. }) => {
                                        self.forward_plugin_runtime_command(&command);
                                    }
                                    Some(Command::InjectTool { tool_name, payload }) => {
                                        self.defer_tool_injections(
                                            session,
                                            stream_tx,
                                            std::iter::once((tool_name, payload)),
                                        );
                                    }
                                    Some(Command::CommitPluginDeliveries {
                                        delivery_ids,
                                        tool_injections,
                                        persistence_ack,
                                    }) => {
                                        if let Err(error) =
                                            crate::react::message::commit_plugin_deliveries(
                                                session,
                                                stream_tx,
                                                delivery_ids,
                                                tool_injections,
                                                persistence_ack,
                                            )
                                        {
                                            tracing::warn!(%error, "提交插件持久投递失败");
                                        }
                                    }
                                    Some(Command::CompressContext) => {
                                        let _ = stream_tx.send(StreamEvent::AgentNotification {
                                            agent_id: "system".to_string(),
                                            agent_label: "系统".to_string(),
                                            content:
                                                "当前轮次执行中，已跳过手动压缩，请在轮次结束后重试"
                                                    .to_string(),
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
                            };

                            crate::approval_store::remove_pending(&session.id, &request_id);

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
                                self.flush_deferred_tool_injections(session, stream_tx);
                                session.persist_to_disk();
                                let _ = stream_tx.send(StreamEvent::ToolResult {
                                    name: call.name.clone(),
                                    tool_call_id: Some(call.id.clone()),
                                    ok: false,
                                    output: "用户拒绝执行".to_string(),
                                    full_output: None,
                                    duration_ms: None,
                                });
                                merge_plugin_usage(&mut accumulated_usage);
                                let _ = stream_tx.send(StreamEvent::Done {
                                    usage: Some(accumulated_usage.clone()),
                                });
                                return accumulated_usage;
                            }
                        }
                    }

                    if check_cancel(
                        self.cancel_flag
                            .as_ref()
                            .expect("cancel_flag 必须在 execute_turn 前注入"),
                    ) {
                        {
                            let _ = stream_tx.send(StreamEvent::Error {
                                message: "已取消".into(),
                            });
                            merge_plugin_usage(&mut accumulated_usage);
                            return accumulated_usage;
                        }
                    }

                    let _ = stream_tx.send(StreamEvent::ToolStart {
                        name: call.name.clone(),
                        args_summary: args_summary.clone(),
                    });
                    let tool_start_time = std::time::Instant::now();

                    let cancel_flag = self
                        .cancel_flag
                        .as_ref()
                        .expect("cancel_flag 必须在 execute_turn 前注入")
                        .clone();
                    let shutdown_flag = self.shutdown_flag.clone();
                    let mut buffered_tool_commands = Vec::new();
                    // 工具处理器只在启动瞬间借用 Session；执行期间 Core 仍可原子提交
                    // 插件投递并及时回 ACK，避免工具等待 ACK、Core 又等待工具的互等。
                    let mut tool_future =
                        self.engine.start_tool_call(call, session, &self.agent_id);
                    let result = loop {
                        tokio::select! {
                            biased;
                            _ = wait_for_abort_signal(cancel_flag.clone(), shutdown_flag.clone()) => {
                                let command = if shutdown_flag.as_ref().is_some_and(|flag| {
                                    flag.load(std::sync::atomic::Ordering::Acquire)
                                }) {
                                    Command::Shutdown
                                } else {
                                    Command::Cancel
                                };
                                self.forward_plugin_runtime_command(&command);
                                break None;
                            }
                            result = &mut tool_future => break Some(result),
                            command = cmd_rx.recv() => {
                                match command {
                                    Some(command @ Command::Approval { .. })
                                    | Some(command @ Command::PluginControl { .. }) => {
                                        self.forward_plugin_runtime_command(&command);
                                    }
                                    Some(Command::EmitStreamEvent(event)) => {
                                        let _ = stream_tx.send(*event);
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
                                            tracing::warn!(%error, "工具执行期间提交插件持久投递失败");
                                        }
                                    }
                                    Some(Command::UpdateSessionMetadata {
                                        update,
                                        persistence_ack,
                                    }) => {
                                        let trust_mode =
                                            self.engine.permission_gate().trust_mode_handle();
                                        if let Err(error) =
                                            crate::core::apply_session_metadata_update(
                                                session,
                                                &trust_mode,
                                                update,
                                                persistence_ack,
                                            )
                                        {
                                            tracing::warn!(%error, "工具执行期间更新会话元数据失败");
                                        }
                                    }
                                    Some(Command::Cancel) => {
                                        self.forward_plugin_runtime_command(&Command::Cancel);
                                        break None;
                                    }
                                    Some(Command::Shutdown) => {
                                        self.request_shutdown();
                                        self.forward_plugin_runtime_command(&Command::Shutdown);
                                        break None;
                                    }
                                    Some(command) => buffered_tool_commands.push(command),
                                    None => {
                                        self.forward_plugin_runtime_command(&Command::Cancel);
                                        break None;
                                    }
                                }
                            }
                        }
                    };
                    drop(tool_future);
                    let Some(result) = result else {
                        let output = "工具调用因执行取消或会话关闭而中断。".to_string();
                        let _ = stream_tx.send(StreamEvent::ToolResult {
                            name: call.name.clone(),
                            tool_call_id: Some(call.id.clone()),
                            ok: false,
                            output: output.clone(),
                            full_output: None,
                            duration_ms: Some(tool_start_time.elapsed().as_millis() as u64),
                        });
                        append_tool_result_message(session, &call.id, &call.name, output, true);
                        let buffered_effect = process_buffered_commands(
                            session,
                            &self.engine,
                            &self.agent_id,
                            stream_tx,
                            buffered_tool_commands,
                            self.turn_trust_mode.as_ref(),
                        );
                        if let PendingCommandEffect::MessagesInjected {
                            current_agent_input: Some(input),
                            ..
                        } = buffered_effect
                            && !self
                                .shutdown_flag
                                .as_ref()
                                .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
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
                        let _ = stream_tx.send(StreamEvent::Error {
                            message: "已取消".into(),
                        });
                        return accumulated_usage;
                    };
                    let tool_llm_usage = tiangong_types::TokenUsage::default();
                    let allow_memory_context = false;
                    let usage_source = "";
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
                    let _ = stream_tx.send(StreamEvent::ToolResult {
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
                    match process_buffered_commands(
                        session,
                        &self.engine,
                        &self.agent_id,
                        stream_tx,
                        buffered_tool_commands,
                        self.turn_trust_mode.as_ref(),
                    ) {
                        PendingCommandEffect::Terminate => {
                            merge_plugin_usage(&mut accumulated_usage);
                            return accumulated_usage;
                        }
                        PendingCommandEffect::Shutdown => {
                            self.request_shutdown();
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
                        PendingCommandEffect::MessagesInjected { .. }
                        | PendingCommandEffect::None => {}
                    }
                    if !result.ok
                        && check_cancel(
                            self.cancel_flag
                                .as_ref()
                                .expect("cancel_flag 必须在 execute_turn 前注入"),
                        )
                    {
                        {
                            let _ = stream_tx.send(StreamEvent::Error {
                                message: "已取消".into(),
                            });
                            merge_plugin_usage(&mut accumulated_usage);
                            return accumulated_usage;
                        }
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

                    if check_cancel(
                        self.cancel_flag
                            .as_ref()
                            .expect("cancel_flag 必须在 execute_turn 前注入"),
                    ) {
                        {
                            let _ = stream_tx.send(StreamEvent::Error {
                                message: "已取消".into(),
                            });
                            merge_plugin_usage(&mut accumulated_usage);
                            return accumulated_usage;
                        }
                    }
                    // 记忆候选评估已下沉到 memory 插件 on_turn_finished（从 session 重建候选，
                    // 统一提交给 actor 的 pending list，反刍时自动合并）。
                    let compression_cancelled = maybe_update_context_summary(
                        session,
                        &self.engine,
                        &response.usage,
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
                        merge_plugin_usage(&mut accumulated_usage);
                        return accumulated_usage;
                    }

                    match drain_pending_commands_async(
                        session,
                        &self.engine,
                        &self.agent_id,
                        stream_tx,
                        cmd_rx,
                        self.turn_trust_mode.as_ref(),
                    ) {
                        PendingCommandEffect::Terminate => {
                            merge_plugin_usage(&mut accumulated_usage);
                            return accumulated_usage;
                        }
                        PendingCommandEffect::Shutdown => {
                            self.request_shutdown();
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
                    self.flush_deferred_tool_injections(session, stream_tx);
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
            let summary_result = self
                .run_summary_phase(session, stream_tx, cmd_rx, outer_iteration + 1)
                .await;
            match summary_result {
                SummaryPhaseResult::Completed(usage) => {
                    accumulated_usage.accumulate(&usage);
                    merge_plugin_usage(&mut accumulated_usage);
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
                        self.force_final_response(session, stream_tx, ForceFinalReason::OuterLimit)
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
                    self.force_final_response(session, stream_tx, ForceFinalReason::SummaryError)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::react::test_support::{
        MockLlmServer, MockResponse, RecordedRuntimeCommand, approval, plugin_control,
        runtime_with_recorder,
    };

    fn test_react_engine(
        engine: crate::runtime::RuntimeEngine,
        tools: Vec<ToolSpec>,
    ) -> ReactEngine {
        ReactEngine::new(engine, tools, 2, 1)
            .with_cancel_flag(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )))
            .with_shutdown_flag(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )))
    }

    #[test]
    fn nested_engine_does_not_forward_control_back_to_shared_plugins() {
        let (engine, recorder) = runtime_with_recorder("http://127.0.0.1:1/v1".to_string());
        let react = test_react_engine(engine, Vec::new()).with_agent_id("child-agent".to_string());

        assert!(!react.forward_plugin_runtime_command(&plugin_control("cancel-sibling")));
        assert!(!react.forward_plugin_runtime_command(&approval("sibling-approval", true,)));
        assert!(recorder.commands().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn main_model_stream_forwards_plugin_runtime_commands() {
        let server = MockLlmServer::start(MockResponse::Stall);
        let (engine, recorder) = runtime_with_recorder(server.base_url());
        let mut react = test_react_engine(engine, Vec::new());
        let mut session = Session::new("main-stream-runtime-command");
        let (stream_tx, _stream_rx) = std::sync::mpsc::channel();
        let (command_tx, mut command_rx) = tokio_mpsc::unbounded_channel();

        let execution = tokio::spawn(async move {
            react
                .execute_turn(&mut session, None, &stream_tx, &mut command_rx)
                .await;
        });
        server.wait_until_connected().await;
        command_tx.send(plugin_control("cancel-child")).unwrap();
        command_tx.send(approval("child-approval", true)).unwrap();
        command_tx.send(Command::Cancel).unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(3), execution)
            .await
            .expect("主模型流式执行未及时退出")
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
                    approved: true,
                },
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn approval_wait_forwards_unmatched_plugin_runtime_commands() {
        crate::storage::set_storage_root(
            std::env::temp_dir().join("tiangong-core-react-command-tests"),
        );
        let server = MockLlmServer::start(MockResponse::ToolCall {
            name: "approval_test_tool".to_string(),
        });
        let (engine, recorder) = runtime_with_recorder(server.base_url());
        let tool = ToolSpec {
            name: "approval_test_tool".to_string(),
            description: "测试审批等待".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        };
        let mut react = test_react_engine(engine, vec![tool]);
        let mut session = Session::new("approval-wait-runtime-command");
        let (stream_tx, stream_rx) = std::sync::mpsc::channel();
        let (command_tx, mut command_rx) = tokio_mpsc::unbounded_channel();

        let execution = tokio::spawn(async move {
            react
                .execute_turn(&mut session, None, &stream_tx, &mut command_rx)
                .await;
        });
        server.wait_until_connected().await;
        let request_id = tokio::task::spawn_blocking(move || {
            loop {
                match stream_rx.recv_timeout(std::time::Duration::from_secs(3)) {
                    Ok(StreamEvent::ApprovalNeeded { request_id, .. }) => break request_id,
                    Ok(_) => {}
                    Err(error) => panic!("未收到主 Agent 审批请求：{error}"),
                }
            }
        })
        .await
        .unwrap();

        command_tx.send(approval("child-approval", true)).unwrap();
        command_tx.send(plugin_control("cancel-child")).unwrap();
        command_tx.send(approval(&request_id, false)).unwrap();
        command_tx.send(Command::Cancel).unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(3), execution)
            .await
            .expect("审批等待执行未及时退出")
            .unwrap();
        assert_eq!(
            recorder.commands(),
            vec![
                RecordedRuntimeCommand::Approval {
                    request_id: "child-approval".to_string(),
                    approved: true,
                },
                RecordedRuntimeCommand::PluginControl {
                    plugin_id: "test-plugin".to_string(),
                    action: "cancel-child".to_string(),
                },
            ],
            "主 Agent 自己的审批响应必须继续由当前审批等待分支消费"
        );
    }

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
