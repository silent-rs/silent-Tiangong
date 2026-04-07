//! TiangongCore：单一对话处理核心
//!
//! 只维护一个 session，不知道会话列表，不知道应用配置。
//! CLI 直接使用一个 TiangongCore，GUI 使用 HashMap<String, TiangongCore>。

use std::sync::mpsc::{self, Receiver, Sender};
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

/// 默认上下文窗口大小
const DEFAULT_CONTEXT_LIMIT: usize = 32_768;

/// 单一对话处理核心
pub struct TiangongCore {
    /// runner 线程
    thread: Option<JoinHandle<LoopOutcome>>,
    /// 向 runner 发送事件
    event_tx: Option<Sender<LoopEvent>>,
    /// 从 runner 接收 TurnEvent
    turn_rx: Receiver<TurnEvent>,
    /// 唯一的 session
    session: Session,
    /// 当前 assistant 消息 ID（流式追加用）
    pending_assistant_id: Option<String>,
    /// 当前 stage thinking 消息 ID
    pending_thinking_id: Option<String>,
}

impl TiangongCore {
    /// 创建新对话（自动加载配置）
    pub fn new() -> Self {
        let session = Session::new("新对话");
        Self::with_session(session)
    }

    /// 从已有 session 创建（加载历史对话）
    pub fn with_session(session: Session) -> Self {
        let engine = Self::build_engine();
        Self::start(engine, session)
    }

    /// 从配置构建 RuntimeEngine
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

    /// 启动 runner 线程
    fn start(engine: RuntimeEngine, session: Session) -> Self {
        let (turn_tx, turn_rx) = mpsc::channel::<TurnEvent>();
        let (event_tx, event_rx) = mpsc::channel::<LoopEvent>();

        let runner = EventLoopRunner::new(engine, session.clone(), turn_tx, event_rx);
        let thread = thread::spawn(move || runner.run());

        Self {
            thread: Some(thread),
            event_tx: Some(event_tx),
            turn_rx,
            session,
            pending_assistant_id: None,
            pending_thinking_id: None,
        }
    }

    /// 发送事件到 runner
    pub fn send(&self, event: LoopEvent) {
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(event);
        }
    }

    /// 发送用户消息
    pub fn send_message(&mut self, content: String) {
        self.session
            .append_message(MessageRole::User, content.clone());
        self.send(LoopEvent::UserMessage { content });
    }

    /// 取消当前执行
    pub fn cancel(&self) {
        self.send(LoopEvent::Cancel);
    }

    /// 消费 TurnEvent，更新 session，返回 StreamEvent 列表
    pub fn poll(&mut self) -> Vec<StreamEvent> {
        let mut events = Vec::new();

        // 检查 runner 线程是否已结束
        if let Some(ref thread) = self.thread
            && thread.is_finished()
        {
            self.event_tx = None;
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }

        // 消费所有待处理的 TurnEvent
        while let Ok(turn_event) = self.turn_rx.try_recv() {
            let (_, stream_event) = self.process_event(turn_event);
            if let Some(se) = stream_event {
                events.push(se);
            }
        }

        events
    }

    /// 处理单个 TurnEvent：更新 session + 生成 StreamEvent
    fn process_event(&mut self, event: TurnEvent) -> (bool, Option<StreamEvent>) {
        let mut done = false;

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
            TurnEvent::Chunk(delta) => {
                self.apply_chunk(&delta);
            }
            TurnEvent::LlmOutput(output) => {
                if !output.tool_calls.is_empty() {
                    self.pending_assistant_id = None;
                    self.pending_thinking_id = None;
                }
                self.append_llm_output(&output);
            }
            TurnEvent::ToolExecution(result) => {
                self.append_tool_execution(&result);
            }
            TurnEvent::StageThinking { stage, delta } => {
                self.apply_stage_thinking(&stage, &delta);
            }
            TurnEvent::Completed(_) => {
                done = true;
            }
            TurnEvent::Failed(err) => {
                self.session
                    .append_message(MessageRole::System, format!("执行失败：{err}"));
                done = true;
            }
            _ => {}
        }

        (done, stream_event)
    }

    /// 流式追加到 assistant 消息
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

    /// 追加 LLM 输出系统消息
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

    /// 追加工具执行系统消息
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
        let content = format!(
            "工具执行 [{tool_name}]\n{status} exit_code={}\nsummary: {}\nstdout:\n{stdout_preview}",
            result.exit_code, result.summary,
        );
        self.session.append_message(MessageRole::System, content);
    }

    /// 流式追加 stage thinking 到系统消息
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

    /// 获取 session 引用
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// 获取 session 可变引用
    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    /// 是否有活跃任务
    pub fn is_busy(&self) -> bool {
        self.event_tx.is_some() && self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    /// 会话 ID
    pub fn session_id(&self) -> &str {
        &self.session.id
    }

    /// 关闭
    pub fn shutdown(&mut self) {
        self.send(LoopEvent::SystemSignal(
            crate::event_loop::SystemSignalKind::Shutdown,
        ));
        self.event_tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Default for TiangongCore {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TiangongCore {
    fn drop(&mut self) {
        self.shutdown();
    }
}
