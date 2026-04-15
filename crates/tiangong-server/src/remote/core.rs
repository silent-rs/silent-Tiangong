use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crate::api::SharedState;
use crate::remote::event::{EventBus, TiangongEvent};
use anyhow::{Result, anyhow};
use tiangong_config::CoreConfigProvider;
use tiangong_core::core::TiangongCore;
use tiangong_core::permission::TrustMode;
use tiangong_core::session::{MessageRole, Session};
use tiangong_types::{SessionStreamEvent, StreamEvent};

#[derive(Clone)]
pub struct ServerCoreManager {
    state: SharedState,
    config: CoreConfigProvider,
    event_bus: Arc<EventBus>,
    cores: Arc<Mutex<HashMap<String, TiangongCore>>>,
    trackers: Arc<Mutex<HashMap<String, Arc<ExecutionTracker>>>>,
}

impl ServerCoreManager {
    pub fn new(state: SharedState, config: CoreConfigProvider, event_bus: Arc<EventBus>) -> Self {
        Self {
            state,
            config,
            event_bus,
            cores: Arc::new(Mutex::new(HashMap::new())),
            trackers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn send_message_and_wait(
        &self,
        requested_session_id: &str,
        content: String,
    ) -> Result<(String, String)> {
        let (session_id, session, created) = self.ensure_core(requested_session_id).await?;
        if created {
            let _ = session;
        }

        let tracker = self.tracker_for(&session_id);
        let turn_id = tracker.start_turn();

        {
            let cores = self.cores.lock().unwrap();
            let Some(core) = cores.get(&session_id) else {
                return Err(anyhow!("会话 core 不存在：{session_id}"));
            };
            core.send_message(content);
        }

        let tracker_for_wait = tracker.clone();
        let outcome = tokio::task::spawn_blocking(move || tracker_for_wait.wait_for_turn(turn_id))
            .await
            .map_err(|err| anyhow!("等待执行结果失败：{err}"))?;
        let response = match outcome {
            TurnOutcome::Completed => self
                .last_assistant_response(&session_id)
                .await
                .unwrap_or_else(|| "处理完成".to_string()),
            TurnOutcome::Failed(message) => return Err(anyhow!(message)),
        };

        Ok((session_id, response))
    }

    async fn ensure_core(&self, requested_session_id: &str) -> Result<(String, Session, bool)> {
        let (session_id, session) = {
            let mut state = self.state.lock().await;
            let session_id = if state
                .sessions()
                .iter()
                .any(|s| s.id == requested_session_id)
            {
                if state.active_session_id() != requested_session_id {
                    state.switch_session(requested_session_id);
                }
                requested_session_id.to_string()
            } else {
                let idx = state.ensure_active_session_index();
                state.sessions()[idx].id.clone()
            };
            let session = state
                .sessions()
                .iter()
                .find(|s| s.id == session_id)
                .cloned()
                .ok_or_else(|| anyhow!("会话不存在：{session_id}"))?;
            (session_id, session)
        };

        let mut cores = self.cores.lock().unwrap();
        if cores.contains_key(&session_id) {
            return Ok((session_id, session, false));
        }

        let (stream_tx, stream_rx) = mpsc::channel::<SessionStreamEvent>();
        let core = TiangongCore::with_session(self.config.clone(), session.clone(), stream_tx);
        core.set_trust_mode(TrustMode::FullTrust);
        let actual_session_id = core.session_id().to_string();
        let tracker = self.tracker_for(&actual_session_id);
        cores.insert(actual_session_id.clone(), core);
        drop(cores);

        self.spawn_stream_forwarder(actual_session_id.clone(), stream_rx, tracker);

        Ok((actual_session_id, session, true))
    }

    fn spawn_stream_forwarder(
        &self,
        session_id: String,
        stream_rx: mpsc::Receiver<SessionStreamEvent>,
        tracker: Arc<ExecutionTracker>,
    ) {
        let state = self.state.clone();
        let event_bus = self.event_bus.clone();
        thread::spawn(move || {
            for session_event in stream_rx {
                let event = session_event.event;
                tracker.observe_event(&event);
                sync_stream_event_to_state(&state, &event_bus, &session_id, &event);
            }
        });
    }

    fn tracker_for(&self, session_id: &str) -> Arc<ExecutionTracker> {
        let mut trackers = self.trackers.lock().unwrap();
        trackers
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(ExecutionTracker::default()))
            .clone()
    }

    async fn last_assistant_response(&self, session_id: &str) -> Option<String> {
        let state = self.state.lock().await;
        state
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| {
                session
                    .messages
                    .iter()
                    .rev()
                    .find(|message| message.role == MessageRole::Assistant)
                    .map(|message| message.content.clone())
            })
    }
}

#[derive(Debug, Clone)]
enum TurnOutcome {
    Completed,
    Failed(String),
}

#[derive(Default)]
struct ExecutionTracker {
    state: Mutex<ExecutionTrackerState>,
    notify: Condvar,
}

#[derive(Default)]
struct ExecutionTrackerState {
    next_turn_id: u64,
    current_turn_id: Option<u64>,
    outcomes: HashMap<u64, TurnOutcome>,
}

impl ExecutionTracker {
    fn start_turn(&self) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.next_turn_id += 1;
        let turn_id = state.next_turn_id;
        state.current_turn_id = Some(turn_id);
        state.outcomes.remove(&turn_id);
        turn_id
    }

    fn observe_event(&self, event: &StreamEvent) {
        let mut state = self.state.lock().unwrap();
        let Some(turn_id) = state.current_turn_id else {
            return;
        };

        match event {
            StreamEvent::Done { .. } => {
                state.outcomes.insert(turn_id, TurnOutcome::Completed);
                state.current_turn_id = None;
                self.notify.notify_all();
            }
            StreamEvent::Error { message } => {
                state
                    .outcomes
                    .insert(turn_id, TurnOutcome::Failed(message.clone()));
                state.current_turn_id = None;
                self.notify.notify_all();
            }
            _ => {}
        }
    }

    fn wait_for_turn(&self, turn_id: u64) -> TurnOutcome {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(outcome) = state.outcomes.remove(&turn_id) {
                return outcome;
            }
            state = self.notify.wait(state).unwrap();
        }
    }
}

fn sync_stream_event_to_state(
    state: &SharedState,
    event_bus: &Arc<EventBus>,
    session_id: &str,
    event: &StreamEvent,
) {
    let mut state = state.blocking_lock();
    let mut should_persist = false;
    let mut completion_event: Option<bool> = None;

    let Some(session) = state.sessions_mut().iter_mut().find(|s| s.id == session_id) else {
        return;
    };

    match event {
        StreamEvent::UserMessage {
            message_id,
            content,
        } => {
            if !session
                .messages
                .iter()
                .any(|message| message.id == *message_id)
            {
                session.append_message_with_id(
                    message_id.clone(),
                    MessageRole::User,
                    content.clone(),
                    String::new(),
                );
            }
        }
        StreamEvent::Delta {
            message_id,
            content,
        } => append_assistant_delta(session, message_id, content),
        StreamEvent::Reasoning {
            message_id,
            content,
        } => append_assistant_reasoning(session, message_id, content),
        StreamEvent::ToolCalls { names, .. } => {
            session.append_message(
                MessageRole::System,
                format!("LLM 输出\ntool_calls: {}", names.join(", ")),
            );
        }
        StreamEvent::ToolStart { name, args_summary } => {
            let summary = if args_summary.trim().is_empty() {
                format!("正在执行工具：{name}")
            } else {
                format!("正在执行工具：{name} {args_summary}")
            };
            session.append_message(MessageRole::System, summary);
        }
        StreamEvent::ToolResult { name, ok, output } => {
            let status = if *ok { "成功" } else { "失败" };
            session.append_message(
                MessageRole::System,
                format!("工具 {name} 执行{status}\n{output}"),
            );
        }
        StreamEvent::ApprovalNeeded { .. } => {
            session.append_message(
                MessageRole::System,
                "[Server 模式已强制 full_trust，不应进入审批状态]".to_string(),
            );
            should_persist = true;
            completion_event = Some(false);
        }
        StreamEvent::Done { .. } => {
            should_persist = true;
            completion_event = Some(true);
        }
        StreamEvent::Error { message } => {
            session.append_message(MessageRole::System, format!("[错误] {message}"));
            should_persist = true;
            completion_event = Some(false);
        }
        StreamEvent::Retry {
            message,
            attempt,
            max_attempts,
        } => {
            session.append_message(
                MessageRole::System,
                format!("[重试] ({attempt}/{max_attempts}) {message}"),
            );
        }
        _ => {}
    }

    if should_persist {
        let _ = state.persist_session_and_app(session_id);
    }
    drop(state);

    if let Some(success) = completion_event {
        event_bus.publish(TiangongEvent::TurnCompleted {
            session_id: session_id.to_string(),
            success,
        });
    }
}

fn append_assistant_delta(session: &mut Session, message_id: &str, content: &str) {
    ensure_assistant_message(session, message_id);
    if let Some(message) = session
        .messages
        .iter_mut()
        .find(|message| message.id == message_id)
    {
        message.content.push_str(content);
    }
}

fn append_assistant_reasoning(session: &mut Session, message_id: &str, content: &str) {
    ensure_assistant_message(session, message_id);
    if let Some(message) = session
        .messages
        .iter_mut()
        .find(|message| message.id == message_id)
    {
        message.reasoning_content.push_str(content);
    }
}

fn ensure_assistant_message(session: &mut Session, message_id: &str) {
    if session
        .messages
        .iter()
        .any(|message| message.id == message_id)
    {
        return;
    }

    session.append_message_with_id(
        message_id.to_string(),
        MessageRole::Assistant,
        String::new(),
        String::new(),
    );
}
