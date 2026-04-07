//! EventLoopRunner：事件驱动循环执行器
//!
//! 职责：事件输入 → 组织上下文 → LLM 调用 → 工具执行 → 通过 TurnEvent channel 输出
//!
//! 不更新 session。session 更新由消费方（poll）完成。
//! CLI / GUI / Connector 统一消费 TurnEvent channel。

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use anyhow::Result;

use super::context::events_to_messages;
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
use crate::prompt::PromptAssembler;
use crate::runtime::{LlmOutputRecord, RuntimeEngine, inject_enhanced_tools, use_stream_mode};
use crate::session::{Message, MessageRole, Session, now_text};

const MAX_ROUNDS: usize = 20;

/// 事件驱动循环执行器
///
/// 唯一输出通道：`Sender<TurnEvent>`
/// CLI / GUI / Connector 统一消费此 channel。
pub struct EventLoopRunner {
    engine: RuntimeEngine,
    session: Session,
    /// 唯一输出通道
    tx: Sender<TurnEvent>,
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
        tx: Sender<TurnEvent>,
        event_rx: Receiver<LoopEvent>,
    ) -> Self {
        let session_id = session.id.clone();
        let context_limit = engine.context_limit;
        Self {
            engine,
            session,
            tx,
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
        tx: Sender<TurnEvent>,
        event_rx: Receiver<LoopEvent>,
    ) -> Self {
        let context_limit = engine.context_limit;
        let session_id = session.id.clone();
        Self {
            engine,
            session,
            tx,
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

    fn emit(&self, event: TurnEvent) {
        let _ = self.tx.send(event);
    }

    pub fn run(mut self) -> LoopOutcome {
        if let Err(err) = self.init_tools() {
            self.emit(TurnEvent::Failed(err.to_string()));
            return LoopOutcome::Error(self.state, err.to_string());
        }
        self.run_loop()
    }

    fn run_loop(&mut self) -> LoopOutcome {
        loop {
            let events = self.collect_events();

            if events.is_empty() && self.pending_tool_calls.is_empty() && !self.needs_llm_followup
            {
                self.state.interrupted_phase = LoopPhase::Idle;
                self.state.mark_suspended();
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

            // 事件注入 loop_context（runner 自用上下文）
            let new_messages = events_to_messages(&events);
            self.state.loop_context.extend(new_messages);

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

        // 使用 PromptAssembler 构建分层 prompt
        let assembler = PromptAssembler::new(self.engine.context_limit);
        let user_input = if self.state.round == 0 {
            last_user_input.to_string()
        } else {
            "根据上面的工具执行结果继续处理。如果已经收集到足够信息，直接给出最终回复，不要再调用工具。"
                .to_string()
        };
        let assembled = assembler.assemble(
            &self.session,
            &user_input,
            self.function_tools.clone(),
            self.engine.models_config(),
            self.engine.agent_config(),
            &self.state.loop_context,
        );

        // 构建 ModelRequest
        // 使用 assembled_system_prompt 标记：已由 PromptAssembler 完成装配
        // build_openai_messages 检测到此标记后跳过自己的环境注入
        self.system_prompt = assembled.final_system_prompt();
        let req = ModelRequest {
            session_title: self.session.title.clone(),
            user_input: assembled.user_input.clone(),
            context: assembled.build_messages(),
            assembled_system_prompt: Some(self.system_prompt.clone()),
        };

        // 流式回调：发 Chunk 实时写入 assistant 消息
        // 如果最终有工具调用，这个 assistant 消息保留为中间推理过程
        // 下一轮创建新 assistant 消息
        let tx = self.tx.clone();
        let response = self.engine.client().complete_with_functions_stream(
            &req,
            &self.function_tools,
            &mut |delta: &ModelStreamChunk| {
                let _ = tx.send(TurnEvent::Chunk(delta.clone()));
            },
        )?;

        self.accumulated_usage.accumulate(&response.usage);
        self.state.accumulated_usage.accumulate(&response.usage);
        self.observer
            .record_llm_call(scru128::new().to_string(), &response.usage);

        if response.tool_calls.is_empty() {
            // 满足：最终回复（已通过 Chunk callback 流式写入 assistant 消息）
            self.emit(TurnEvent::LlmOutput(LlmOutputRecord {
                stage: format!("react-round-{}", self.state.round + 1),
                content: String::new(),
                reasoning_content: String::new(),
                tool_calls: Vec::new(),
                usage: response.usage.clone(),
            }));
            self.state.round += 1;
            Ok(true)
        } else {
            // 不满足：工具调用
            let tool_call_names: Vec<String> =
                response.tool_calls.iter().map(|tc| tc.name.clone()).collect();

            self.emit(TurnEvent::LlmOutput(LlmOutputRecord {
                stage: format!("react-round-{}", self.state.round + 1),
                content: response.text.clone(),
                reasoning_content: response.reasoning_content.clone(),
                tool_calls: tool_call_names.clone(),
                usage: response.usage.clone(),
            }));

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
                        content: msg,
                        reasoning_content: String::new(),
                        worker_id: None,
                        created_at: now_text(),
                    });
                    continue;
                }
                PermissionDecision::NeedsApproval { request_id } => {
                    let args_summary = serde_json::to_string(&call.arguments)
                        .unwrap_or_default()
                        .chars()
                        .take(200)
                        .collect::<String>();
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

            self.emit(TurnEvent::ToolStarted {
                name: call.name.clone(),
                summary: String::new(),
            });

            let result = self.engine.execute_tool_call(
                call,
                &self.mcp_targets,
                &self.engine.agent_config().mcp,
            );

            self.emit(TurnEvent::ToolExecution(result.clone()));

            // 工具结果注入 loop_context（runner 自用）
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
                content: feedback,
                reasoning_content: String::new(),
                worker_id: None,
                created_at: now_text(),
            });
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
            assembled_system_prompt: None,
        };

        let tx = self.tx.clone();
        let resp = if use_stream_mode() {
            self.engine
                .client()
                .complete_stream_with_callback(&req, |delta| {
                    let _ = tx.send(TurnEvent::Chunk(delta.clone()));
                })?
        } else {
            let r = self.engine.client().complete(&req)?;
            if !r.text.is_empty() {
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
