use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::message::ChatMessage;
use crate::usage::TokenUsageData;

/// 统一停止原因。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    Other(String),
}

/// 统一 provider 响应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderResponse {
    pub id: Option<String>,
    pub model: Option<String>,
    pub assistant_message: ChatMessage,
    #[serde(default)]
    pub reasoning_content: Option<String>,
    pub stop_reason: Option<StopReason>,
    pub usage: Option<TokenUsageData>,
    #[serde(default)]
    pub raw: Option<Value>,
}
