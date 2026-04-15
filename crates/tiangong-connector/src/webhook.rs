use anyhow::Result;
use async_trait::async_trait;
use tiangong_types::OutgoingMessage;

use crate::traits::{Connector, ConnectorStatus};

pub struct WebhookConnector {
    name: String,
    endpoint: String,
    running: bool,
}

impl WebhookConnector {
    pub fn new(name: String, endpoint: String) -> Self {
        Self {
            name,
            endpoint,
            running: false,
        }
    }
}

#[async_trait]
impl Connector for WebhookConnector {
    fn name(&self) -> &str {
        &self.name
    }

    async fn start(&mut self) -> Result<()> {
        tracing::info!(
            connector = %self.name,
            endpoint = %self.endpoint,
            "Webhook connector 已启动"
        );
        self.running = true;
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        tracing::info!(connector = %self.name, "Webhook connector 已停止");
        self.running = false;
        Ok(())
    }

    async fn send_message(&self, channel_id: &str, message: &OutgoingMessage) -> Result<()> {
        tracing::info!(
            connector = %self.name,
            channel = %channel_id,
            "发送消息到 webhook"
        );
        // TODO: 实际 HTTP POST 到 endpoint
        let _ = message;
        Ok(())
    }

    async fn health_check(&self) -> Result<ConnectorStatus> {
        Ok(if self.running {
            ConnectorStatus::Running
        } else {
            ConnectorStatus::Stopped
        })
    }
}
