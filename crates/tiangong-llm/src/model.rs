use std::str::FromStr;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

/// Provider 协议类型。
///
/// `OpenAi` 走 OpenAI Responses API（`/responses`，需显式选择）；`OpenAiChatCompletions`
/// 走 Chat Completions API（`/chat/completions`），是默认协议，适用于官方 OpenAI 端点以及
/// vLLM/Ollama/智谱等第三方 OpenAI 兼容端点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProviderProtocol {
    /// OpenAI Responses API（`/responses`）。
    OpenAi,
    /// OpenAI Chat Completions API（`/chat/completions`）。
    #[default]
    OpenAiChatCompletions,
    Anthropic,
    DeepSeek,
}

impl Serialize for ProviderProtocol {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ProviderProtocol {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl ProviderProtocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderProtocol::OpenAi => "openai",
            ProviderProtocol::OpenAiChatCompletions => "openai_chatcompletions",
            ProviderProtocol::Anthropic => "anthropic",
            ProviderProtocol::DeepSeek => "deepseek",
        }
    }
}

impl FromStr for ProviderProtocol {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            // 空值回退到默认协议（Chat Completions），避免未配置端点误走 Responses。
            "" => Ok(ProviderProtocol::OpenAiChatCompletions),
            "openai" => Ok(ProviderProtocol::OpenAi),
            "openai_responses" | "openai-responses" | "responses" => Ok(ProviderProtocol::OpenAi),
            "openai_chatcompletions"
            | "openai_chat_completions"
            | "openai_chat"
            | "openai-chat"
            | "chat_completions"
            | "chat-completions" => Ok(ProviderProtocol::OpenAiChatCompletions),
            // 历史别名：旧的 "openai_compatible" 归入 Chat Completions。
            "openai_compatible" | "open_ai_compatible" => {
                Ok(ProviderProtocol::OpenAiChatCompletions)
            }
            "anthropic" => Ok(ProviderProtocol::Anthropic),
            "deepseek" | "deep_seek" => Ok(ProviderProtocol::DeepSeek),
            other => Err(anyhow!("不支持的 provider 协议：{other}")),
        }
    }
}

/// Provider 暴露的模型信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderModelInfo {
    pub id: String,
    pub display_name: Option<String>,
}
