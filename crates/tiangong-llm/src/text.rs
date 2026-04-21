//! 简单文本完成工具。
//!
//! 供上层 crate 复用统一 provider 构建与纯文本调用，避免各业务 crate
//! 重复实现 OpenAI/Anthropic 分支。

use std::time::Duration;

use crate::error::LlmError;
use crate::message::{ChatMessage, MessageContent, MessageRole};
use crate::model::ProviderProtocol;
use crate::provider::LlmProvider;
use crate::providers::anthropic::{AnthropicConfig, AnthropicProvider};
use crate::providers::openai::{OpenAiCompatibleConfig, OpenAiCompatibleProvider};
use crate::request::ProviderRequest;

#[derive(Debug, Clone)]
pub struct LlmEndpointConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub protocol: ProviderProtocol,
    pub timeout: Duration,
    pub max_retries: u32,
}

impl LlmEndpointConfig {
    pub fn new(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
            model: model.into(),
            protocol: ProviderProtocol::default(),
            timeout: Duration::from_secs(60),
            max_retries: 3,
        }
    }
}

pub async fn complete_text(
    config: &LlmEndpointConfig,
    system: &str,
    prompt: &str,
    max_tokens: u32,
) -> Result<String, LlmError> {
    let provider = build_provider(config)?;
    let request = ProviderRequest {
        model: config.model.clone(),
        system: Some(system.to_string()),
        messages: vec![ChatMessage::text(MessageRole::User, prompt)],
        tools: Vec::new(),
        tool_choice: None,
        max_tokens: Some(max_tokens),
        temperature: Some(0.2),
        top_p: None,
        stop_sequences: Vec::new(),
        metadata: None,
        thinking: None,
    };
    let response = provider.complete(request).await?;
    Ok(message_text(&response.assistant_message))
}

fn build_provider(config: &LlmEndpointConfig) -> Result<Box<dyn LlmProvider>, LlmError> {
    match config.protocol {
        ProviderProtocol::OpenAiCompatible => {
            let mut provider_config =
                OpenAiCompatibleConfig::new(config.api_key.clone(), config.base_url.clone());
            provider_config.timeout = config.timeout;
            provider_config.max_retries = config.max_retries;
            Ok(Box::new(OpenAiCompatibleProvider::new(provider_config)))
        }
        ProviderProtocol::Anthropic => {
            let mut provider_config = AnthropicConfig::new(config.api_key.clone());
            if !config.base_url.trim().is_empty() {
                provider_config.base_url = Some(config.base_url.clone());
            }
            provider_config.timeout = config.timeout;
            provider_config.max_retries = config.max_retries;
            Ok(Box::new(AnthropicProvider::from_config(provider_config)?))
        }
    }
}

fn message_text(message: &ChatMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|content| match content {
            MessageContent::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}
