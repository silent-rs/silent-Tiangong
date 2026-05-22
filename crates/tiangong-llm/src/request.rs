use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::message::ChatMessage;
use crate::tool::{ToolChoice, ToolSpec};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThinkingConfig {
    #[serde(default = "default_thinking_budget_tokens")]
    pub budget_tokens: u32,
}

fn default_thinking_budget_tokens() -> u32 {
    2048
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
    Max,
}

/// 统一 provider 请求。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
    #[serde(default)]
    pub tool_choice: Option<ToolChoice>,
    pub max_tokens: u32,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub stop_sequences: Vec<String>,
    #[serde(default)]
    pub metadata: Option<Map<String, Value>>,
    #[serde(default)]
    pub thinking: Option<ThinkingConfig>,
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// 显式禁用思考模式（如 DeepSeek 的 thinking: {"type": "disabled"}）
    #[serde(default)]
    pub thinking_disabled: bool,
}
