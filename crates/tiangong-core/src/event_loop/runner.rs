//! EventLoopRunner：事件驱动循环执行器
//!
//! core 内部完成完整处理链路：
//! 事件输入 → 组织上下文 → LLM 调用 → 工具执行 → session 更新
//!
//! 输出两条通道：
//! - LoopOutput 回调：简化通知（CLI 直接打印）
//! - TurnEvent channel：完整事件流（GUI poll 兼容）

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use anyhow::Result;

use super::context::events_to_messages;
use super::output::LoopOutput;
use super::types::*;
use crate::agents::execution_mcp_agent::{McpFunctionTarget, execution_function_tools};
use crate::app_state::TurnEvent;
use crate::context::assembler::ContextAssembler;
use crate::context::compressor::compress_loop_messages;
use crate::context::organizer::ContextOrganizer;
use crate::model::{
    FunctionToolSpec, ModelClient, ModelFunctionCall, ModelRequest, ModelStreamChunk, TokenUsage,
};
use crate::observe::ObserveCollector;
use crate::runtime::{
    LlmOutputRecord, RuntimeEngine, build_react_system_prompt, inject_enhanced_tools,
    use_stream_mode,
};
use crate::session::{Message, MessageRole, Session, now_text};

const MAX_ROUNDS: usize = 20;

/// 事件驱动循环执行器
pub struct EventLoopRunner {
    engine: RuntimeEngine,
    session: Session,
    /// 简化回调（CLI 直接打印）
    output: Box<dyn LoopOutput>,
    /// 完整事件流（GUI poll 兼容）
    event_tx: Option<Sender<TurnEvent>>,
    event_rx: Receiver<LoopEvent>,

    state: LoopState,
    phase: LoopPhase,

    context: Vec<Message>,
    function_tools: Vec<FunctionToolSpec>,
    mcp_targets: HashMap<String, McpFunctionTarget>,
    system_prompt: String,
    organizer: ContextOrganizer,
    session_cwd: Option<std::path::PathBuf>,

    pending_tool_calls: Vec<ModelFunctionCall>,
    accumulated_usage: TokenUsage,
    needs_llm_followup: bool,
    observer: ObserveCollector,
}

impl EventLoopRunner {
    pub fn new(
        engine: RuntimeEngine,
        session: Session,
        output: Box<dyn LoopOutput>,
        event_tx: Option<Sender<TurnEvent>>,
        event_rx: Receiver<LoopEvent>,
    ) -> Self {
        let session_id = session.id.clone();
        let context_limit = engine.context_limit;
        Self {
            engine,
            session,
            output,
            event_tx,
            event_rx,
            state: LoopState::new(session_id.clone()),
            phase: LoopPhase::Idle,
            context: Vec::new(),
            function_tools: Vec::new(),
            mcp_targets: HashMap::new(),
            system_prompt: String::new(),
            organizer: ContextOrganizer::new(context_limit).with_keep_recent_turns(6),
            session_cwd: None,
            pending_tool_calls: Vec::new(),
            accumulated_usage: TokenUsage::default(),
            needs_llm_followup: false,
            observer: ObserveCollector::new().with_session(session_id),
        }
    }

    pub fn resume(
        engine: RuntimeEngine,
        session: Session,
        state: LoopState,
        output: Box<dyn LoopOutput>,
        event_tx: Option<Sender<TurnEvent>>,
        event_rx: Receiver<LoopEvent>,
    ) -> Self {
        let context_limit = engine.context_limit;
        let session_id = session.id.clone();
        Self {
            engine,
            session,
            output,
            event_tx,
            event_rx,
            state,
            phase: LoopPhase::Idle,
            context: Vec::new(),
            function_tools: Vec::new(),
            mcp_targets: HashMap::new(),
            system_prompt: String::new(),
            organizer: ContextOrganizer::new(context_limit).with_keep_recent_turns(6),
            session_cwd: None,
            pending_tool_calls: Vec::new(),
            accumulated_usage: TokenUsage::default(),
            needs_llm_followup: false,
            observer: ObserveCollector::new().with_session(session_id),
        }
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// 发送 TurnEvent 到 channel（如果有）
    fn emit(&self, event: TurnEvent) {
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(event);
        }
    }

    pub fn run(mut self) -> (LoopOutcome, Session) {
        if let Err(err) = self.init_tools() {
            self.output.on_error(&err.to_string());
            self.emit(TurnEvent::Failed(err.to_string()));
            let session = self.session.clone();
            return (LoopOutcome::Error(self.state, err.to_string()), session);
        }

        let outcome = self.run_loop();
        let session = self.session.clone();
        (outcome, session)
    }

    fn run_loop(&mut self) -> LoopOutcome {
        loop {
            let events = self.collect_events();

            if events.is_empty() && self.pending_tool_calls.is_empty() && !self.needs_llm_followup
            {
                self.state.interrupted_phase = LoopPhase::Idle;
                self.state.mark_suspended();
                self.output.on_round_complete();
                return LoopOutcome::Suspended(self.state.clone());
            }

            for event in &events {
                match event {
                    LoopEvent::Cancel => {
                        self.state.interrupted_phase = LoopPhase::Cancelled;
                        return LoopOutcome::Cancelled(self.state.clone());
                    }
                    LoopEvent::SystemSignal(SystemSignalKind::Shutdown) => {
                        self.state.interrupted_phase = self.phase.clone();
                        self.state.pending_tool_calls = self
                            .pending_tool_calls
                            .iter()
                            .map(PendingToolCall::from)
                            .collect();
                        return LoopOutcome::Shutdown(self.state.clone());
                    }
                    _ => {}
                }
            }

            // 事件注入 session 和 loop_context
            let new_messages = events_to_messages(&events);
            for msg in &new_messages {
                self.state.loop_context.push(msg.clone());
            }

            for event in &events {
                if let LoopEvent::PermissionResponse { approved, .. } = event {
                    if *approved {
                        self.phase = LoopPhase::Processing;
                    } else {
                        self.pending_tool_calls.clear();
                    }
                }
            }

            if !self.pending_tool_calls.is_empty()
                && !matches!(self.phase, LoopPhase::WaitingApproval { .. })
            {
                self.execute_pending_tools();
                continue;
            }

            self.needs_llm_followup = false;
            self.phase = LoopPhase::Processing;
            match self.call_llm() {
                Ok(true) => {
                    self.phase = LoopPhase::Idle;
                }
                Ok(false) => {}
                Err(err) => {
                    self.output.on_error(&err.to_string());
                    self.emit(TurnEvent::Failed(err.to_string()));
                    return LoopOutcome::Error(self.state.clone(), err.to_string());
                }
            }
        }
    }

    fn init_tools(&mut self) -> Result<()> {
        self.session_cwd = if self.session.cwd.is_empty() {
            None
        } else {
            let p = std::path::PathBuf::from(&self.session.cwd);
            if p.is_dir() { Some(p) } else { None }
        };
        crate::tool::set_session_cwd(self.session_cwd.clone());

        let (all_tools, mcp_targets) = execution_function_tools(&self.engine.agent_config().mcp);
        let mut all_tools: Vec<FunctionToolSpec> = all_tools
            .into_iter()
            .filter(|t| t.name != "mark_step_completed")
            .collect();
        inject_enhanced_tools(
            &mut all_tools,
            self.engine.models_config(),
            self.engine.agent_config(),
        );
        self.function_tools = all_tools;
        self.mcp_targets = mcp_targets;

        let assembler = ContextAssembler::new(self.engine.context_limit);
        self.context = assembler.organizer().build_context(&self.session);
        Ok(())
    }

    fn collect_events(&self) -> Vec<LoopEvent> {
        let mut events = Vec::new();
        loop {
            match self.event_rx.try_recv() {
                Ok(event) => events.push(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        events
    }

    fn call_llm(&mut self) -> Result<bool> {
        let last_user_input = self
            .state
            .loop_context
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::User)
            .map(|m| m.content.as_str())
            .unwrap_or("");

        self.system_prompt = build_react_system_prompt(
            last_user_input,
            self.engine.models_config(),
            self.engine.agent_config(),
        );

        let memory_store = crate::context::memory::MemoryStore::load_from_disk();
        let memory_context = memory_store.format_for_context(&self.session.id);

        let req = ModelRequest {
            session_title: self.session.title.clone(),
            user_input: if self.state.round == 0 {
                match memory_context {
                    Some(mem) => format!("{}\n\n{}", mem, self.system_prompt),
                    None => self.system_prompt.clone(),
                }
            } else {
                "根据上面的工具执行结果继续处理。如果已经收集到足够信息，直接给出最终回复，不要再调用工具。"
                    .to_string()
            },
            context: {
                let mut ctx = self.context.clone();
                ctx.extend(self.state.loop_context.clone());
                ctx
            },
        };

        // 创建 assistant 消息接收流式内容
        self.session
            .append_message(MessageRole::Assistant, String::new());
        let assistant_msg_idx = self.session.messages.len() - 1;

        // 流式回调：更新 session + 通知外部 + 发送 TurnEvent
        let output_ref = &self.output;
        let event_tx_ref = &self.event_tx;
        let messages = &mut self.session.messages;

        let response = self.engine.client().complete_with_functions_stream(
            &req,
            &self.function_tools,
            &mut |delta: &ModelStreamChunk| {
                // 更新 session assistant 消息
                if let Some(msg) = messages.get_mut(assistant_msg_idx) {
                    msg.content.push_str(&delta.content);
                    msg.reasoning_content.push_str(&delta.reasoning_content);
                }
                // 通知外部（CLI 打印）
                if !delta.content.is_empty() {
                    output_ref.on_delta(&delta.content);
                }
                if !delta.reasoning_content.is_empty() {
                    output_ref.on_reasoning_delta(&delta.reasoning_content);
                }
                // 发送 TurnEvent（GUI poll）
                if let Some(tx) = event_tx_ref {
                    let _ = tx.send(TurnEvent::Chunk(delta.clone()));
                }
            },
        )?;

        self.accumulated_usage.accumulate(&response.usage);
        self.state.accumulated_usage.accumulate(&response.usage);
        self.observer
            .record_llm_call(scru128::new().to_string(), &response.usage);
        self.output.on_llm_complete(&response.usage);

        if response.tool_calls.is_empty() {
            // 满足：最终回复（session 已通过流式 callback 更新完毕）
            let output = LlmOutputRecord {
                stage: format!("react-round-{}", self.state.round + 1),
                content: response.text.clone(),
                reasoning_content: response.reasoning_content.clone(),
                tool_calls: Vec::new(),
                usage: response.usage.clone(),
            };
            self.emit(TurnEvent::LlmOutput(output));
            self.state.round += 1;
            Ok(true)
        } else {
            // 不满足：工具调用
            // assistant 消息中的流式中间内容保留（推理过程）

            let tool_call_names: Vec<String> =
                response.tool_calls.iter().map(|tc| tc.name.clone()).collect();

            let output = LlmOutputRecord {
                stage: format!("react-round-{}", self.state.round + 1),
                content: response.text.clone(),
                reasoning_content: response.reasoning_content.clone(),
                tool_calls: tool_call_names.clone(),
                usage: response.usage.clone(),
            };
            self.emit(TurnEvent::LlmOutput(output));

            let assistant_text = if response.text.is_empty() {
                format!("[调用工具: {}]", tool_call_names.join(", "))
            } else {
                response.text.clone()
            };
            self.state.loop_context.push(Message {
                id: scru128::new().to_string(),
                role: MessageRole::Assistant,
                content: assistant_text,
                reasoning_content: response.reasoning_content.clone(),
                worker_id: None,
                created_at: now_text(),
            });

            self.pending_tool_calls = response.tool_calls;
            self.state.round += 1;

            if self.state.round >= MAX_ROUNDS {
                self.force_final_response()?;
                return Ok(true);
            }

            Ok(false)
        }
    }

    fn execute_pending_tools(&mut self) {
        use crate::permission::PermissionDecision;

        let pending = std::mem::take(&mut self.pending_tool_calls);

        for call in &pending {
            match self.engine.permission_gate().check(&call.name) {
                PermissionDecision::Approved => {}
                PermissionDecision::Denied { reason } => {
                    let msg = format!("权限拒绝工具 {}：{}", call.name, reason);
                    self.state.loop_context.push(Message {
                        id: scru128::new().to_string(),
                        role: MessageRole::System,
                        content: msg.clone(),
                        reasoning_content: String::new(),
                        worker_id: None,
                        created_at: now_text(),
                    });
                    self.session.append_message(MessageRole::System, msg);
                    continue;
                }
                PermissionDecision::NeedsApproval { request_id } => {
                    let args_summary = serde_json::to_string(&call.arguments)
                        .unwrap_or_default()
                        .chars()
                        .take(200)
                        .collect::<String>();
                    self.output
                        .on_approval_request(&request_id, &call.name, &args_summary);
                    self.emit(TurnEvent::ApprovalRequest {
                        request_id: request_id.clone(),
                        tool_name: call.name.clone(),
                        tool_args_summary: args_summary,
                    });
                    self.pending_tool_calls = pending.clone();
                    self.phase = LoopPhase::WaitingApproval {
                        request_id,
                        tool_name: call.name.clone(),
                    };
                    return;
                }
            }

            self.output.on_tool_start(&call.name, "");
            self.emit(TurnEvent::ToolStarted {
                name: call.name.clone(),
                summary: String::new(),
            });

            let result = self.engine.execute_tool_call(
                call,
                &self.mcp_targets,
                &self.engine.agent_config().mcp,
            );

            self.output.on_tool_result(&call.name, &result);
            self.emit(TurnEvent::ToolExecution(result.clone()));

            let feedback = format!(
                "工具 {} 执行{}：{}",
                call.name,
                if result.ok { "成功" } else { "失败" },
                if result.stdout.chars().count() > 2000 {
                    let truncated: String = result.stdout.chars().take(2000).collect();
                    format!("{truncated}...(截断)")
                } else if result.stdout.is_empty() {
                    result.summary.clone()
                } else {
                    result.stdout.clone()
                }
            );
            self.state.loop_context.push(Message {
                id: scru128::new().to_string(),
                role: MessageRole::System,
                content: feedback.clone(),
                reasoning_content: String::new(),
                worker_id: None,
                created_at: now_text(),
            });
            self.session.append_message(MessageRole::System, feedback);
        }

        let prompt_tokens = self.accumulated_usage.prompt_tokens;
        if self.organizer.needs_compression(prompt_tokens)
            && let Ok(compressed) =
                compress_loop_messages(&self.state.loop_context, 3, self.engine.client())
        {
            self.state.loop_context = compressed;
        }

        self.needs_llm_followup = true;
    }

    fn force_final_response(&mut self) -> Result<()> {
        let req = ModelRequest {
            session_title: self.session.title.clone(),
            user_input: "请基于以上所有工具执行结果，直接给出最终回复。".to_string(),
            context: {
                let mut ctx = self.context.clone();
                ctx.extend(self.state.loop_context.clone());
                ctx
            },
        };

        self.session
            .append_message(MessageRole::Assistant, String::new());
        let idx = self.session.messages.len() - 1;

        let output_ref = &self.output;
        let event_tx_ref = &self.event_tx;
        let messages = &mut self.session.messages;

        let resp = if use_stream_mode() {
            self.engine
                .client()
                .complete_stream_with_callback(&req, |delta| {
                    if let Some(msg) = messages.get_mut(idx) {
                        msg.content.push_str(&delta.content);
                        msg.reasoning_content.push_str(&delta.reasoning_content);
                    }
                    if !delta.content.is_empty() {
                        output_ref.on_delta(&delta.content);
                    }
                    if !delta.reasoning_content.is_empty() {
                        output_ref.on_reasoning_delta(&delta.reasoning_content);
                    }
                    if let Some(tx) = event_tx_ref {
                        let _ = tx.send(TurnEvent::Chunk(delta.clone()));
                    }
                })?
        } else {
            let r = self.engine.client().complete(&req)?;
            if let Some(msg) = messages.get_mut(idx) {
                msg.content = r.text.clone();
                msg.reasoning_content = r.reasoning_content.clone();
            }
            output_ref.on_delta(&r.text);
            if let Some(tx) = event_tx_ref {
                let _ = tx.send(TurnEvent::Chunk(ModelStreamChunk {
                    content: r.text.clone(),
                    reasoning_content: r.reasoning_content.clone(),
                }));
            }
            r
        };
        self.accumulated_usage.accumulate(&resp.usage);
        Ok(())
    }
}
