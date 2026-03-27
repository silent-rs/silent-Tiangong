use tokio::sync::broadcast;

use crate::message::{IncomingMessage, OutgoingMessage};

/// 事件类型（简化版，后续按需扩展）
#[derive(Debug, Clone)]
pub enum TiangongEvent {
    // 会话事件
    MessageReceived(IncomingMessage),
    MessageSent(OutgoingMessage),
    SessionCreated(String),

    // Agent 事件
    TurnCompleted { session_id: String, success: bool },

    // Connector 事件
    ConnectorStarted(String),
    ConnectorStopped(String),
    ConnectorError { name: String, error: String },

    // 系统事件
    ConfigChanged,
    Shutdown,
}

/// 事件总线
pub struct EventBus {
    sender: broadcast::Sender<TiangongEvent>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    /// 发布事件到总线，忽略无接收者错误
    pub fn publish(&self, event: TiangongEvent) {
        let _ = self.sender.send(event);
    }

    /// 订阅事件总线
    pub fn subscribe(&self) -> broadcast::Receiver<TiangongEvent> {
        self.sender.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(256)
    }
}
