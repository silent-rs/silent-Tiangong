//! TiangongCore：单一对话处理核心
//!
//! 所有事件统一在消费线程中处理，session 由消费线程独占维护。
//! 外部通过 Sender<StreamEvent> 接收输出，通过方法发送输入。

use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

use crate::agent_config::AgentConfig;
use crate::app_state::StreamEvent;
use crate::app_state::TurnEvent;
use crate::event_loop::{EventLoopRunner, LoopEvent, LoopOutcome};
use crate::model::{ModelProviderConfig, ModelStreamChunk, SingleProviderClient};
use crate::models_config::ModelsConfig;
use crate::runtime::{LlmOutputRecord, RuntimeEngine};
use crate::session::{MessageRole, Session};
use crate::tool::ToolResult;

const DEFAULT_CONTEXT_LIMIT: usize = 32_768;

/// 消费线程内部事件（统一所有事件来源）
enum CoreEvent {
    /// 从 runner 来的事件
    Turn(TurnEvent),
    /// 用户消息
    UserMessage(String),
    /// 审批响应
    ApprovalResponse { request_id: String, approved: bool },
    /// 取消当前执行
    Cancel,
    /// 关闭 core
    Shutdown,
}

/// 单一对话处理核心
pub struct TiangongCore {
    /// 向 runner 发送事件
    event_tx: Option<Sender<LoopEvent>>,
    /// 向消费线程发送事件
    core_tx: Option<Sender<CoreEvent>>,
    /// runner 线程
    runner_thread: Option<JoinHandle<LoopOutcome>>,
    /// 消费线程（返回最终 session）
    consumer_thread: Option<JoinHandle<Session>>,
    /// 会话 ID
    session_id: String,
}

impl TiangongCore {
    /// 创建新对话
    pub fn new(stream_tx: Sender<StreamEvent>) -> Self {
        let session = Session::new("新对话");
        Self::with_session(session, stream_tx)
    }

    /// 从已有 session 创建
    pub fn with_session(session: Session, stream_tx: Sender<StreamEvent>) -> Self {
        let engine = Self::build_engine();
        let session_id = session.id.clone();

        let (turn_tx, turn_rx) = mpsc::channel::<TurnEvent>();
        let (event_tx, event_rx) = mpsc::channel::<LoopEvent>();
        let (core_tx, core_rx) = mpsc::channel::<CoreEvent>();

        // runner 的 TurnEvent 转发到 core_rx
        let core_tx_for_turn = core_tx.clone();
        thread::spawn(move || {
            while let Ok(event) = turn_rx.recv() {
                if core_tx_for_turn.send(CoreEvent::Turn(event)).is_err() {
                    break;
                }
            }
        });

        // 启动 runner 线程
        let runner = EventLoopRunner::new(engine, session.clone(), turn_tx, event_rx);
        let runner_thread = thread::spawn(move || runner.run());

        // 启动消费线程
        let event_tx_for_consumer = event_tx.clone();
        let consumer_thread = thread::spawn(move || {
            let mut state = ConsumerState {
                session,
                event_tx: event_tx_for_consumer,
                stream_tx,
                pending_assistant_id: None,
                pending_thinking_id: None,
            };

            while let Ok(event) = core_rx.recv() {
                match event {
                    CoreEvent::Turn(turn_event) => {
                        let stream_event = state.process_turn_event(turn_event);
                        if let Some(se) = stream_event
                            && state.stream_tx.send(se).is_err()
                        {
                            break;
                        }
                    }
                    CoreEvent::UserMessage(content) => {
                        // 新一轮：重置 assistant 和 thinking ID
                        state.pending_assistant_id = None;
                        state.pending_thinking_id = None;
                        state
                            .session
                            .append_message(MessageRole::User, content.clone());
                        let _ = state.event_tx.send(LoopEvent::UserMessage { content });
                    }
                    CoreEvent::ApprovalResponse {
                        request_id,
                        approved,
                    } => {
                        let _ = state.event_tx.send(LoopEvent::PermissionResponse {
                            request_id,
                            approved,
                        });
                    }
                    CoreEvent::Cancel => {
                        let _ = state.event_tx.send(LoopEvent::Cancel);
                    }
                    CoreEvent::Shutdown => {
                        let _ = state
                            .event_tx
                            .send(LoopEvent::SystemSignal(
                                crate::event_loop::SystemSignalKind::Shutdown,
                            ));
                        break;
                    }
                }
            }

            state.session
        });

        Self {
            event_tx: Some(event_tx),
            core_tx: Some(core_tx),
            runner_thread: Some(runner_thread),
            consumer_thread: Some(consumer_thread),
            session_id,
        }
    }

    fn send_core_event(&self, event: CoreEvent) {
        if let Some(ref tx) = self.core_tx {
            let _ = tx.send(event);
        }
    }

    fn build_engine() -> RuntimeEngine {
        let mut models_config = ModelsConfig::load();
        if models_config.is_empty() {
            let env_config = ModelProviderConfig::from_env();
            if !env_config.api_auth_token.is_empty() {
                models_config = ModelsConfig::from_legacy(&env_config);
            }
        }
        let model_config = models_config.to_chat_provider_config();
        let agent_config = AgentConfig::default();

        RuntimeEngine::new(
            SingleProviderClient::new(model_config),
            DEFAULT_CONTEXT_LIMIT,
            agent_config,
        )
        .with_models_config(models_config)
    }

    /// 发送用户消息
    pub fn send_message(&self, content: String) {
        self.send_core_event(CoreEvent::UserMessage(content));
    }

    /// 取消当前执行
    pub fn cancel(&self) {
        self.send_core_event(CoreEvent::Cancel);
    }

    /// 响应审批
    pub fn respond_approval(&self, request_id: String, approved: bool) {
        self.send_core_event(CoreEvent::ApprovalResponse {
            request_id,
            approved,
        });
    }

    /// 会话 ID
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// 关闭并获取最终 session（用于持久化）
    pub fn into_session(mut self) -> Session {
        // 发送 Shutdown 事件 → 消费线程 break → runner 收到 Shutdown
        self.send_core_event(CoreEvent::Shutdown);
        self.core_tx = None;
        self.event_tx = None;

        // 等待 runner 线程
        if let Some(t) = self.runner_thread.take() {
            let _ = t.join();
        }

        // 等待消费线程返回 session
        if let Some(t) = self.consumer_thread.take() {
            match t.join() {
                Ok(session) => return session,
                Err(_) => tracing::warn!("消费线程 panic"),
            }
        }

        Session::new("recovered")
    }
}

impl Drop for TiangongCore {
    fn drop(&mut self) {
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(LoopEvent::SystemSignal(
                crate::event_loop::SystemSignalKind::Shutdown,
            ));
        }
        self.event_tx = None;
    }
}

// ==================== 消费线程内部状态 ====================

struct ConsumerState {
    session: Session,
    event_tx: Sender<LoopEvent>,
    stream_tx: Sender<StreamEvent>,
    pending_assistant_id: Option<String>,
    pending_thinking_id: Option<String>,
}

impl ConsumerState {
    fn process_turn_event(&mut self, event: TurnEvent) -> Option<StreamEvent> {
        let stream_event = match &event {
            TurnEvent::Chunk(delta) => {
                if !delta.content.is_empty() {
                    Some(StreamEvent::Delta { content: delta.content.clone() })
                } else if !delta.reasoning_content.is_empty() {
                    Some(StreamEvent::Reasoning { content: delta.reasoning_content.clone() })
                } else {
                    None
                }
            }
            TurnEvent::ToolStarted { name, summary } => Some(StreamEvent::ToolStart {
                name: name.clone(),
                summary: summary.clone(),
            }),
            TurnEvent::ToolExecution(result) => Some(StreamEvent::ToolResult {
                name: result.summary.clone(),
                ok: result.ok,
                output: result.stdout.clone(),
            }),
            TurnEvent::LlmOutput(output) if !output.tool_calls.is_empty() => {
                Some(StreamEvent::ToolCalls { names: output.tool_calls.clone(), usage: Some(output.usage.clone()) })
            }
            TurnEvent::LlmOutput(output) if output.tool_calls.is_empty() => {
                // 无 tool_calls 的 LlmOutput = 最终回复完成
                Some(StreamEvent::Done {
                    usage: Some(output.usage.clone()),
                })
            }
            TurnEvent::Completed(_) => Some(StreamEvent::Done { usage: None }),
            TurnEvent::Failed(err) => Some(StreamEvent::Error { message: err.clone() }),
            TurnEvent::ApprovalRequest {
                request_id,
                tool_name,
                tool_args_summary,
            } => Some(StreamEvent::ApprovalNeeded {
                request_id: request_id.clone(),
                tool_name: tool_name.clone(),
                args_summary: tool_args_summary.clone(),
            }),
            _ => None,
        };

        // 更新 session
        match event {
            TurnEvent::Chunk(delta) => self.apply_chunk(&delta),
            TurnEvent::LlmOutput(output) => {
                if !output.tool_calls.is_empty() {
                    self.pending_assistant_id = None;
                    self.pending_thinking_id = None;
                }
                self.append_llm_output(&output);
            }
            TurnEvent::ToolExecution(result) => self.append_tool_execution(&result),
            TurnEvent::StageThinking { stage, delta } => self.apply_stage_thinking(&stage, &delta),
            TurnEvent::Failed(err) => {
                self.session
                    .append_message(MessageRole::System, format!("执行失败：{err}"));
            }
            _ => {}
        }

        stream_event
    }

    fn apply_chunk(&mut self, delta: &ModelStreamChunk) {
        if delta.content.is_empty() && delta.reasoning_content.is_empty() {
            return;
        }
        if let Some(ref id) = self.pending_assistant_id
            && let Some(msg) = self.session.messages.iter_mut().find(|m| m.id == *id)
        {
            msg.content.push_str(&delta.content);
            msg.reasoning_content.push_str(&delta.reasoning_content);
        } else {
            self.session
                .append_message(MessageRole::Assistant, String::new());
            if let Some(msg) = self.session.messages.last_mut() {
                msg.content.push_str(&delta.content);
                msg.reasoning_content.push_str(&delta.reasoning_content);
                self.pending_assistant_id = Some(msg.id.clone());
            }
        }
    }

    fn append_llm_output(&mut self, output: &LlmOutputRecord) {
        let content = crate::app_state::formatting::format_llm_output_message(output);

        if let Some(ref id) = self.pending_thinking_id
            && let Some(msg) = self.session.messages.iter_mut().find(|m| m.id == *id)
        {
            msg.content = content;
            msg.reasoning_content = output.reasoning_content.clone();
            self.pending_thinking_id = None;
            return;
        }
        self.session.append_message_with_reasoning(
            MessageRole::System,
            content,
            output.reasoning_content.clone(),
        );
        self.pending_thinking_id = None;
    }

    fn append_tool_execution(&mut self, result: &ToolResult) {
        let content = crate::app_state::formatting::format_tool_trace_message(result);
        self.session.append_message(MessageRole::System, content);
    }

    fn apply_stage_thinking(&mut self, stage: &str, delta: &ModelStreamChunk) {
        if delta.content.is_empty() && delta.reasoning_content.is_empty() {
            return;
        }
        if let Some(ref id) = self.pending_thinking_id
            && let Some(msg) = self.session.messages.iter_mut().find(|m| m.id == *id)
        {
            msg.content.push_str(&delta.content);
            msg.reasoning_content.push_str(&delta.reasoning_content);
            return;
        }
        let initial = format!("LLM 输出 [{stage}]\n");
        self.session.append_message(MessageRole::System, initial);
        if let Some(msg) = self.session.messages.last_mut() {
            msg.content.push_str(&delta.content);
            msg.reasoning_content.push_str(&delta.reasoning_content);
            self.pending_thinking_id = Some(msg.id.clone());
        }
    }
}
