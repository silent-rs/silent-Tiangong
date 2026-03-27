use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorConfig {
    pub name: String,
    pub connector_type: ConnectorType,
    pub enabled: bool,
    pub settings: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum ConnectorType {
    #[default]
    Webhook,
    Telegram,
    Discord,
    Lark,
}
