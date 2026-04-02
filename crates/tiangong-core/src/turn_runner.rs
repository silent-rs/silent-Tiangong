//! 事件驱动状态机：自驱动 ReAct 循环 + 控制信号检查点
//!
//! `TurnRunner` 将 ReAct 循环从 `RuntimeEngine` 中提取出来，
//! 在每轮之间检查 `ControlSignal`（用户追加消息、取消等），
//! 实现用户与 AI 智能体的协作运行。

use std::sync::mpsc::{Receiver, Sender};

use anyhow::Result;

use crate::app_state::{ControlSignal, TurnEvent};
use crate::context::assembler::{ContextAssembler, QueryMode};
use crate::context::compressor::compress_loop_messages;
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

/// ReAct 循环的最大迭代次数
const MAX_ROUNDS: usize = 20;

/// 状态机驱动的 Turn 执行器
pub struct TurnRunner {
    engine: RuntimeEngine,
    session: Session,
    user_input: String,
    tx: Sender<TurnEvent>,
    ctrl_rx: Receiver<ControlSignal>,
    phase: TurnPhase,
    cancelled: bool,
    pending_user_messages: Vec<String>,
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
        }
    }

    /// 运行状态机直到终止
    pub fn run(mut self) -> Result<TurnExecution> {
        // 设置会话级工作目录
        let session_cwd = if self.session.cwd.is_empty() {
            None
        } else {
            let p = std::path::PathBuf::from(&self.session.cwd);
            if p.is_dir() { Some(p) } else { None }
        };
        crate::tool::set_session_cwd(session_cwd.clone());

        // 上下文装配
        self.phase = TurnPhase::ContextAssembly;
        let assembler = ContextAssembler::new(self.engine.context_limit);

        let (all_tools, mcp_targets) =
            crate::agents::execution_mcp_agent::execution_function_tools(
                &self.engine.agent_config().mcp,
            );
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

        let context = assembled.messages;
        let function_tools = assembled.tools;
        let system_prompt = assembled.system_prompt.clone();
        let organizer = crate::context::organizer::ContextOrganizer::new(self.engine.context_limit)
            .with_keep_recent_turns(6);

        // === DirectAnswer 快速路径 ===
        if assembled.mode == QueryMode::DirectAnswer {
            self.phase = TurnPhase::LlmCalling;
            let req = ModelRequest {
                session_title: self.session.title.clone(),
                user_input: system_prompt.clone(),
                context,
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

            // DirectAnswer 模式不发送 LlmOutput 事件（避免创建多余的系统消息），
            // 回复已通过 Chunk 事件写入 assistant 消息

            let cleaned = strip_tool_traces_from_response(&resp.text);

            #[cfg(feature = "llm-debug-log")]
            let llm_calls = {
                let mut calls = assembled.llm_calls;
                calls.push(crate::session::LlmCallRecord {
                    stage: "direct-answer".to_string(),
                    prompt: system_prompt.clone(),
                    context_count: req.context.len(),
                    tool_names: Vec::new(),
                    response_text: resp.text.clone(),
                    reasoning_len: resp.reasoning_content.len(),
                    tool_calls: Vec::new(),
                    usage: resp.usage.clone(),
                    timestamp: now_text(),
                });
                calls
            };
            #[cfg(not(feature = "llm-debug-log"))]
            let llm_calls = Vec::new();

            self.phase = TurnPhase::Completed;
            return Ok(TurnExecution {
                assistant_message: cleaned,
                assistant_reasoning_content: resp.reasoning_content,
                system_prompt,
                plan: crate::planner::TaskPlan {
                    id: scru128::new().to_string(),
                    objective: self.user_input.chars().take(50).collect::<String>(),
                    ..Default::default()
                },
                tool_result_summary: None,
                tool_execution: None,
                verify_records: Vec::new(),
                output_mode: "stream".to_string(),
                output_chunk_count: 1,
                usage: resp.usage,
                llm_calls,
            });
        }

        // === ReAct 循环 ===
        self.phase = TurnPhase::LlmCalling;
        let plan = crate::planner::TaskPlan {
            id: scru128::new().to_string(),
            objective: self.user_input.chars().take(50).collect::<String>(),
            ..Default::default()
        };
        let _ = self.tx.send(TurnEvent::PlanReady(plan.clone()));

        let mut accumulated_usage = TokenUsage::default();
        let mut tool_results: Vec<ToolResult> = Vec::new();
        let mut loop_messages: Vec<Message> = Vec::new();
        let mut final_text = String::new();
        let mut final_reasoning = String::new();
        let mut total_output_chunks = 0usize;

        for round in 0..MAX_ROUNDS {
            // === 检查点：每轮开始前检查控制信号 ===
            self.check_control_signals();
            if self.cancelled {
                self.phase = TurnPhase::Cancelled;
                break;
            }

            // 注入用户追加消息
            self.inject_pending_user_messages(&mut loop_messages);

            let stage = format!("react-round-{}", round + 1);

            // 构建本轮请求
            let req = ModelRequest {
                session_title: self.session.title.clone(),
                user_input: if round == 0 {
                    system_prompt.clone()
                } else {
                    "根据上面的工具执行结果继续处理。如果已经收集到足够信息，直接给出最终回复，不要再调用工具。".to_string()
                },
                context: {
                    let mut ctx = context.clone();
                    ctx.extend(loop_messages.clone());
                    ctx
                },
            };

            // 调用 LLM
            self.phase = TurnPhase::LlmCalling;
            let response = self.engine.client().complete_with_functions_stream(
                &req,
                &function_tools,
                &mut |_delta: &ModelStreamChunk| {},
            )?;

            accumulated_usage.accumulate(&response.usage);

            // 无工具调用 → 最终回复
            if response.tool_calls.is_empty() {
                final_text = response.text.clone();
                final_reasoning = response.reasoning_content.clone();
                if !final_text.is_empty() || !final_reasoning.is_empty() {
                    let _ = self.tx.send(TurnEvent::Chunk(ModelStreamChunk {
                        content: final_text.clone(),
                        reasoning_content: final_reasoning.clone(),
                    }));
                    total_output_chunks += 1;
                }
                self.phase = TurnPhase::Responding;
                break;
            }

            // 有工具调用 → 记录并执行
            let tool_call_names: Vec<String> = response.tool_calls.iter().map(|tc| tc.name.clone()).collect();
            let output = LlmOutputRecord {
                stage: stage.clone(),
                content: response.text.clone(),
                reasoning_content: response.reasoning_content.clone(),
                tool_calls: tool_call_names.clone(),
                usage: response.usage.clone(),
            };
            let _ = self.tx.send(TurnEvent::LlmOutput(output));

            // 记录 assistant 工具调用意图
            let assistant_text = if response.text.is_empty() {
                format!("[调用工具: {}]", tool_call_names.join(", "))
            } else {
                response.text.clone()
            };
            loop_messages.push(Message {
                id: scru128::new().to_string(),
                role: MessageRole::Assistant,
                content: assistant_text,
                reasoning_content: response.reasoning_content.clone(),
                created_at: now_text(),
            });

            // 执行工具
            self.phase = TurnPhase::ToolExecuting;
            let call_results = Self::execute_tool_calls(
                &self.engine,
                &self.tx,
                &response.tool_calls,
                &mcp_targets,
                &session_cwd,
            );

            // 收集结果
            let mut round_feedback_parts: Vec<String> = Vec::new();
            for (call_name, result) in call_results {
                let _ = self.tx.send(TurnEvent::ToolExecution(result.clone()));
                tool_results.push(result.clone());

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

            loop_messages.push(Message {
                id: scru128::new().to_string(),
                role: MessageRole::System,
                content: round_feedback_parts.join("\n\n"),
                reasoning_content: String::new(),
                created_at: now_text(),
            });

            // === 检查点：工具执行完成后检查控制信号 ===
            self.phase = TurnPhase::ResultProcessing;
            self.check_control_signals();
            if self.cancelled {
                self.phase = TurnPhase::Cancelled;
                break;
            }

            // 上下文压缩
            let prompt_tokens = response.usage.prompt_tokens;
            if organizer.needs_compression(prompt_tokens) {
                match compress_loop_messages(&loop_messages, 3, self.engine.client()) {
                    Ok(compressed) => loop_messages = compressed,
                    Err(err) => tracing::warn!("loop_messages 压缩失败：{err}"),
                }
            }
        }

        // 如果取消了，构建取消结果
        if self.cancelled {
            return Ok(TurnExecution {
                assistant_message: "执行已取消".to_string(),
                assistant_reasoning_content: String::new(),
                system_prompt,
                plan,
                tool_result_summary: None,
                tool_execution: None,
                verify_records: Vec::new(),
                output_mode: "stream".to_string(),
                output_chunk_count: 0,
                usage: accumulated_usage,
                llm_calls: Vec::new(),
            });
        }

        // 如果循环结束仍无最终回复，做最后一次无工具调用
        if final_text.is_empty() {
            let req = ModelRequest {
                session_title: self.session.title.clone(),
                user_input: "请基于以上所有工具执行结果，直接给出最终回复。".to_string(),
                context: {
                    let mut ctx = context.clone();
                    ctx.extend(loop_messages.clone());
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
            accumulated_usage.accumulate(&resp.usage);
            final_text = resp.text;
            final_reasoning = resp.reasoning_content;
            total_output_chunks += 1;
        }

        let tool_result_summary = if tool_results.is_empty() {
            None
        } else {
            Some(format!(
                "{} 次工具调用，{} 成功，{} 失败",
                tool_results.len(),
                tool_results.iter().filter(|r| r.ok).count(),
                tool_results.iter().filter(|r| !r.ok).count(),
            ))
        };
        let tool_execution = tool_results.into_iter().filter_map(|r| r.execution).next_back();
        let cleaned_text = strip_tool_traces_from_response(&final_text);

        #[cfg(feature = "llm-debug-log")]
        let llm_calls = assembled.llm_calls;
        #[cfg(not(feature = "llm-debug-log"))]
        let llm_calls = Vec::new();

        self.phase = TurnPhase::Completed;
        Ok(TurnExecution {
            assistant_message: cleaned_text,
            assistant_reasoning_content: final_reasoning,
            system_prompt,
            plan,
            tool_result_summary,
            tool_execution,
            verify_records: Vec::new(),
            output_mode: "stream".to_string(),
            output_chunk_count: total_output_chunks,
            usage: accumulated_usage,
            llm_calls,
        })
    }

    /// 非阻塞检查控制信号
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
                ControlSignal::PermissionResponse { .. } => {
                    // Phase 5+ 处理
                }
            }
        }
    }

    /// 将用户追加消息注入到 loop_messages
    fn inject_pending_user_messages(&mut self, loop_messages: &mut Vec<Message>) {
        for msg in self.pending_user_messages.drain(..) {
            loop_messages.push(Message {
                id: scru128::new().to_string(),
                role: MessageRole::User,
                content: format!("[用户追加指示] {msg}"),
                reasoning_content: String::new(),
                created_at: now_text(),
            });
        }
    }

    /// 执行工具调用（管理命令通过 tx 发出，其余并发执行）
    fn execute_tool_calls(
        engine: &RuntimeEngine,
        tx: &Sender<TurnEvent>,
        tool_calls: &[ModelFunctionCall],
        mcp_targets: &std::collections::HashMap<String, crate::agents::execution_mcp_agent::McpFunctionTarget>,
        session_cwd: &Option<std::path::PathBuf>,
    ) -> Vec<(String, ToolResult)> {
        // 分离管理工具
        let mut call_results: Vec<(String, ToolResult)> = Vec::new();
        let mut other_calls: Vec<&ModelFunctionCall> = Vec::new();

        for call in tool_calls {
            if let Some(result) = RuntimeEngine::handle_management_tool(call, tx) {
                call_results.push((call.name.clone(), result));
            } else {
                other_calls.push(call);
            }
        }

        // 执行普通工具
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
