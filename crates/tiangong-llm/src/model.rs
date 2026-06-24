use std::str::FromStr;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

/// Provider 协议类型。
///
/// OpenAI 兼容协议统一走 Chat Completions API（`/chat/completions`），适用于官方
/// OpenAI 端点以及 vLLM/Ollama/智谱等第三方 OpenAI 兼容端点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProviderProtocol {
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
            "" => Ok(ProviderProtocol::OpenAiChatCompletions),
            "openai"
            | "openai_compatible"
            | "open_ai_compatible"
            | "openai_chatcompletions"
            | "openai_chat_completions"
            | "openai_chat"
            | "openai-chat"
            | "chat_completions"
            | "chat-completions" => Ok(ProviderProtocol::OpenAiChatCompletions),
            "openai_responses" | "openai-responses" | "responses" => Err(anyhow!(
                "当前主线暂不启用 OpenAI Responses 协议，请使用 openai_chatcompletions"
            )),
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

#[cfg(test)]
mod tests {
    use super::ProviderProtocol;

    #[test]
    fn parses_legacy_openai_protocols_as_chat_completions() {
        for protocol in ["", "openai", "openai_compatible"] {
            assert_eq!(
                protocol.parse::<ProviderProtocol>().unwrap(),
                ProviderProtocol::OpenAiChatCompletions,
                "{protocol:?} 应兼容解析为 Chat Completions 协议"
            );
        }
    }

    #[test]
    fn rejects_responses_protocols_on_mainline() {
        for protocol in ["openai_responses", "responses"] {
            let err = protocol.parse::<ProviderProtocol>().unwrap_err();
            assert!(
                err.to_string().contains("暂不启用 OpenAI Responses"),
                "{protocol:?} 应明确拒绝 Responses 协议，实际错误：{err}"
            );
        }
    }
}
