use std::sync::Arc;

use anyhow::Result;
use tiangong_core::app_state::TiangongState;
use tiangong_core::core_config::CoreConfigProvider;
use tiangong_core::event::{EventSource, RuntimeEvent, RuntimeEventType};
use tiangong_core::session::MessageRole;
use tiangong_core::task::TaskNotification;
use tiangong_media::agent::MediaAgent;
use tiangong_media::stt::TranscribeRequest;
use tokio::sync::Mutex;

use crate::event::{EventBus, TiangongEvent};
use crate::message::{IncomingMessage, MessageContent, OutgoingMessage};

pub struct MessageRouter {
    state: Arc<Mutex<TiangongState>>,
    event_bus: Arc<EventBus>,
    media_agent: Option<Arc<MediaAgent>>,
    core_config: Option<CoreConfigProvider>,
}

impl MessageRouter {
    pub fn new(state: Arc<Mutex<TiangongState>>, event_bus: Arc<EventBus>) -> Self {
        Self {
            state,
            event_bus,
            media_agent: None,
            core_config: None,
        }
    }

    /// 设置 CoreConfigProvider，使远程入口在处理消息前同步 Core 配置快照
    pub fn with_core_config_provider(mut self, provider: CoreConfigProvider) -> Self {
        self.core_config = Some(provider);
        self
    }

    /// 设置 MediaAgent（启用语音转文字等能力）
    pub fn with_media_agent(mut self, agent: Arc<MediaAgent>) -> Self {
        self.media_agent = Some(agent);
        self
    }

    /// 处理入站消息：分发到 Agent 执行
    ///
    /// 根据发送者角色限制可执行操作，音频消息自动 STT 转文字。
    pub async fn handle_incoming(&self, msg: IncomingMessage) -> Result<OutgoingMessage> {
        // 角色权限检查
        if !msg.sender_role.can_send_message() {
            let denied_text = format!(
                "权限不足：{}角色不允许发送消息",
                msg.sender_role.display_name()
            );
            return Ok(OutgoingMessage {
                content: MessageContent::Text(denied_text),
                reply_to: Some(msg.id.clone()),
            });
        }

        self.event_bus
            .publish(TiangongEvent::MessageReceived(msg.clone()));

        // 提取文本内容，音频消息自动转文字
        let text = self.extract_text(&msg).await;
        let session_id = if msg.channel_id.trim().is_empty() {
            let state = self.state.lock().await;
            state.active_session_id().to_string()
        } else {
            msg.channel_id.clone()
        };
        let event = RuntimeEvent::new(
            RuntimeEventType::UserMessage,
            session_id,
            EventSource::Connector,
            serde_json::json!({
                "connector": msg.connector,
                "sender_id": msg.sender_id,
                "sender_role": msg.sender_role,
                "text": text,
            }),
        );

        let Some(outgoing) = self
            .handle_runtime_event_with_reply(event, Some(msg.id.clone()))
            .await?
        else {
            return Ok(OutgoingMessage {
                content: MessageContent::Text("处理完成".to_string()),
                reply_to: Some(msg.id.clone()),
            });
        };

        Ok(outgoing)
    }

    /// 处理统一运行时事件入口。
    ///
    /// 远程输入、后台任务通知、系统信号都通过此入口接入，
    /// 避免不同来源事件走各自独立的状态更新分支。
    pub async fn handle_runtime_event(
        &self,
        event: RuntimeEvent,
    ) -> Result<Option<OutgoingMessage>> {
        self.handle_runtime_event_with_reply(event, None).await
    }

    /// 将后台任务通知转换为统一运行时事件入口。
    pub async fn handle_task_notification(
        &self,
        notification: TaskNotification,
    ) -> Result<Option<OutgoingMessage>> {
        let Some(event) = notification.to_runtime_event() else {
            return Ok(None);
        };
        self.handle_runtime_event(event).await
    }

    /// 将系统信号转换为统一运行时事件入口。
    pub async fn handle_system_signal(
        &self,
        session_id: impl Into<String>,
        signal: impl Into<String>,
        payload: serde_json::Value,
    ) -> Result<Option<OutgoingMessage>> {
        let signal = signal.into();
        let mut envelope = serde_json::Map::new();
        envelope.insert("signal".to_string(), serde_json::Value::String(signal));
        envelope.insert("payload".to_string(), payload);
        let event = RuntimeEvent::new(
            RuntimeEventType::SystemSignal,
            session_id.into(),
            EventSource::System,
            serde_json::Value::Object(envelope),
        );
        self.handle_runtime_event(event).await
    }

    /// 从消息中提取文本，音频自动 STT
    async fn extract_text(&self, msg: &IncomingMessage) -> String {
        match &msg.content {
            MessageContent::Text(text) => text.clone(),
            MessageContent::Audio { url, .. } => self.try_transcribe_audio(url).await,
            MessageContent::Image { caption, .. } => {
                caption.clone().unwrap_or_else(|| "[图片消息]".to_string())
            }
            MessageContent::Video { caption, .. } => {
                caption.clone().unwrap_or_else(|| "[视频消息]".to_string())
            }
            MessageContent::File { name, .. } => {
                format!("[文件: {name}]")
            }
        }
    }

    /// 尝试通过 STT 转录音频 URL
    async fn try_transcribe_audio(&self, url: &str) -> String {
        let Some(media_agent) = &self.media_agent else {
            tracing::warn!("收到音频消息但未配置 MediaAgent，无法转文字");
            return "[语音消息，未配置语音识别]".to_string();
        };

        if !media_agent.has_speech_recognizer() {
            tracing::warn!("收到音频消息但未配置 SpeechRecognizer");
            return "[语音消息，未配置语音识别]".to_string();
        }

        // 下载音频数据
        let audio_data = match download_url(url).await {
            Ok(data) => data,
            Err(err) => {
                tracing::error!(url = %url, error = %err, "下载音频失败");
                return "[语音消息，下载失败]".to_string();
            }
        };

        // 推断 MIME 类型
        let mime_type = if url.ends_with(".ogg") || url.ends_with(".oga") {
            "audio/ogg"
        } else if url.ends_with(".mp3") {
            "audio/mp3"
        } else if url.ends_with(".wav") {
            "audio/wav"
        } else {
            "audio/ogg" // 大多数 IM 语音消息是 ogg 格式
        };

        let request = TranscribeRequest {
            audio: audio_data,
            mime_type: mime_type.to_string(),
            language: None,
            model: None,
        };

        match media_agent.transcribe(request).await {
            Ok(response) => {
                tracing::info!(
                    text_len = response.text.len(),
                    language = ?response.language,
                    "语音转文字成功"
                );
                response.text
            }
            Err(err) => {
                tracing::error!(error = %err, "语音转文字失败");
                "[语音消息，识别失败]".to_string()
            }
        }
    }

    async fn handle_runtime_event_with_reply(
        &self,
        event: RuntimeEvent,
        reply_to: Option<String>,
    ) -> Result<Option<OutgoingMessage>> {
        match event.event_type {
            RuntimeEventType::UserMessage => {
                let requested_session_id = event.session_id.clone();
                let text = event
                    .payload
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .unwrap_or_default()
                    .to_string();
                if text.is_empty() {
                    return Ok(None);
                }

                let response_text = {
                    let mut state = self.state.lock().await;
                    let actual_session_id =
                        prepare_active_session_for_input(&mut state, &requested_session_id);
                    if let Some(provider) = &self.core_config {
                        let base = provider.snapshot();
                        let next = state.build_core_config_from_base(&base);
                        provider.replace(next);
                    }
                    state.update_draft(text);
                    state.send_current_input()?;

                    loop {
                        state.poll_pending_turn();
                        if !state.has_pending_turn_for(&actual_session_id) {
                            break;
                        }
                        drop(state);
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        state = self.state.lock().await;
                    }

                    state
                        .active_session()
                        .and_then(|s| s.messages.last())
                        .filter(|m| m.role == MessageRole::Assistant)
                        .map(|m| m.content.clone())
                        .unwrap_or_else(|| "处理完成".to_string())
                };

                let outgoing = OutgoingMessage {
                    content: MessageContent::Text(response_text),
                    reply_to,
                };
                self.event_bus
                    .publish(TiangongEvent::MessageSent(outgoing.clone()));
                Ok(Some(outgoing))
            }
            RuntimeEventType::TaskCompleted
            | RuntimeEventType::TaskFailed
            | RuntimeEventType::Notification
            | RuntimeEventType::SystemSignal => {
                let Some(summary) = summarize_runtime_event(&event) else {
                    return Ok(None);
                };

                {
                    let mut state = self.state.lock().await;
                    state.append_session_message(
                        &event.session_id,
                        MessageRole::System,
                        summary.clone(),
                    )?;
                }

                let outgoing = OutgoingMessage {
                    content: MessageContent::Text(summary),
                    reply_to,
                };
                self.event_bus
                    .publish(TiangongEvent::MessageSent(outgoing.clone()));
                Ok(Some(outgoing))
            }
            _ => Ok(None),
        }
    }
}

fn prepare_active_session_for_input(
    state: &mut TiangongState,
    requested_session_id: &str,
) -> String {
    let session_exists = state
        .sessions()
        .iter()
        .any(|session| session.id == requested_session_id);
    if session_exists && state.active_session_id() != requested_session_id {
        state.switch_session(requested_session_id);
    }
    state.active_session_id().to_string()
}

fn summarize_runtime_event(event: &RuntimeEvent) -> Option<String> {
    match event.event_type {
        RuntimeEventType::TaskCompleted => Some(
            event
                .payload
                .get("summary")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| "后台任务已完成".to_string()),
        ),
        RuntimeEventType::TaskFailed => Some(
            event
                .payload
                .get("summary")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| "后台任务执行失败".to_string()),
        ),
        RuntimeEventType::Notification => Some(
            event
                .payload
                .get("summary")
                .or_else(|| event.payload.get("message"))
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| "收到系统通知".to_string()),
        ),
        RuntimeEventType::SystemSignal => Some(
            event
                .payload
                .get("signal")
                .and_then(serde_json::Value::as_str)
                .map(|signal| format!("系统信号：{signal}"))
                .unwrap_or_else(|| "收到系统信号".to_string()),
        ),
        _ => None,
    }
}

/// 下载 URL 内容
async fn download_url(url: &str) -> Result<Vec<u8>> {
    // 简单的 HTTP GET 下载
    // 实际使用时应复用 reqwest::Client
    let response = reqwest::get(url).await?;
    let bytes = response.bytes().await?;
    Ok(bytes.to_vec())
}
