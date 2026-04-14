use std::str::FromStr;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

/// Provider 协议类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProtocol {
    #[default]
    OpenAiCompatible,
    Anthropic,
}

impl ProviderProtocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderProtocol::OpenAiCompatible => "openai_compatible",
            ProviderProtocol::Anthropic => "anthropic",
        }
    }
}

impl FromStr for ProviderProtocol {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "" | "openai_compatible" => Ok(ProviderProtocol::OpenAiCompatible),
            "anthropic" => Ok(ProviderProtocol::Anthropic),
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
