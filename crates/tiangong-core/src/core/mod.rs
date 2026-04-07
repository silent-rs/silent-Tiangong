//! TiangongCore：单一对话处理核心
//!
//! 只维护一个 session，不知道会话列表，不知道应用配置。
//! 创建时传入 Sender<StreamEvent>，core 自动推送事件，外部消费 receiver。

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

/// 单一对话处理核心
///
/// 创建时传入 `Sender<StreamEvent>`，core 自动推送事件。
/// session 由内部消费线程独占维护，不需要锁。
pub struct TiangongCore {
    /// 向 runner 发送事件
    event_tx: Option<Sender<LoopEvent>>,
    /// runner 线程
    runner_thread: Option<JoinHandle<LoopOutcome>>,
    /// 消费线程（从 turn_rx 消费 → 更新 session → 推送 StreamEvent）
    consumer_thread: Option<JoinHandle<Session>>,
    /// 会话 ID（创建时记录）
    session_id: String,
}

/// 消费线程内部状态
struct ConsumerState {
    session: Session,
    stream_tx: Sender<StreamEvent>,
    pending_assistant_id: Option<String>,
    pending_thinking_id: Option<String>,
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

        // 启动 runner 线程
        let runner = EventLoopRunner::new(engine, session.clone(), turn_tx, event_rx);
        let runner_thread = thread::spawn(move || runner.run());

        // 启动消费线程：从 turn_rx 消费 → 更新 session → 推送 StreamEvent
        let consumer_thread = thread::spawn(move || {
            let mut state = ConsumerState {
                session,
                stream_tx,
                pending_assistant_id: None,
                pending_thinking_id: None,
            };

            while let Ok(event) = turn_rx.recv() {
                let stream_event = state.process_event(event);
                if let Some(se) = stream_event
                    && state.stream_tx.send(se).is_err()
                {
                    break; // receiver 已关闭
                }
            }

            state.session
        });

        Self {
            event_tx: Some(event_tx),
            runner_thread: Some(runner_thread),
            consumer_thread: Some(consumer_thread),
            session_id,
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

    /// 发送事件
    pub fn send(&self, event: LoopEvent) {
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(event);
        }
    }

    /// 发送用户消息
    pub fn send_message(&self, content: String) {
        self.send(LoopEvent::UserMessage { content });
    }

    /// 取消当前执行
    pub fn cancel(&self) {
        self.send(LoopEvent::Cancel);
    }

    /// 会话 ID
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// 是否有活跃任务
    pub fn is_busy(&self) -> bool {
        self.runner_thread
            .as_ref()
            .is_some_and(|t| !t.is_finished())
    }

    /// 关闭并获取最终 session（用于持久化）
    pub fn shutdown(mut self) -> Session {
        // 通知 runner 关闭
        self.send(LoopEvent::SystemSignal(
            crate::event_loop::SystemSignalKind::Shutdown,
        ));
        self.event_tx = None;

        // 等待 runner 线程结束
        if let Some(t) = self.runner_thread.take() {
            let _ = t.join();
        }

        // 等待消费线程结束并获取 session
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
        self.send(LoopEvent::SystemSignal(
            crate::event_loop::SystemSignalKind::Shutdown,
        ));
        self.event_tx = None;
    }
}

// ==================== ConsumerState 事件处理 ====================

impl ConsumerState {
    fn process_event(&mut self, event: TurnEvent) -> Option<StreamEvent> {
        // 生成 StreamEvent
        let stream_event = match &event {
            TurnEvent::Chunk(delta) => {
                if !delta.content.is_empty() {
                    Some(StreamEvent::Delta(delta.content.clone()))
                } else if !delta.reasoning_content.is_empty() {
                    Some(StreamEvent::Reasoning(delta.reasoning_content.clone()))
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
                Some(StreamEvent::ToolCalls(output.tool_calls.clone()))
            }
            TurnEvent::Completed(_) => Some(StreamEvent::Done),
            TurnEvent::Failed(err) => Some(StreamEvent::Error(err.clone())),
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
        let mut lines = vec![format!("LLM 输出 [{}]", output.stage)];
        lines.push(format!(
            "tokens: prompt={}, completion={}, total={}",
            output.usage.prompt_tokens, output.usage.completion_tokens, output.usage.total_tokens
        ));
        if !output.tool_calls.is_empty() {
            lines.push(format!("tool_calls: {}", output.tool_calls.join(", ")));
        }
        if !output.content.is_empty() {
            lines.push(format!("content:\n{}", output.content));
        }
        let content = lines.join("\n");

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
        let status = if result.ok { "ok=true" } else { "ok=false" };
        let tool_name = result
            .execution
            .as_ref()
            .map(|e| e.tool_name.as_str())
            .unwrap_or("unknown");
        let stdout_preview = if result.stdout.chars().count() > 500 {
            let truncated: String = result.stdout.chars().take(500).collect();
            format!("{truncated}...(截断)")
        } else {
            result.stdout.clone()
        };
        self.session.append_message(
            MessageRole::System,
            format!(
                "工具执行 [{tool_name}]\n{status} exit_code={}\nsummary: {}\nstdout:\n{stdout_preview}",
                result.exit_code, result.summary,
            ),
        );
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
