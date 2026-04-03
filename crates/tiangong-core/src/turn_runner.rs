//! 事件驱动状态机
//!
//! `TurnRunner` 以 `loop { match self.phase { ... } }` 驱动执行，
//! 每个状态是独立方法，状态之间可接收控制信号（用户追加消息、取消）。

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};

use anyhow::Result;

use crate::agents::execution_mcp_agent::{McpFunctionTarget, execution_function_tools};
use crate::app_state::{ControlSignal, TurnEvent};
use crate::context::assembler::{ContextAssembler, QueryMode};
use crate::context::compressor::compress_loop_messages;
use crate::context::organizer::ContextOrganizer;
use crate::model::{
    FunctionToolSpec, ModelClient, ModelFunctionCall, ModelRequest, ModelStreamChunk, TokenUsage,
};
use crate::runtime::{
    LlmOutputRecord, RuntimeEngine, TurnExecution,
    build_react_system_prompt, inject_enhanced_tools,
    strip_tool_traces_from_response, use_stream_mode,
};
use crate::session::{Message, MessageRole, Session, now_text};
use crate::tool::ToolResult;
use crate::turn_state::TurnPhase;

const MAX_ROUNDS: usize = 20;

/// 事件驱动状态机 Turn 执行器
pub struct TurnRunner {
    // 固定配置
    engine: RuntimeEngine,
    session: Session,
    user_input: String,
    tx: Sender<TurnEvent>,
    ctrl_rx: Receiver<ControlSignal>,

    // 状态
    phase: TurnPhase,
    cancelled: bool,
    pending_user_messages: Vec<String>,

    // Init 阶段初始化
    context: Vec<Message>,
    function_tools: Vec<FunctionToolSpec>,
    mcp_targets: HashMap<String, McpFunctionTarget>,
    system_prompt: String,
    organizer: Option<ContextOrganizer>,
    session_cwd: Option<std::path::PathBuf>,
    query_mode: QueryMode,
    #[cfg(feature = "llm-debug-log")]
    llm_calls: Vec<crate::session::LlmCallRecord>,

    // 循环累积
    loop_messages: Vec<Message>,
    accumulated_usage: TokenUsage,
    tool_results: Vec<ToolResult>,
    round: usize,
    total_output_chunks: usize,

    // LLM 响应（待处理的工具调用）
    pending_tool_calls: Vec<ModelFunctionCall>,

    // 最终结果
    final_text: String,
    final_reasoning: String,
}

impl TurnRunner {
    pub fn new(
        engine: RuntimeEngine,
        session: Session,
        user_input: String,
        tx: Sender<TurnEvent>,
        ctrl_rx: Receiver<ControlSignal>,
    ) -> Self {
        Self {
            engine,
            session,
            user_input,
            tx,
            ctrl_rx,
            phase: TurnPhase::Init,
            cancelled: false,
            pending_user_messages: Vec::new(),
            context: Vec::new(),
            function_tools: Vec::new(),
            mcp_targets: HashMap::new(),
            system_prompt: String::new(),
            organizer: None,
            session_cwd: None,
            query_mode: QueryMode::ToolExecution,
            #[cfg(feature = "llm-debug-log")]
            llm_calls: Vec::new(),
            loop_messages: Vec::new(),
            accumulated_usage: TokenUsage::default(),
            tool_results: Vec::new(),
            round: 0,
            total_output_chunks: 0,
            pending_tool_calls: Vec::new(),
            final_text: String::new(),
            final_reasoning: String::new(),
        }
    }

    /// 状态机主循环
    pub fn run(mut self) -> Result<TurnExecution> {
        loop {
            // 每次状态转换前检查控制信号
            self.check_control_signals();
            if self.cancelled {
                self.phase = TurnPhase::Cancelled;
            }

            match self.phase.clone() {
                TurnPhase::Init => self.do_init()?,
                TurnPhase::ContextAssembly => self.do_context_assembly(),
                TurnPhase::LlmCalling => self.do_llm_calling()?,
                TurnPhase::ToolDispatching => self.do_tool_dispatching(),
                TurnPhase::WaitingApproval { ref request_id, ref tool_name } => {
                    self.do_waiting_approval(request_id.clone(), tool_name.clone());
                }
                TurnPhase::ToolExecuting => self.do_tool_executing(),
                TurnPhase::ResultProcessing => self.do_result_processing(),
                TurnPhase::Responding => return self.do_responding(),
                TurnPhase::Completed => unreachable!(),
                TurnPhase::Failed { error } => return Err(anyhow::anyhow!(error)),
                TurnPhase::Cancelled => return self.build_cancelled_result(),
            }
        }
    }

    // ===== Init =====
    fn do_init(&mut self) -> Result<()> {
        // 设置会话级工作目录
        self.session_cwd = if self.session.cwd.is_empty() {
            None
        } else {
            let p = std::path::PathBuf::from(&self.session.cwd);
            if p.is_dir() { Some(p) } else { None }
        };
        crate::tool::set_session_cwd(self.session_cwd.clone());

        // 上下文装配
        let assembler = ContextAssembler::new(self.engine.context_limit);
        let (all_tools, mcp_targets) = execution_function_tools(&self.engine.agent_config().mcp);
        let mut all_tools: Vec<FunctionToolSpec> = all_tools
            .into_iter()
            .filter(|t| t.name != "mark_step_completed")
            .collect();
        inject_enhanced_tools(&mut all_tools, self.engine.models_config(), self.engine.agent_config());

        let full_system_prompt = build_react_system_prompt(
            &self.user_input,
            self.engine.models_config(),
            self.engine.agent_config(),
        );

        let assembled = assembler.assemble(
            &self.session,
            &self.user_input,
            all_tools,
            full_system_prompt,
            self.engine.client(),
            self.engine.models_config(),
            self.engine.agent_config(),
        );

        self.context = assembled.messages;
        self.function_tools = assembled.tools;
        self.system_prompt = assembled.system_prompt.clone();
        self.mcp_targets = mcp_targets;
        self.query_mode = assembled.mode;
        self.organizer = Some(
            ContextOrganizer::new(self.engine.context_limit).with_keep_recent_turns(6),
        );

        #[cfg(feature = "llm-debug-log")]
        { self.llm_calls = assembled.llm_calls; }

        // 根据查询模式决定下一步
        if assembled.mode == QueryMode::DirectAnswer {
            // DirectAnswer 直接进入 LlmCalling（无工具）
            self.phase = TurnPhase::LlmCalling;
        } else {
            // ToolExecution 先发 PlanReady
            let plan = crate::planner::TaskPlan {
                id: scru128::new().to_string(),
                objective: self.user_input.chars().take(50).collect::<String>(),
                ..Default::default()
            };
            let _ = self.tx.send(TurnEvent::PlanReady(plan));
            self.phase = TurnPhase::ContextAssembly;
        }
        Ok(())
    }

    // ===== ContextAssembly =====
    fn do_context_assembly(&mut self) {
        // 注入用户追加消息
        self.inject_pending_user_messages();
        self.phase = TurnPhase::LlmCalling;
    }

    // ===== LlmCalling =====
    fn do_llm_calling(&mut self) -> Result<()> {
        if self.query_mode == QueryMode::DirectAnswer {
            return self.do_llm_direct_answer();
        }

        let req = ModelRequest {
            session_title: self.session.title.clone(),
            user_input: if self.round == 0 {
                self.system_prompt.clone()
            } else {
                "根据上面的工具执行结果继续处理。如果已经收集到足够信息，直接给出最终回复，不要再调用工具。".to_string()
            },
            context: {
                let mut ctx = self.context.clone();
                ctx.extend(self.loop_messages.clone());
                ctx
            },
        };

        let response = self.engine.client().complete_with_functions_stream(
            &req,
            &self.function_tools,
            &mut |_delta: &ModelStreamChunk| {},
        )?;

        self.accumulated_usage.accumulate(&response.usage);

        if response.tool_calls.is_empty() {
            // 无工具调用 → 最终回复
            self.final_text = response.text.clone();
            self.final_reasoning = response.reasoning_content.clone();
            if !self.final_text.is_empty() || !self.final_reasoning.is_empty() {
                let _ = self.tx.send(TurnEvent::Chunk(ModelStreamChunk {
                    content: self.final_text.clone(),
                    reasoning_content: self.final_reasoning.clone(),
                }));
                self.total_output_chunks += 1;
            }
            self.phase = TurnPhase::Responding;
        } else {
            // 有工具调用
            let tool_call_names: Vec<String> =
                response.tool_calls.iter().map(|tc| tc.name.clone()).collect();

            // 中间文字通过 StageThinking 推送（不写入 assistant 消息，避免被最终回复覆盖）
            if !response.text.is_empty() {
                let _ = self.tx.send(TurnEvent::StageThinking {
                    stage: format!("react-round-{}", self.round + 1),
                    delta: ModelStreamChunk {
                        content: response.text.clone(),
                        reasoning_content: response.reasoning_content.clone(),
                    },
                });
            }

            let output = LlmOutputRecord {
                stage: format!("react-round-{}", self.round + 1),
                content: response.text.clone(),
                reasoning_content: response.reasoning_content.clone(),
                tool_calls: tool_call_names.clone(),
                usage: response.usage.clone(),
            };
            let _ = self.tx.send(TurnEvent::LlmOutput(output));

            // 记录 assistant 工具调用意图到 loop_messages
            let assistant_text = if response.text.is_empty() {
                format!("[调用工具: {}]", tool_call_names.join(", "))
            } else {
                response.text.clone()
            };
            self.loop_messages.push(Message {
                id: scru128::new().to_string(),
                role: MessageRole::Assistant,
                content: assistant_text,
                reasoning_content: response.reasoning_content.clone(),
                worker_id: None,
                created_at: now_text(),
            });

            self.pending_tool_calls = response.tool_calls;
            self.phase = TurnPhase::ToolDispatching;
        }
        Ok(())
    }

    /// DirectAnswer 快速路径
    fn do_llm_direct_answer(&mut self) -> Result<()> {
        let req = ModelRequest {
            session_title: self.session.title.clone(),
            user_input: self.system_prompt.clone(),
            context: self.context.clone(),
        };
        let tx = self.tx.clone();
        let resp = if use_stream_mode() {
            self.engine.client().complete_stream_with_callback(&req, |delta| {
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

        self.final_text = resp.text;
        self.final_reasoning = resp.reasoning_content;
        self.accumulated_usage = resp.usage;
        self.total_output_chunks = 1;

        #[cfg(feature = "llm-debug-log")]
        self.llm_calls.push(crate::session::LlmCallRecord {
            stage: "direct-answer".to_string(),
            prompt: self.system_prompt.clone(),
            context_count: req.context.len(),
            tool_names: Vec::new(),
            response_text: self.final_text.clone(),
            reasoning_len: self.final_reasoning.len(),
            tool_calls: Vec::new(),
            usage: self.accumulated_usage.clone(),
            timestamp: now_text(),
        });

        self.phase = TurnPhase::Responding;
        Ok(())
    }

    // ===== WaitingApproval =====
    fn do_waiting_approval(&mut self, request_id: String, tool_name: String) {
        // 阻塞等待审批响应（通过 ctrl_rx）
        // 使用 recv_timeout 避免无限阻塞，每秒检查一次取消信号
        loop {
            match self.ctrl_rx.recv_timeout(std::time::Duration::from_secs(1)) {
                Ok(ControlSignal::PermissionResponse { request_id: rid, approved }) => {
                    if rid == request_id {
                        if approved {
                            tracing::info!("工具 {tool_name} 审批通过");
                            self.phase = TurnPhase::ToolExecuting;
                        } else {
                            tracing::info!("工具 {tool_name} 审批拒绝");
                            // 移除被拒绝的工具调用
                            self.pending_tool_calls.retain(|c| c.name != tool_name);
                            self.loop_messages.push(Message {
                                id: scru128::new().to_string(),
                                role: MessageRole::System,
                                content: format!("工具 {tool_name} 被用户拒绝"),
                                reasoning_content: String::new(),
                                worker_id: None,
                                                created_at: now_text(),
                            });
                            if self.pending_tool_calls.is_empty() {
                                self.phase = TurnPhase::ContextAssembly;
                            } else {
                                self.phase = TurnPhase::ToolExecuting;
                            }
                        }
                        return;
                    }
                }
                Ok(ControlSignal::Cancel) => {
                    self.cancelled = true;
                    self.phase = TurnPhase::Cancelled;
                    return;
                }
                Ok(ControlSignal::UserMessage(msg)) => {
                    self.pending_user_messages.push(msg);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // 继续等待
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    // 通道断开，取消
                    self.cancelled = true;
                    self.phase = TurnPhase::Cancelled;
                    return;
                }
            }
        }
    }

    // ===== ToolDispatching =====
    fn do_tool_dispatching(&mut self) {
        use crate::permission::PermissionDecision;

        let mut denied_tools: Vec<String> = Vec::new();

        for call in &self.pending_tool_calls {
            match self.engine.permission_gate().check(&call.name) {
                PermissionDecision::Approved => {}
                PermissionDecision::Denied { reason } => {
                    tracing::warn!("权限拒绝工具 {}: {}", call.name, reason);
                    denied_tools.push(call.name.clone());
                }
                PermissionDecision::NeedsApproval { request_id } => {
                    // 发送审批请求到前端
                    let args_summary = serde_json::to_string(&call.arguments)
                        .unwrap_or_default()
                        .chars()
                        .take(200)
                        .collect::<String>();
                    let _ = self.tx.send(TurnEvent::ApprovalRequest {
                        request_id: request_id.clone(),
                        tool_name: call.name.clone(),
                        tool_args_summary: args_summary,
                    });
                    // 暂停在 WaitingApproval 状态，等待 ControlSignal::PermissionResponse
                    self.phase = TurnPhase::WaitingApproval {
                        tool_name: call.name.clone(),
                        request_id,
                    };
                    return;
                }
            }
        }

        // 移除被拒绝的工具调用
        if !denied_tools.is_empty() {
            self.pending_tool_calls.retain(|c| !denied_tools.contains(&c.name));
            // 将拒绝信息加入 loop_messages 作为反馈
            let denied_msg = denied_tools.iter()
                .map(|n| format!("工具 {n} 被权限策略拒绝"))
                .collect::<Vec<_>>()
                .join("\n");
            self.loop_messages.push(Message {
                id: scru128::new().to_string(),
                role: MessageRole::System,
                content: denied_msg,
                reasoning_content: String::new(),
                worker_id: None,
                created_at: now_text(),
            });
        }

        if self.pending_tool_calls.is_empty() {
            // 所有工具被拒绝，回到 ContextAssembly 让 LLM 知道
            self.phase = TurnPhase::ContextAssembly;
        } else {
            self.phase = TurnPhase::ToolExecuting;
        }
    }

    // ===== ToolExecuting =====
    fn do_tool_executing(&mut self) {
        let pending_calls = std::mem::take(&mut self.pending_tool_calls);
        let call_results = Self::execute_tool_calls_static(
            &self.engine,
            &self.tx,
            &pending_calls,
            &self.mcp_targets,
            &self.session_cwd,
        );

        let mut round_feedback_parts: Vec<String> = Vec::new();
        for (call_name, result) in call_results {
            let _ = self.tx.send(TurnEvent::ToolExecution(result.clone()));
            self.tool_results.push(result.clone());

            let feedback = format!(
                "工具 {} 执行{}：{}",
                call_name,
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
            round_feedback_parts.push(feedback);
        }

        self.loop_messages.push(Message {
            id: scru128::new().to_string(),
            role: MessageRole::System,
            content: round_feedback_parts.join("\n\n"),
            reasoning_content: String::new(),
            worker_id: None,
            created_at: now_text(),
        });

        self.phase = TurnPhase::ResultProcessing;
    }

    // ===== ResultProcessing =====
    fn do_result_processing(&mut self) {
        if let Some(organizer) = &self.organizer {
            let prompt_tokens = self.accumulated_usage.prompt_tokens;
            if organizer.needs_compression(prompt_tokens) {
                match compress_loop_messages(&self.loop_messages, 3, self.engine.client()) {
                    Ok(compressed) => self.loop_messages = compressed,
                    Err(err) => tracing::warn!("loop_messages 压缩失败：{err}"),
                }
            }
        }

        self.round += 1;
        if self.round >= MAX_ROUNDS {
            self.phase = TurnPhase::Responding;
        } else {
            self.phase = TurnPhase::ContextAssembly;
        }
    }

    // ===== Responding =====
    fn do_responding(mut self) -> Result<TurnExecution> {
        // 如果循环结束仍无最终回复，做最后一次无工具 LLM 调用
        if self.final_text.is_empty() && self.query_mode != QueryMode::DirectAnswer {
            let req = ModelRequest {
                session_title: self.session.title.clone(),
                user_input: "请基于以上所有工具执行结果，直接给出最终回复。".to_string(),
                context: {
                    let mut ctx = self.context.clone();
                    ctx.extend(self.loop_messages.clone());
                    ctx
                },
            };
            let tx = self.tx.clone();
            let resp = if use_stream_mode() {
                self.engine.client().complete_stream_with_callback(&req, |delta| {
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
            self.final_text = resp.text;
            self.final_reasoning = resp.reasoning_content;
            self.total_output_chunks += 1;
        }

        self.build_result()
    }

    // ===== 结果构建 =====
    fn build_result(self) -> Result<TurnExecution> {
        let tool_result_summary = if self.tool_results.is_empty() {
            None
        } else {
            Some(format!(
                "{} 次工具调用，{} 成功，{} 失败",
                self.tool_results.len(),
                self.tool_results.iter().filter(|r| r.ok).count(),
                self.tool_results.iter().filter(|r| !r.ok).count(),
            ))
        };
        let tool_execution = self.tool_results.into_iter().filter_map(|r| r.execution).next_back();
        let cleaned_text = strip_tool_traces_from_response(&self.final_text);

        #[cfg(feature = "llm-debug-log")]
        let llm_calls = self.llm_calls;
        #[cfg(not(feature = "llm-debug-log"))]
        let llm_calls = Vec::new();

        Ok(TurnExecution {
            assistant_message: cleaned_text,
            assistant_reasoning_content: self.final_reasoning,
            system_prompt: self.system_prompt,
            plan: crate::planner::TaskPlan {
                id: scru128::new().to_string(),
                objective: self.user_input.chars().take(50).collect::<String>(),
                ..Default::default()
            },
            tool_result_summary,
            tool_execution,
            verify_records: Vec::new(),
            output_mode: "stream".to_string(),
            output_chunk_count: self.total_output_chunks,
            usage: self.accumulated_usage,
            llm_calls,
        })
    }

    fn build_cancelled_result(self) -> Result<TurnExecution> {
        #[cfg(feature = "llm-debug-log")]
        let llm_calls = self.llm_calls;
        #[cfg(not(feature = "llm-debug-log"))]
        let llm_calls = Vec::new();

        Ok(TurnExecution {
            assistant_message: "执行已取消".to_string(),
            assistant_reasoning_content: String::new(),
            system_prompt: self.system_prompt,
            plan: crate::planner::TaskPlan::default(),
            tool_result_summary: None,
            tool_execution: None,
            verify_records: Vec::new(),
            output_mode: "stream".to_string(),
            output_chunk_count: 0,
            usage: self.accumulated_usage,
            llm_calls,
        })
    }

    // ===== 控制信号处理 =====
    fn check_control_signals(&mut self) {
        while let Ok(signal) = self.ctrl_rx.try_recv() {
            match signal {
                ControlSignal::UserMessage(msg) => {
                    tracing::info!(msg_len = msg.len(), "收到用户追加消息");
                    self.pending_user_messages.push(msg);
                }
                ControlSignal::Cancel => {
                    tracing::info!("收到取消信号");
                    self.cancelled = true;
                }
                ControlSignal::PermissionResponse { .. } => {}
            }
        }
    }

    fn inject_pending_user_messages(&mut self) {
        for msg in self.pending_user_messages.drain(..) {
            self.loop_messages.push(Message {
                id: scru128::new().to_string(),
                role: MessageRole::User,
                content: format!("[用户追加指示] {msg}"),
                reasoning_content: String::new(),
                worker_id: None,
                created_at: now_text(),
            });
        }
    }

    // ===== 工具执行（静态方法，避免借用冲突） =====
    fn execute_tool_calls_static(
        engine: &RuntimeEngine,
        tx: &Sender<TurnEvent>,
        tool_calls: &[ModelFunctionCall],
        mcp_targets: &HashMap<String, McpFunctionTarget>,
        session_cwd: &Option<std::path::PathBuf>,
    ) -> Vec<(String, ToolResult)> {
        let mut call_results: Vec<(String, ToolResult)> = Vec::new();
        let mut other_calls: Vec<&ModelFunctionCall> = Vec::new();

        for call in tool_calls {
            if let Some(result) = RuntimeEngine::handle_management_tool(call, tx) {
                call_results.push((call.name.clone(), result));
            } else {
                other_calls.push(call);
            }
        }

        if other_calls.len() == 1 {
            let call = other_calls[0];
            let result = engine.execute_tool_call(call, mcp_targets, &engine.agent_config().mcp);
            call_results.push((call.name.clone(), result));
        } else if !other_calls.is_empty() {
            let other_results: Vec<(String, ToolResult)> = std::thread::scope(|scope| {
                let handles: Vec<_> = other_calls
                    .iter()
                    .map(|call| {
                        let mcp_config = &engine.agent_config().mcp;
                        let name = call.name.clone();
                        let thread_cwd = session_cwd.clone();
                        scope.spawn(move || {
                            crate::tool::set_session_cwd(thread_cwd);
                            let result = engine.execute_tool_call(call, mcp_targets, mcp_config);
                            (name, result)
                        })
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap_or_else(|_| {
                    ("unknown".to_string(), ToolResult {
                        ok: false, summary: "工具执行线程 panic".to_string(),
                        stdout: String::new(), stderr: "thread panicked".to_string(),
                        exit_code: 1, execution: None,
                    })
                })).collect()
            });
            call_results.extend(other_results);
        }

        call_results
    }
}
