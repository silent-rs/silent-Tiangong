//! EventLoopRunner：事件驱动循环执行器
//!
//! 替代 TurnRunner，以事件驱动方式处理用户消息、工具结果等。
//! 无事件时自动挂起，有事件时被唤起继续执行。

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use anyhow::Result;

use crate::agents::execution_mcp_agent::{McpFunctionTarget, execution_function_tools};
use crate::app_state::TurnEvent;
use crate::context::assembler::ContextAssembler;
use crate::context::compressor::compress_loop_messages;
use crate::context::organizer::ContextOrganizer;
use crate::model::{
    FunctionToolSpec, ModelClient, ModelFunctionCall, ModelRequest, ModelStreamChunk, TokenUsage,
};
use crate::runtime::{
    LlmOutputRecord, RuntimeEngine, build_react_system_prompt, inject_enhanced_tools,
    strip_tool_traces_from_response, use_stream_mode,
};
use crate::session::{Message, MessageRole, Session, now_text};
use super::context::events_to_messages;
use super::types::*;

const MAX_ROUNDS: usize = 20;

/// 事件驱动循环执行器
pub struct EventLoopRunner {
    // 固定配置
    engine: RuntimeEngine,
    session: Session,
    output_tx: Sender<TurnEvent>,
    event_rx: Receiver<LoopEvent>,

    // 运行状态
    state: LoopState,
    phase: LoopPhase,

    // 初始化后的缓存（可重建）
    context: Vec<Message>,
    function_tools: Vec<FunctionToolSpec>,
    mcp_targets: HashMap<String, McpFunctionTarget>,
    system_prompt: String,
    organizer: ContextOrganizer,
    session_cwd: Option<std::path::PathBuf>,

    // 当前轮的临时状态
    pending_tool_calls: Vec<ModelFunctionCall>,
    accumulated_usage: TokenUsage,
}

impl EventLoopRunner {
    /// 创建新的 EventLoopRunner
    pub fn new(
        engine: RuntimeEngine,
        session: Session,
        output_tx: Sender<TurnEvent>,
        event_rx: Receiver<LoopEvent>,
    ) -> Self {
        let session_id = session.id.clone();
        let context_limit = engine.context_limit;
        Self {
            engine,
            session,
            output_tx,
            event_rx,
            state: LoopState::new(session_id),
            phase: LoopPhase::Idle,
            context: Vec::new(),
            function_tools: Vec::new(),
            mcp_targets: HashMap::new(),
            system_prompt: String::new(),
            organizer: ContextOrganizer::new(context_limit).with_keep_recent_turns(6),
            session_cwd: None,
            pending_tool_calls: Vec::new(),
            accumulated_usage: TokenUsage::default(),
        }
    }

    /// 从挂起状态恢复
    pub fn resume(
        engine: RuntimeEngine,
        session: Session,
        state: LoopState,
        output_tx: Sender<TurnEvent>,
        event_rx: Receiver<LoopEvent>,
    ) -> Self {
        let context_limit = engine.context_limit;
        Self {
            engine,
            session,
            output_tx,
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
        }
    }

    /// 主循环入口
    ///
    /// 执行事件循环，直到挂起、取消或收到关闭信号。
    /// 返回 LoopOutcome 供外部决定如何处理状态。
    pub fn run(mut self) -> LoopOutcome {
        // 初始化工具和上下文
        if let Err(err) = self.init_tools() {
            return LoopOutcome::Error(self.state, err.to_string());
        }

        loop {
            // 1. 收集待处理事件
            let events = self.collect_events();

            // 2. 无事件且无待处理工作 → 挂起
            if events.is_empty() && self.pending_tool_calls.is_empty() {
                self.state.interrupted_phase = LoopPhase::Idle;
                self.state.mark_suspended();
                return LoopOutcome::Suspended(self.state);
            }

            // 3. 处理控制事件（Cancel / Shutdown）
            for event in &events {
                match event {
                    LoopEvent::Cancel => {
                        self.state.interrupted_phase = LoopPhase::Cancelled;
                        return LoopOutcome::Cancelled(self.state);
                    }
                    LoopEvent::SystemSignal(SystemSignalKind::Shutdown) => {
                        self.state.interrupted_phase = self.phase.clone();
                        self.state.pending_tool_calls = self
                            .pending_tool_calls
                            .iter()
                            .map(PendingToolCall::from)
                            .collect();
                        return LoopOutcome::Shutdown(self.state);
                    }
                    _ => {}
                }
            }

            // 4. 将事件注入上下文
            let new_messages = events_to_messages(&events);
            self.state.loop_context.extend(new_messages);

            // 5. 处理权限响应
            for event in &events {
                if let LoopEvent::PermissionResponse {
                    approved,
                    ..
                } = event
                {
                    if *approved {
                        // 审批通过，继续执行待处理的工具调用
                        self.phase = LoopPhase::Processing;
                    } else {
                        // 审批拒绝，清空待处理工具调用
                        self.pending_tool_calls.clear();
                    }
                }
            }

            // 6. 如果有待执行的工具调用且不在等待审批，先执行
            if !self.pending_tool_calls.is_empty()
                && !matches!(self.phase, LoopPhase::WaitingApproval { .. })
            {
                self.execute_pending_tools();
                // 工具结果已注入 state.loop_context，继续循环
                continue;
            }

            // 7. 组织上下文 + LLM 调用
            self.phase = LoopPhase::Processing;
            match self.call_llm() {
                Ok(true) => {
                    // LLM 返回文本回复（满足），回到循环顶部检查后续事件
                    self.phase = LoopPhase::Idle;
                }
                Ok(false) => {
                    // LLM 返回工具调用（不满足），工具调用在 pending_tool_calls
                    // 下一轮循环会执行它们
                }
                Err(err) => {
                    let _ = self.output_tx.send(TurnEvent::Failed(err.to_string()));
                    return LoopOutcome::Error(self.state, err.to_string());
                }
            }
        }
    }

    /// 初始化工具定义和系统 prompt
    fn init_tools(&mut self) -> Result<()> {
        // 设置会话级工作目录
        self.session_cwd = if self.session.cwd.is_empty() {
            None
        } else {
            let p = std::path::PathBuf::from(&self.session.cwd);
            if p.is_dir() { Some(p) } else { None }
        };
        crate::tool::set_session_cwd(self.session_cwd.clone());

        // 工具定义
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

        // 上下文
        let assembler = ContextAssembler::new(self.engine.context_limit);
        self.context = assembler.organizer().build_context(&self.session);

        Ok(())
    }

    /// 收集所有待处理事件（非阻塞）
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

    /// 调用 LLM
    ///
    /// 返回 Ok(true) 表示满足（文本回复），Ok(false) 表示不满足（工具调用）。
    fn call_llm(&mut self) -> Result<bool> {
        // 构建 system prompt（每轮重建，确保包含最新用户输入）
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

        let req = ModelRequest {
            session_title: self.session.title.clone(),
            user_input: if self.state.round == 0 {
                self.system_prompt.clone()
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

        // 流式回调：推送 StageThinking
        let tx = self.output_tx.clone();
        let round = self.state.round;
        let response = self.engine.client().complete_with_functions_stream(
            &req,
            &self.function_tools,
            &mut |delta: &ModelStreamChunk| {
                let _ = tx.send(TurnEvent::StageThinking {
                    stage: format!("react-round-{}", round + 1),
                    delta: delta.clone(),
                });
            },
        )?;

        self.accumulated_usage.accumulate(&response.usage);
        self.state.accumulated_usage.accumulate(&response.usage);

        if response.tool_calls.is_empty() {
            // 满足：文本回复
            let final_text = response.text.clone();
            let final_reasoning = response.reasoning_content.clone();

            // 发送 LlmOutput
            let output = LlmOutputRecord {
                stage: format!("react-round-{}", self.state.round + 1),
                content: final_text.clone(),
                reasoning_content: final_reasoning.clone(),
                tool_calls: Vec::new(),
                usage: response.usage.clone(),
            };
            let _ = self.output_tx.send(TurnEvent::LlmOutput(output));

            // 发送 reasoning chunk
            if !final_reasoning.is_empty() {
                let _ = self.output_tx.send(TurnEvent::Chunk(ModelStreamChunk {
                    content: String::new(),
                    reasoning_content: final_reasoning,
                }));
            }

            // 发送 content chunks
            let cleaned = strip_tool_traces_from_response(&final_text);
            if !cleaned.is_empty() {
                let _ = self.output_tx.send(TurnEvent::Chunk(ModelStreamChunk {
                    content: cleaned,
                    reasoning_content: String::new(),
                }));
            }

            self.state.round += 1;
            Ok(true)
        } else {
            // 不满足：工具调用
            let tool_call_names: Vec<String> =
                response.tool_calls.iter().map(|tc| tc.name.clone()).collect();

            let output = LlmOutputRecord {
                stage: format!("react-round-{}", self.state.round + 1),
                content: response.text.clone(),
                reasoning_content: response.reasoning_content.clone(),
                tool_calls: tool_call_names.clone(),
                usage: response.usage.clone(),
            };
            let _ = self.output_tx.send(TurnEvent::LlmOutput(output));

            // 记录 assistant 工具调用意图到 loop_context
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

            // 轮次限制
            if self.state.round >= MAX_ROUNDS {
                // 超限，强制回复
                self.force_final_response()?;
                return Ok(true);
            }

            Ok(false)
        }
    }

    /// 执行待处理的工具调用
    fn execute_pending_tools(&mut self) {
        use crate::permission::PermissionDecision;

        let pending = std::mem::take(&mut self.pending_tool_calls);

        for call in &pending {
            // 权限检查
            match self.engine.permission_gate().check(&call.name) {
                PermissionDecision::Approved => {}
                PermissionDecision::Denied { reason } => {
                    self.state.loop_context.push(Message {
                        id: scru128::new().to_string(),
                        role: MessageRole::System,
                        content: format!("权限拒绝工具 {}：{}", call.name, reason),
                        reasoning_content: String::new(),
                        worker_id: None,
                        created_at: now_text(),
                    });
                    continue;
                }
                PermissionDecision::NeedsApproval { request_id } => {
                    // 发送审批请求
                    let args_summary = serde_json::to_string(&call.arguments)
                        .unwrap_or_default()
                        .chars()
                        .take(200)
                        .collect::<String>();
                    let _ = self.output_tx.send(TurnEvent::ApprovalRequest {
                        request_id: request_id.clone(),
                        tool_name: call.name.clone(),
                        tool_args_summary: args_summary,
                    });
                    // 保留剩余工具调用，等待审批
                    self.pending_tool_calls = pending.clone();
                    self.phase = LoopPhase::WaitingApproval {
                        request_id,
                        tool_name: call.name.clone(),
                    };
                    return;
                }
            }

            // 执行工具
            let _ = self.output_tx.send(TurnEvent::ToolStarted {
                name: call.name.clone(),
                summary: String::new(),
            });

            let result = self.engine.execute_tool_call(
                call,
                &self.mcp_targets,
                &self.engine.agent_config().mcp,
            );

            let _ = self.output_tx.send(TurnEvent::ToolExecution(result.clone()));

            // 工具结果注入上下文
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

        // 上下文压缩检查
        let prompt_tokens = self.accumulated_usage.prompt_tokens;
        if self.organizer.needs_compression(prompt_tokens)
            && let Ok(compressed) =
                compress_loop_messages(&self.state.loop_context, 3, self.engine.client())
        {
            self.state.loop_context = compressed;
        }
    }

    /// 超限时强制生成最终回复
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
        let tx = self.output_tx.clone();
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
