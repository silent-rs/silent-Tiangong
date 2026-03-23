use std::sync::Arc;

use anyhow::Result;
use tiangong_core::app_state::TiangongState;
use tiangong_core::session::MessageRole;
use tokio::sync::Mutex;

use crate::event::{EventBus, TiangongEvent};
use crate::message::{IncomingMessage, MessageContent, OutgoingMessage};

pub struct MessageRouter {
    state: Arc<Mutex<TiangongState>>,
    event_bus: Arc<EventBus>,
}

impl MessageRouter {
    pub fn new(state: Arc<Mutex<TiangongState>>, event_bus: Arc<EventBus>) -> Self {
        Self { state, event_bus }
    }

    /// 处理入站消息：分发到 Agent 执行
    pub async fn handle_incoming(&self, msg: IncomingMessage) -> Result<OutgoingMessage> {
        self.event_bus
            .publish(TiangongEvent::MessageReceived(msg.clone()));

        let response_text = {
            let mut state = self.state.lock().await;
            // 创建或获取 connector 对应的会话
            state.update_draft(match &msg.content {
                MessageContent::Text(text) => text.clone(),
                _ => "[非文本消息]".to_string(),
            });
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

            // 获取最新回复
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
}
