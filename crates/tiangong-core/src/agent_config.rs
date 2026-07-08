//! Agent 运行时配置。
//!
//! AgentConfig 仅保留 agent runtime 配置；扩展能力配置由各插件自管。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentConfig {
    /// 当前权限信任模式（可按当前会话调整）
    #[serde(default)]
    pub trust_mode: crate::permission::TrustMode,
    /// 新对话默认权限信任模式
    #[serde(default)]
    pub default_trust_mode: crate::permission::TrustMode,
    /// 用户自定义特色 Prompt，会注入到 system prompt。
    #[serde(default)]
    pub custom_system_prompt: String,
    /// 思考强度设置：none/low/medium/high/max，默认 medium
    #[serde(default = "default_reasoning_effort")]
    pub reasoning_effort: String,
}

fn default_reasoning_effort() -> String {
    "medium".to_string()
}
