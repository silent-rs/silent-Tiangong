use std::time::Duration;

#[derive(Debug, Clone)]
pub struct AnthropicConfig {
    pub api_key: String,
    pub base_url: String,
    pub timeout: Duration,
    pub api_version: String,
    pub beta: Option<String>,
}

impl AnthropicConfig {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: "https://api.anthropic.com".to_string(),
            timeout: Duration::from_secs(60),
            api_version: "2023-06-01".to_string(),
            beta: None,
        }
    }
}
