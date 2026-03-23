use anyhow::Result;
use async_trait::async_trait;
use tiangong_gateway::message::{IncomingMessage, OutgoingMessage};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectorStatus {
    Running,
    Stopped,
    Error(String),
}

/// Connector trait — 所有通道适配器必须实现
#[async_trait]
pub trait Connector: Send + Sync {
    fn name(&self) -> &str;
    async fn start(&mut self) -> Result<()>;
    async fn stop(&mut self) -> Result<()>;
    async fn send_message(&self, channel_id: &str, message: &OutgoingMessage) -> Result<()>;
    async fn health_check(&self) -> Result<ConnectorStatus>;
}

/// 消息发送器回调类型
pub type MessageSender = Box<dyn Fn(IncomingMessage) + Send + Sync>;
