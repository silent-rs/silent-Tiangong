use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tiangong_core::agent_input::{AgentInput, AgentInputKind};
use tiangong_core::core::TiangongCore;
use tiangong_core::core_config::CoreConfigProvider;
use tiangong_core::permission::TrustMode;
use tiangong_core::session::{Message, MessageRole, MessageToolCall, Session};
use tiangong_scheduler::executor::SchedulerContext;
use tokio::sync::Mutex as AsyncMutex;
use tracing::debug;

type SchedulerWaiter = tokio::sync::oneshot::Sender<Result<(), String>>;
type SchedulerWaiters = Arc<std::sync::Mutex<HashMap<String, SchedulerWaiter>>>;

/// Desktop 端调度器执行上下文
///
/// 使用 TiangongApp 共享的 state 和 config，维护独立的定时任务 core map。
/// 定时任务 core 运行在 FullTrust 模式下，与 UI 核心隔离。
pub struct DesktopSchedulerContext {
    state: Arc<AsyncMutex<tiangong_app_state::app_state::TiangongState>>,
    config: CoreConfigProvider,
    scheduler_cores: Arc<std::sync::Mutex<HashMap<String, TiangongCore>>>,
    scheduler_session_locks: std::sync::Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    scheduler_waiters: SchedulerWaiters,
}

impl DesktopSchedulerContext {
    pub fn new(
        state: Arc<AsyncMutex<tiangong_app_state::app_state::TiangongState>>,
        config: CoreConfigProvider,
    ) -> Self {
        Self {
            state,
            config,
            scheduler_cores: Arc::new(std::sync::Mutex::new(HashMap::new())),
            scheduler_session_locks: std::sync::Mutex::new(HashMap::new()),
            scheduler_waiters: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    fn scheduler_session_lock(&self, session_id: &str) -> Arc<AsyncMutex<()>> {
        self.scheduler_session_locks
            .lock()
            .unwrap()
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }
}

impl Drop for DesktopSchedulerContext {
    fn drop(&mut self) {
        // 解除 stream thread → cores map → Core sender → stream receiver 的生命周期环。
        self.scheduler_cores.lock().unwrap().clear();
        let mut waiters = self.scheduler_waiters.lock().unwrap();
        for (_, waiter) in waiters.drain() {
            let _ = waiter.send(Err("调度器正在关闭".to_string()));
        }
    }
}

#[async_trait]
impl SchedulerContext for DesktopSchedulerContext {
    async fn send_message(&self, session_id: &str, content: String) -> anyhow::Result<()> {
        let session_lock = self.scheduler_session_lock(session_id);
        let _session_guard = session_lock.lock().await;
        // 确保 core 存在
        let needs_create = {
            let cores = self.scheduler_cores.lock().unwrap();
            !cores.contains_key(session_id)
        };
        if needs_create {
            self.ensure_scheduler_core(session_id).await?;
        }

        let (completion_tx, completion_rx) = tokio::sync::oneshot::channel();
        self.scheduler_waiters
            .lock()
            .unwrap()
            .insert(session_id.to_string(), completion_tx);
        let delivery_result = {
            let cores = self.scheduler_cores.lock().unwrap();
            cores
                .get(session_id)
                .map(|core| core.deliver(AgentInputKind::message(content)))
        };
        if !matches!(delivery_result, Some(Ok(()))) {
            self.scheduler_waiters.lock().unwrap().remove(session_id);
            return Err(anyhow::anyhow!(
                "定时任务 core 不存在或命令通道已关闭：{session_id}"
            ));
        }
        match completion_rx.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(anyhow::anyhow!(error)),
            Err(_) => Err(anyhow::anyhow!("定时任务 core 事件流已关闭")),
        }
    }

    async fn resolve_or_create_session(
        &self,
        requested_session_id: Option<&str>,
        trigger_name: &str,
    ) -> anyhow::Result<(String, bool)> {
        if let Some(sid) = requested_session_id {
            let state = self.state.lock().await;
            if state.sessions().iter().any(|s| s.id == *sid) {
                return Ok((sid.to_string(), false));
            }
        }

        let mut state = self.state.lock().await;
        let title = format!("定时任务：{}", trigger_name);
        let session = tiangong_core::session::Session::new_isolated(title);
        let session_id = session.id.clone();
        state.sessions_mut().push(session);
        state.persist_session(&session_id)?;
        Ok((session_id, true))
    }
}

impl DesktopSchedulerContext {
    /// 为定时任务创建 core 并启动流消费线程
    async fn ensure_scheduler_core(&self, session_id: &str) -> anyhow::Result<()> {
        let session = {
            let state = self.state.lock().await;
            state
                .sessions()
                .iter()
                .find(|s| s.id == session_id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("定时任务会话不存在：{session_id}"))?
        };

        let (stream_tx, stream_rx) =
            std::sync::mpsc::channel::<tiangong_types::SessionStreamEvent>();
        let core = TiangongCore::builder()
            .config(self.config.clone())
            .session(session)
            .event_sender(stream_tx)
            .plugins(tiangong_plugin_scheduler::default_plugins())
            .storage(tiangong_core::core::CoreStorageLocation::new(
                tiangong_app_state::app_state::storage_root(),
            ))
            .build()?;
        core.set_trust_mode(TrustMode::FullTrust);

        {
            let mut cores = self.scheduler_cores.lock().unwrap();
            cores.insert(session_id.to_string(), core);
        }

        // 启动后台线程消费流事件并持久化到 state
        let state = self.state.clone();
        let waiters = self.scheduler_waiters.clone();
        let cores = self.scheduler_cores.clone();
        let sid = session_id.to_string();
        let runtime_handle = tokio::runtime::Handle::current();
        std::thread::Builder::new()
            .name(format!("scheduler-stream-{sid}"))
            .spawn(move || {
                for session_event in stream_rx {
                    let event = session_event.event;
                    let terminal = match &event {
                        tiangong_types::StreamEvent::Done { .. } => Some(Ok(())),
                        tiangong_types::StreamEvent::Error { message } => {
                            Some(Err(message.clone()))
                        }
                        _ => None,
                    };
                    let final_user_snapshot = matches!(
                        &event,
                        tiangong_types::StreamEvent::SessionMessageUpsert { message, .. }
                            if message.role == MessageRole::User && message.turn_status.is_some()
                    );
                    runtime_handle.block_on(async {
                        let mut state = state.lock().await;
                        if final_user_snapshot {
                            if let Err(error) = state.reload_session_from_disk(&sid) {
                                tracing::warn!(%error, session_id = %sid, "刷新定时任务会话失败");
                            }
                            return;
                        }
                        if let Some(session) = state
                            .sessions_mut()
                            .iter_mut()
                            .find(|session| session.id == sid)
                        {
                            apply_scheduler_stream_event(session, &event);
                        }
                    });
                    if let Some(result) = terminal {
                        if let Some(waiter) = waiters.lock().unwrap().remove(&sid) {
                            let _ = waiter.send(result);
                        }
                    }
                }
                if let Some(waiter) = waiters.lock().unwrap().remove(&sid) {
                    let _ = waiter.send(Err("定时任务 core 事件流已关闭".to_string()));
                }
                let mut cores = cores.lock().unwrap();
                if cores.get(&sid).is_some_and(TiangongCore::is_stopped) {
                    cores.remove(&sid);
                }
                debug!(session_id = %sid, "定时任务流消费线程退出");
            })?;

        Ok(())
    }
}

fn apply_scheduler_stream_event(session: &mut Session, event: &tiangong_types::StreamEvent) {
    use tiangong_types::StreamEvent;

    match event {
        StreamEvent::UserMessage {
            message_id,
            content,
            content_blocks,
            media,
            model_excluded,
            pending_agent_deliveries,
        } => {
            let prepared = if content_blocks.is_empty() {
                let mut blocks = vec![tiangong_types::ContentBlock::text(content.clone())];
                blocks.extend(media.iter().map(|asset| asset.to_content_block()));
                tiangong_types::PreparedUserMessage::new(blocks).stable()
            } else {
                tiangong_types::PreparedUserMessage::new(content_blocks.clone()).stable()
            };
            let existing = session
                .messages
                .iter()
                .position(|message| message.id == *message_id && message.role == MessageRole::User)
                .map(|index| session.messages.remove(index));
            if let Some(mut message) = existing {
                if !content_blocks.is_empty()
                    || !media.is_empty()
                    || message.content.is_empty()
                    || message.text_content() != *content
                {
                    message.content = prepared.content;
                }
                message.model_excluded = *model_excluded;
                session.messages.push(message);
            } else {
                session.append_prepared_user_message_with_id(message_id.clone(), prepared);
                session.set_message_model_excluded(message_id, *model_excluded);
            }
            session.pending_agent_deliveries = pending_agent_deliveries.clone();
        }
        StreamEvent::SessionMessageUpsert {
            message,
            pending_agent_deliveries,
            deferred_tool_injections,
        } => {
            if let Some(existing) = session
                .messages
                .iter_mut()
                .find(|existing| existing.id == message.id)
            {
                *existing = message.clone();
            } else {
                session.messages.push(message.clone());
            }
            if let Some(deliveries) = pending_agent_deliveries {
                session.pending_agent_deliveries = deliveries.clone();
            }
            if let Some(injections) = deferred_tool_injections {
                session.deferred_tool_injections = injections.clone();
            }
        }
        StreamEvent::PendingAgentDeliveriesChanged { deliveries } => {
            session.pending_agent_deliveries = deliveries.clone();
        }
        StreamEvent::DeferredToolInjectionsChanged { injections } => {
            session.deferred_tool_injections = injections.clone();
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
        } => {
            ensure_scheduler_assistant_message(session, message_id);
            if let Some(message) = session.messages.iter_mut().find(|m| m.id == *message_id) {
                match message.content.last_mut() {
                    Some(tiangong_types::ContentBlock::Text { text }) => text.push_str(content),
                    _ => message
                        .content
                        .push(tiangong_types::ContentBlock::text(content.clone())),
                }
            }
        }
        StreamEvent::Reasoning {
            message_id,
            content,
        } => {
            ensure_scheduler_assistant_message(session, message_id);
            if let Some(message) = session.messages.iter_mut().find(|m| m.id == *message_id) {
                message.reasoning_content.push_str(content);
            }
        }
        StreamEvent::ToolCalls {
            message_id, calls, ..
        } => {
            ensure_scheduler_assistant_message(session, message_id);
            if let Some(message) = session.messages.iter_mut().find(|m| m.id == *message_id) {
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
        StreamEvent::ToolResult {
            name,
            tool_call_id: Some(tool_call_id),
            ok,
            output,
            full_output,
            ..
        } => {
            let content = full_output.as_deref().unwrap_or(output).to_string();
            append_scheduler_tool_result(session, tool_call_id, name, content, !ok);
        }
        StreamEvent::TokenUsage {
            usage,
            current_tokens,
            compression_threshold_tokens,
            context_limit_tokens,
            agent_id,
            ..
        } => {
            session.token_usage.accumulate(usage);
            if let Some(current) = current_tokens {
                session.current_tokens = *current;
            }
            if let Some(threshold) = compression_threshold_tokens {
                session.compression_threshold_tokens = *threshold;
            }
            if let Some(limit) = context_limit_tokens {
                session.context_limit_tokens = *limit;
            }
            if let Some(agent_id) = agent_id {
                session
                    .agent_token_usage
                    .entry(agent_id.clone())
                    .or_default()
                    .accumulate(usage);
            }
        }
        _ => {}
    }
}

fn ensure_scheduler_assistant_message(session: &mut Session, message_id: &str) {
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

fn append_scheduler_tool_result(
    session: &mut Session,
    tool_call_id: &str,
    tool_name: &str,
    content: String,
    is_error: bool,
) {
    let assistant_index = session.messages.iter().rposition(|message| {
        message.role == MessageRole::Assistant
            && message
                .tool_calls
                .iter()
                .any(|call| call.id == tool_call_id)
    });
    let existing_result = assistant_index.and_then(|index| {
        session.messages[index + 1..]
            .iter()
            .take_while(|message| message.role == MessageRole::Tool)
            .position(|message| message.tool_call_id.as_deref() == Some(tool_call_id))
            .map(|offset| index + 1 + offset)
    });
    if let Some(message) = existing_result.and_then(|index| session.messages.get_mut(index)) {
        message.content = vec![tiangong_types::ContentBlock::text(content)];
        message.tool_result_is_error = is_error;
        return;
    }
    session.messages.push(Message::tool_result(
        tool_call_id,
        tool_name,
        content,
        is_error,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_reducer_keeps_authoritative_user_and_message_snapshots() {
        let mut session = Session::new("scheduler-state");
        apply_scheduler_stream_event(
            &mut session,
            &tiangong_types::StreamEvent::UserMessage {
                message_id: "user-1".to_string(),
                content: "hello".to_string(),
                content_blocks: vec![tiangong_types::ContentBlock::text("hello")],
                media: Vec::new(),
                model_excluded: false,
                pending_agent_deliveries: Vec::new(),
            },
        );
        let mut assistant = Message::new(MessageRole::Assistant, "done");
        assistant.id = "assistant-1".to_string();
        assistant.phase = tiangong_core::session::MessagePhase::Summary;
        apply_scheduler_stream_event(
            &mut session,
            &tiangong_types::StreamEvent::SessionMessageUpsert {
                message: assistant.clone(),
                pending_agent_deliveries: None,
                deferred_tool_injections: None,
            },
        );

        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].id, "user-1");
        assert_eq!(session.messages[1].id, assistant.id);
        assert_eq!(session.messages[1].text_content(), assistant.text_content());
        assert_eq!(session.messages[1].phase, assistant.phase);
    }
}
