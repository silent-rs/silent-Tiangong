//! TiangongCore：单一对话处理核心
//!
//! 只维护一个 session，不知道会话列表，不知道应用配置。
//! CLI 直接使用一个 TiangongCore，GUI 使用 HashMap<String, TiangongCore>。

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use crate::app_state::StreamEvent;
use crate::app_state::TurnEvent;
use crate::event_loop::{EventLoopRunner, LoopEvent, LoopOutcome};
use crate::model::ModelStreamChunk;
use crate::runtime::{LlmOutputRecord, RuntimeEngine};
use crate::session::{MessageRole, Session};
use crate::tool::ToolResult;

/// 单一对话处理核心
pub struct TiangongCore {
    /// runner 线程
    thread: Option<JoinHandle<LoopOutcome>>,
    /// 向 runner 发送事件
    event_tx: Option<Sender<LoopEvent>>,
    /// 从 runner 接收 TurnEvent
    turn_rx: Receiver<TurnEvent>,
    /// 唯一的 session（由 core 维护）
    session: Session,
    /// 引擎配置（用于重新启动 runner）
    #[allow(dead_code)]
    engine: RuntimeEngine,
    /// 当前 assistant 消息 ID（流式追加用）
    pending_assistant_id: Option<String>,
    /// 当前 stage thinking 消息 ID
    pending_thinking_id: Option<String>,
}

impl TiangongCore {
    /// 创建对话核心
    pub fn new(engine: RuntimeEngine, session: Session) -> Self {
        let (turn_tx, turn_rx) = mpsc::channel::<TurnEvent>();
        let (event_tx, event_rx) = mpsc::channel::<LoopEvent>();

        let runner = EventLoopRunner::new(
            engine.clone(),
            session.clone(),
            turn_tx,
            event_rx,
        );

        let thread = thread::spawn(move || runner.run());

        Self {
            thread: Some(thread),
            event_tx: Some(event_tx),
            turn_rx,
            session,
            engine,
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
        // 在 session 中记录用户消息
        self.session.append_message(MessageRole::User, content.clone());
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
            TurnEvent::Chunk(delta) => {
                self.apply_chunk(&delta);
            }
            TurnEvent::LlmOutput(output) => {
                if !output.tool_calls.is_empty() {
                    // 工具调用：重置 assistant id，下一轮 Chunk 创建新消息
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
                self.session.append_message(MessageRole::System, format!("执行失败：{err}"));
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

        if let Some(ref id) = self.pending_assistant_id {
            // 追加到已有消息
            if let Some(msg) = self.session.messages.iter_mut().find(|m| m.id == *id) {
                msg.content.push_str(&delta.content);
                msg.reasoning_content.push_str(&delta.reasoning_content);
            }
        } else {
            // 创建新 assistant 消息
            self.session.append_message(MessageRole::Assistant, String::new());
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

        // 如果有 stage thinking 消息，更新它；否则创建新的
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
        let content = format!(
            "工具执行 [{}]\n{} exit_code={}\nsummary: {}\nstdout:\n{}",
            result.execution.as_ref().map(|e| e.tool_name.as_str()).unwrap_or("unknown"),
            status,
            result.exit_code,
            result.summary,
            if result.stdout.len() > 500 {
                format!("{}...(截断)", &result.stdout.chars().take(500).collect::<String>())
            } else {
                result.stdout.clone()
            }
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

        // 创建新的系统消息
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

    /// 关闭核心
    pub fn shutdown(&mut self) {
        self.send(LoopEvent::SystemSignal(crate::event_loop::SystemSignalKind::Shutdown));
        self.event_tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for TiangongCore {
    fn drop(&mut self) {
        self.shutdown();
    }
}
