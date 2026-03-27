use std::sync::Arc;

use async_channel::Sender as UnboundedSender;
use async_lock::RwLock;
use silent::prelude::*;
use silent::ws::{WSHandlerAppend, WebSocketHandler, WebSocketParts};

use super::SharedState;
use tiangong_gateway::event::{EventBus, TiangongEvent};
use tiangong_gateway::message::MessageContent;

/// 共享的 EventBus 类型（注入到 Configs 中）
#[derive(Clone)]
pub struct SharedEventBus(pub Arc<EventBus>);

/// 构建 WebSocket 路由: GET /api/v1/ws
pub fn ws_route() -> Route {
    let handler = WebSocketHandler::new()
        .on_connect(on_connect)
        .on_send(on_send)
        .on_receive(on_receive)
        .on_close(on_close);

    Route::new("ws").ws(None, handler)
}

/// WebSocket 连接建立时：订阅 EventBus 并持续推送事件到客户端
async fn on_connect(
    parts: Arc<RwLock<WebSocketParts>>,
    sender: UnboundedSender<Message>,
) -> Result<()> {
    // 从 extensions 中获取 EventBus
    let event_bus = {
        let parts_read = parts.read().await;
        parts_read
            .extensions()
            .get::<SharedEventBus>()
            .cloned()
            .map(|eb| eb.0)
    };

    let Some(event_bus) = event_bus else {
        tracing::warn!("WebSocket 连接未找到 EventBus，无法推送事件");
        return Ok(());
    };

    let mut receiver = event_bus.subscribe();
    let sender_clone = sender.clone();

    // 在后台任务中持续推送事件
    tokio::spawn(async move {
        loop {
            match receiver.recv().await {
                Ok(event) => {
                    let json = event_to_json(&event);
                    if sender_clone.send(Message::text(json)).await.is_err() {
                        // 客户端已断开
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("WebSocket 事件推送落后 {n} 条消息");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
    });

    tracing::info!("WebSocket 客户端已连接");
    Ok(())
}

/// 发送消息前的回调（直接透传）
async fn on_send(msg: Message, _parts: Arc<RwLock<WebSocketParts>>) -> Result<Message> {
    Ok(msg)
}

/// 收到客户端消息时：通过 MessageRouter 处理
async fn on_receive(msg: Message, parts: Arc<RwLock<WebSocketParts>>) -> Result<()> {
    let text = match msg.to_str() {
        Ok(t) => t.to_string(),
        Err(_) => {
            tracing::warn!("收到非文本 WebSocket 消息，忽略");
            return Ok(());
        }
    };

    // 尝试解析客户端消息
    // TODO: 后续通过 MessageRouter 处理完整消息路由
    let parts_read = parts.read().await;
    let state = parts_read.extensions().get::<SharedState>().cloned();
    drop(parts_read);

    if let Some(state) = state {
        let mut app = state.lock().await;
        app.update_draft(text);
        if let Err(e) = app.send_current_input() {
            tracing::error!("WebSocket 消息处理失败: {e}");
        }
    } else {
        tracing::warn!("WebSocket 未找到 SharedState，无法处理消息");
    }

    Ok(())
}

/// WebSocket 连接关闭
async fn on_close(_parts: Arc<RwLock<WebSocketParts>>) {
    tracing::info!("WebSocket 客户端已断开");
}

/// 将 TiangongEvent 转为 JSON 字符串
fn event_to_json(event: &TiangongEvent) -> String {
    match event {
        TiangongEvent::MessageReceived(msg) => serde_json::json!({
            "type": "message_received",
            "data": {
                "id": msg.id,
                "connector": msg.connector,
                "channel_id": msg.channel_id,
                "sender_id": msg.sender_id,
            }
        })
        .to_string(),
        TiangongEvent::MessageSent(msg) => {
            let content_text = match &msg.content {
                MessageContent::Text(t) => t.clone(),
                _ => "[非文本内容]".to_string(),
            };
            serde_json::json!({
                "type": "message_sent",
                "data": {
                    "content": content_text,
                    "reply_to": msg.reply_to,
                }
            })
            .to_string()
        }
        TiangongEvent::SessionCreated(id) => serde_json::json!({
            "type": "session_created",
            "data": { "session_id": id }
        })
        .to_string(),
        TiangongEvent::TurnCompleted {
            session_id,
            success,
        } => serde_json::json!({
            "type": "turn_completed",
            "data": {
                "session_id": session_id,
                "success": success,
            }
        })
        .to_string(),
        TiangongEvent::ConnectorStarted(name) => serde_json::json!({
            "type": "connector_started",
            "data": { "name": name }
        })
        .to_string(),
        TiangongEvent::ConnectorStopped(name) => serde_json::json!({
            "type": "connector_stopped",
            "data": { "name": name }
        })
        .to_string(),
        TiangongEvent::ConnectorError { name, error } => serde_json::json!({
            "type": "connector_error",
            "data": { "name": name, "error": error }
        })
        .to_string(),
        TiangongEvent::ConfigChanged => serde_json::json!({ "type": "config_changed" }).to_string(),
        TiangongEvent::Shutdown => serde_json::json!({ "type": "shutdown" }).to_string(),
    }
}
