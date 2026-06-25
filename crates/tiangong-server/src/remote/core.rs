use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use crate::api::SharedState;
use crate::remote::event::{EventBus, TiangongEvent};
use anyhow::{Result, anyhow};
use tiangong_core::agent_input::{AgentInput, AgentInputKind};
use tiangong_core::core_config::CoreConfigProvider;
use tiangong_core::permission::TrustMode;
use tiangong_core::session::{Message, MessageRole, MessageToolCall, Session, now_text};
use tiangong_types::{
    MediaAsset, MediaKind, MessageContent, OutgoingMessage, SessionStreamEvent, StreamEvent,
};

#[derive(Clone)]
pub struct ServerCoreManager {
    state: SharedState,
    config: CoreConfigProvider,
    event_bus: Arc<EventBus>,
    cores: Arc<Mutex<HashMap<String, tiangong_core::core::TiangongCore>>>,
    trackers: Arc<Mutex<HashMap<String, Arc<ExecutionTracker>>>>,
    remote_sessions: Arc<Mutex<HashMap<String, String>>>,
}

impl ServerCoreManager {
    pub fn new(state: SharedState, config: CoreConfigProvider, event_bus: Arc<EventBus>) -> Self {
        Self {
            state,
            config,
            event_bus,
            cores: Arc::new(Mutex::new(HashMap::new())),
            trackers: Arc::new(Mutex::new(HashMap::new())),
            remote_sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn send_connector_message_and_wait(
        &self,
        connector: &str,
        channel_id: &str,
        content: String,
        message_id: Option<String>,
        media: Vec<MediaAsset>,
    ) -> Result<(String, OutgoingMessage)> {
        let session_id = self
            .resolve_connector_session_id(connector, channel_id)
            .await?;
        self.send_message_and_wait(&session_id, content, message_id, media)
            .await
    }

    /// 发送消息到 core（不等结果），适用于定时任务等触发场景
    pub async fn send_message(
        &self,
        requested_session_id: &str,
        content: String,
        message_id: Option<String>,
        media: Vec<MediaAsset>,
    ) -> Result<()> {
        let (session_id, _session, _created) = self.ensure_core(requested_session_id).await?;
        let cores = self.cores.lock().unwrap();
        let Some(core) = cores.get(&session_id) else {
            return Err(anyhow!("会话 core 不存在：{session_id}"));
        };
        let msg_id = message_id.unwrap_or_else(|| scru128::new().to_string());
        core.deliver(AgentInputKind::message_with_id(content, msg_id, media));
        Ok(())
    }

    pub async fn send_message_and_wait(
        &self,
        requested_session_id: &str,
        content: String,
        message_id: Option<String>,
        media: Vec<MediaAsset>,
    ) -> Result<(String, OutgoingMessage)> {
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
            let msg_id = message_id.unwrap_or_else(|| scru128::new().to_string());
            core.deliver(AgentInputKind::message_with_id(content, msg_id, media));
        }

        let tracker_for_wait = tracker.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let outcome = tracker_for_wait.wait_for_turn(turn_id);
            let _ = tx.send(outcome);
        });
        let outcome = rx
            .await
            .map_err(|_| anyhow!("等待执行结果的线程意外退出"))?;
        let response = match outcome {
            TurnOutcome::Completed => self.last_assistant_outgoing(&session_id).await,
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
        let core = tiangong_core::core::TiangongCore::with_session_for_server(
            self.config.clone(),
            session.clone(),
            stream_tx,
        );
        core.set_trust_mode(TrustMode::FullTrust);
        let actual_session_id = core.session_id().to_string();
        let tracker = self.tracker_for(&actual_session_id);
        cores.insert(actual_session_id.clone(), core);
        drop(cores);

        self.spawn_stream_forwarder(actual_session_id.clone(), stream_rx, tracker);

        Ok((actual_session_id, session, true))
    }

    async fn resolve_connector_session_id(
        &self,
        connector: &str,
        channel_id: &str,
    ) -> Result<String> {
        let channel_id = channel_id.trim();
        if channel_id.is_empty() {
            let state = self.state.lock().await;
            return Ok(state.active_session_id().to_string());
        }

        let key = remote_session_key(connector, channel_id);
        if let Some(session_id) = self.remote_sessions.lock().unwrap().get(&key).cloned() {
            return Ok(session_id);
        }

        let title = remote_session_title(connector, channel_id);
        let session_id = {
            let mut state = self.state.lock().await;
            if state
                .sessions()
                .iter()
                .any(|session| session.id == channel_id)
            {
                channel_id.to_string()
            } else if let Some(session) = state
                .sessions()
                .iter()
                .find(|session| session.title == title)
            {
                session.id.clone()
            } else {
                let mut session = Session::new_isolated(title);
                session.trust_mode = TrustMode::FullTrust;
                let session_id = session.id.clone();
                state.sessions_mut().push(session);
                state.persist_session(&session_id)?;
                self.event_bus
                    .publish(TiangongEvent::SessionCreated(session_id.clone()));
                session_id
            }
        };

        self.remote_sessions
            .lock()
            .unwrap()
            .insert(key, session_id.clone());
        Ok(session_id)
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

    async fn last_assistant_outgoing(&self, session_id: &str) -> OutgoingMessage {
        let state = self.state.lock().await;
        let Some(session) = state
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
        else {
            return text_outgoing("处理完成");
        };

        let last_user_index = session
            .messages
            .iter()
            .rposition(|message| message.role == MessageRole::User)
            .unwrap_or(0);
        let mut latest_text = String::new();
        let mut latest_media: Option<MediaAsset> = None;
        for message in session.messages.iter().skip(last_user_index + 1) {
            if message.role != MessageRole::Assistant {
                continue;
            }
            let text = message.text_content();
            if !text.trim().is_empty() {
                latest_text = text;
            }
            for block in &message.content {
                if let tiangong_types::ContentBlock::Media { kind, url, .. } = block {
                    latest_media = Some(MediaAsset {
                        kind: *kind,
                        url: url.clone(),
                        mime_type: None,
                        title: None,
                        capability: None,
                    });
                }
            }
        }

        if let Some(media) = latest_media {
            return media_outgoing(media, latest_text);
        }

        if latest_text.trim().is_empty() {
            text_outgoing("处理完成")
        } else {
            text_outgoing(latest_text)
        }
    }
}

fn remote_session_key(connector: &str, channel_id: &str) -> String {
    format!("{}:{}", connector.trim(), channel_id.trim())
}

fn remote_session_title(connector: &str, channel_id: &str) -> String {
    let connector = connector.trim();
    let channel_id = channel_id.trim();
    let raw = if connector.is_empty() {
        format!("外部通道 {channel_id}")
    } else {
        format!("{connector} {channel_id}")
    };
    raw.chars().take(80).collect()
}

fn text_outgoing(text: impl Into<String>) -> OutgoingMessage {
    OutgoingMessage {
        content: MessageContent::Text(text.into()),
        reply_to: None,
    }
}

fn media_outgoing(media: MediaAsset, caption: String) -> OutgoingMessage {
    let caption = (!caption.trim().is_empty()).then_some(caption);
    let content = match media.kind {
        MediaKind::Image => MessageContent::Image {
            url: media.url,
            caption,
        },
        MediaKind::Video => MessageContent::Video {
            url: media.url,
            caption,
        },
        MediaKind::Audio => MessageContent::Audio {
            url: media.url,
            duration: None,
        },
        MediaKind::File => MessageContent::File {
            name: media.title.unwrap_or_else(|| "文件".to_string()),
            url: media.url,
        },
    };
    OutgoingMessage {
        content,
        reply_to: None,
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
        let timeout = Duration::from_secs(300);
        let mut state = self.state.lock().unwrap();
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(outcome) = state.outcomes.remove(&turn_id) {
                return outcome;
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                state.current_turn_id = None;
                return TurnOutcome::Failed("执行超时（300 秒）".to_string());
            }
            match self.notify.wait_timeout(state, remaining) {
                Ok((guard, timed_out)) => {
                    state = guard;
                    if timed_out.timed_out() {
                        state.current_turn_id = None;
                        return TurnOutcome::Failed("执行超时（300 秒）".to_string());
                    }
                }
                Err(poisoned) => {
                    state = poisoned.into_inner().0;
                }
            }
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
            media,
        } if !session
            .messages
            .iter()
            .any(|message| message.id == *message_id) =>
        {
            session.append_message_with_id_and_media(
                message_id.clone(),
                MessageRole::User,
                content.clone(),
                String::new(),
                media.clone(),
            );
            should_persist = true;
        }
        StreamEvent::Delta {
            message_id,
            content,
        }
        | StreamEvent::ReactText {
            message_id,
            content,
        }
        | StreamEvent::SummaryText {
            message_id,
            content,
        } => append_assistant_delta(session, message_id, content),
        StreamEvent::PhaseChanged { .. } => {}
        StreamEvent::Reasoning {
            message_id,
            content,
        } => append_assistant_reasoning(session, message_id, content),
        StreamEvent::ToolCalls {
            message_id,
            names,
            calls,
            ..
        } => {
            finalize_assistant_tool_calls(session, message_id, calls);
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
        StreamEvent::ToolResult {
            name,
            tool_call_id,
            ok,
            output,
            full_output,
            media,
        } => {
            let persisted_output = full_output.as_deref().unwrap_or(output);
            let status = if *ok { "成功" } else { "失败" };
            if *ok && !media.is_empty() {
                session.append_message_with_media(
                    MessageRole::Assistant,
                    String::new(),
                    media.clone(),
                );
            } else if *ok {
                let parsed_media = parse_tool_media_assets(name, persisted_output);
                if !parsed_media.is_empty() {
                    session.append_message_with_media(
                        MessageRole::Assistant,
                        String::new(),
                        parsed_media,
                    );
                }
            }
            session.append_message(
                MessageRole::System,
                format!("工具 {name} 执行{status}\n{persisted_output}"),
            );
            append_tool_result_message(
                session,
                tool_call_id.as_deref(),
                name,
                persisted_output.to_string(),
                !*ok,
            );
        }
        StreamEvent::TokenUsage {
            usage,
            current_tokens,
            compression_threshold_tokens,
            context_limit_tokens,
            agent_id,
            ..
        } => {
            if usage.total_tokens > 0 {
                session.token_usage.accumulate(usage);
            }
            if let Some(current_tokens) = current_tokens {
                if let Some(aid) = agent_id {
                    session.active_agent_id = Some(aid.clone());
                    session.active_agent_current_tokens =
                        *current_tokens.max(&session.active_agent_current_tokens);
                } else {
                    session.current_tokens = (*current_tokens).max(session.current_tokens);
                }
            }
            if let Some(compression_threshold_tokens) = compression_threshold_tokens {
                session.compression_threshold_tokens = *compression_threshold_tokens;
            }
            if let Some(context_limit_tokens) = context_limit_tokens {
                session.context_limit_tokens = *context_limit_tokens;
            }
            should_persist = true;
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
        let _ = state.persist_session(session_id);
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
    if content.trim().is_empty()
        && !session
            .messages
            .iter()
            .any(|message| message.id == message_id)
    {
        return;
    }
    ensure_assistant_message(session, message_id);
    if let Some(message) = session
        .messages
        .iter_mut()
        .find(|message| message.id == message_id)
    {
        if message.text_content().trim().is_empty() && content.trim().is_empty() {
            return;
        }
        match message.content.last_mut() {
            Some(tiangong_types::ContentBlock::Text { text }) => text.push_str(content),
            _ => message
                .content
                .push(tiangong_types::ContentBlock::text(content.to_string())),
        }
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

fn cleanup_latest_assistant_before_tool_calls(session: &mut Session) {
    let Some(index) = session
        .messages
        .iter()
        .rposition(|message| message.role == MessageRole::Assistant)
    else {
        return;
    };

    let message = &mut session.messages[index];
    if !message.text_content().trim().is_empty() {
        return;
    }
    message.content.clear();
    if message.reasoning_content.trim().is_empty() && !message.has_media() {
        session.messages.remove(index);
    }
}

fn finalize_assistant_tool_calls(
    session: &mut Session,
    message_id: &str,
    calls: &[tiangong_types::StreamToolCall],
) {
    if calls.is_empty() {
        cleanup_latest_assistant_before_tool_calls(session);
        return;
    }
    ensure_assistant_message(session, message_id);
    if let Some(message) = session
        .messages
        .iter_mut()
        .find(|message| message.id == message_id)
    {
        message.tool_calls = calls
            .iter()
            .map(|call| MessageToolCall {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            })
            .collect();
    }
}

fn append_tool_result_message(
    session: &mut Session,
    tool_call_id: Option<&str>,
    tool_name: &str,
    content: String,
    is_error: bool,
) {
    let Some(tool_call_id) = tool_call_id else {
        return;
    };
    let mut message = Message::new(MessageRole::Tool, content);
    message.tool_call_id = Some(tool_call_id.to_string());
    message.tool_name = Some(tool_name.to_string());
    message.tool_result_is_error = is_error;
    session.messages.push(message);
    session.updated_at = now_text();
}

fn parse_tool_media_assets(name: &str, output: &str) -> Vec<MediaAsset> {
    match name {
        "generate_image" => parse_image_assets(output),
        "generate_video" => parse_video_assets(output),
        _ => Vec::new(),
    }
}

fn parse_image_assets(output: &str) -> Vec<MediaAsset> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if !line.starts_with("![") || !line.ends_with(')') {
                return None;
            }
            let close_alt = line.find("](")?;
            let title = line[2..close_alt].trim();
            let url = line[close_alt + 2..line.len() - 1].trim();
            if url.is_empty() {
                return None;
            }
            Some(MediaAsset {
                kind: MediaKind::Image,
                url: url.to_string(),
                mime_type: None,
                title: (!title.is_empty()).then(|| title.to_string()),
                capability: Some("image_generation".to_string()),
            })
        })
        .collect()
}

fn parse_video_assets(output: &str) -> Vec<MediaAsset> {
    output
        .lines()
        .filter_map(|line| {
            let url = line
                .trim()
                .strip_prefix("Video URL:")
                .or_else(|| line.trim().strip_prefix("video_url:"))
                .map(str::trim)?;
            let url = url.split_whitespace().next().unwrap_or(url);
            (url.starts_with("http://") || url.starts_with("https://")).then(|| MediaAsset {
                kind: MediaKind::Video,
                url: url.to_string(),
                mime_type: Some("video/mp4".to_string()),
                title: Some("生成的视频".to_string()),
                capability: Some("video_generation".to_string()),
            })
        })
        .collect()
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
