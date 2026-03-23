use std::collections::HashMap;

use anyhow::{Result, anyhow};

use crate::config::{ConnectorConfig, ConnectorType};
use crate::traits::{Connector, ConnectorStatus};
use crate::webhook::WebhookConnector;

pub struct ConnectorManager {
    connectors: HashMap<String, Box<dyn Connector>>,
}

impl ConnectorManager {
    pub fn new() -> Self {
        Self {
            connectors: HashMap::new(),
        }
    }

    /// 根据配置创建并注册 connector
    pub fn register_from_config(&mut self, config: &ConnectorConfig) -> Result<()> {
        match config.connector_type {
            ConnectorType::Webhook => {
                let endpoint = config
                    .settings
                    .get("endpoint")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let connector = WebhookConnector::new(config.name.clone(), endpoint);
                self.connectors
                    .insert(config.name.clone(), Box::new(connector));
            }
            _ => {
                tracing::warn!(connector = %config.name, "Connector 类型暂未实现");
            }
        }
        Ok(())
    }

    pub async fn start(&mut self, name: &str) -> Result<()> {
        let connector = self
            .connectors
            .get_mut(name)
            .ok_or_else(|| anyhow!("未找到 connector: {name}"))?;
        connector.start().await
    }

    pub async fn stop(&mut self, name: &str) -> Result<()> {
        let connector = self
            .connectors
            .get_mut(name)
            .ok_or_else(|| anyhow!("未找到 connector: {name}"))?;
        connector.stop().await
    }

    pub async fn health_check(&self, name: &str) -> Result<ConnectorStatus> {
        let connector = self
            .connectors
            .get(name)
            .ok_or_else(|| anyhow!("未找到 connector: {name}"))?;
        connector.health_check().await
    }

    pub fn list(&self) -> Vec<&str> {
        self.connectors.keys().map(|k| k.as_str()).collect()
    }
}

impl Default for ConnectorManager {
    fn default() -> Self {
        Self::new()
    }
}
