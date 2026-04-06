use std::sync::Arc;

use anyhow::Result;
use tiangong_core::app_state::TiangongState;
use tiangong_core::session::MessageRole;
use tiangong_media::agent::MediaAgent;
use tiangong_media::stt::TranscribeRequest;
use tokio::sync::Mutex;

use crate::event::{EventBus, TiangongEvent};
use crate::message::{IncomingMessage, MessageContent, OutgoingMessage};

pub struct MessageRouter {
    state: Arc<Mutex<TiangongState>>,
    event_bus: Arc<EventBus>,
    media_agent: Option<Arc<MediaAgent>>,
}

impl MessageRouter {
    pub fn new(state: Arc<Mutex<TiangongState>>, event_bus: Arc<EventBus>) -> Self {
        Self {
            state,
            event_bus,
            media_agent: None,
        }
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

        let response_text = {
            let mut state = self.state.lock().await;
            state.update_draft(text);
            state.send_current_input()?;

            // 轮询等待结果
            loop {
                state.poll_pending_turn();
                if !state.has_pending_turn() {
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
            reply_to: Some(msg.id.clone()),
        };
        self.event_bus
            .publish(TiangongEvent::MessageSent(outgoing.clone()));
        Ok(outgoing)
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
}

/// 下载 URL 内容
async fn download_url(url: &str) -> Result<Vec<u8>> {
    // 简单的 HTTP GET 下载
    // 实际使用时应复用 reqwest::Client
    let response = reqwest::get(url).await?;
    let bytes = response.bytes().await?;
    Ok(bytes.to_vec())
}
