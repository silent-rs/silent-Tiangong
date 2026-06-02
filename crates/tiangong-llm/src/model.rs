use std::str::FromStr;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

/// Provider 协议类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProviderProtocol {
    #[default]
    OpenAiCompatible,
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
            ProviderProtocol::OpenAiCompatible => "openai",
            ProviderProtocol::Anthropic => "anthropic",
            ProviderProtocol::DeepSeek => "deepseek",
        }
    }
}

impl FromStr for ProviderProtocol {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "" | "openai" | "openai_compatible" | "open_ai_compatible" => {
                Ok(ProviderProtocol::OpenAiCompatible)
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
