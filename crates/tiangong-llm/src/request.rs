use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::message::ChatMessage;
use crate::tool::{ToolChoice, ToolSpec};

/// 思考强度：请求中唯一的思考控制参数。
/// `None` 表示关闭思考；其余档位表示开启思考并表达强度。
/// 各 provider 按自身协议映射（如 Anthropic 的 thinking 开关与预算、
/// DeepSeek 的 thinking.type、OpenAI 的 reasoning.effort）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Low,
    Medium,
    High,
    Max,
}

// derive(Default) 会取第一个变体 None，语义是"关闭思考"，与期望的
// 默认档位 Medium 不符，因此手写实现。
#[allow(clippy::derivable_impls)]
impl Default for ReasoningEffort {
    fn default() -> Self {
        Self::Medium
    }
}

impl ReasoningEffort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Max => "max",
        }
    }

    /// 解析持久化/用户输入字符串：空串与未知值回退 Medium（历史默认档），
    /// 保证旧数据可加载。
    pub fn parse_flexible(value: &str) -> Self {
        match value.trim().to_lowercase().as_str() {
            "none" => Self::None,
            "low" => Self::Low,
            "high" => Self::High,
            "max" => Self::Max,
            _ => Self::Medium,
        }
    }

    pub fn is_thinking_enabled(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// serde 反序列化适配：字符串经 [`ReasoningEffort::parse_flexible`] 容错解析，
/// 旧数据中的空串/未知值不会导致加载失败。
pub fn deserialize_reasoning_effort_flexible<'de, D>(
    deserializer: D,
) -> Result<ReasoningEffort, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    Ok(ReasoningEffort::parse_flexible(&value))
}

/// [`deserialize_reasoning_effort_flexible`] 的 Option 版：null/空串 → None，
/// 未知值回退 Medium。
pub fn deserialize_reasoning_effort_option_flexible<'de, D>(
    deserializer: D,
) -> Result<Option<ReasoningEffort>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: Option<String> = Option::deserialize(deserializer)?;
    Ok(value
        .filter(|value| !value.trim().is_empty())
        .map(|value| ReasoningEffort::parse_flexible(&value)))
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
    /// 思考强度：None 关闭思考，其余档位开启。
    #[serde(default = "default_reasoning_effort_disabled")]
    pub reasoning_effort: ReasoningEffort,
}

fn default_reasoning_effort_disabled() -> ReasoningEffort {
    ReasoningEffort::None
}
