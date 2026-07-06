//! Agent 运行时配置。
//!
//! Skill / MCP 相关配置类型已迁出：
//! - Skill 类型（SkillsConfig / InstalledSkillConfig 等）→ `tiangong-plugin-skill::skill_config`
//! - MCP 类型（McpConfig 等）→ `tiangong-plugin-mcp::config`
//!
//! 本模块仅保留 AgentConfig（信任模式 / 自定义 prompt / reasoning effort）。

use serde::{Deserialize, Serialize};

/// MCP 相关配置类型（`McpConfig` / `McpServerConfig` / `McpTransportMode` 等）
/// 已迁出至 `tiangong-plugin-mcp` crate，core 不再持有 MCP 概念。
/// `~/.tiangong/mcp.json` 由 MCP 插件自托管读写。
///
/// Skill 相关配置类型（`SkillsConfig` / `InstalledSkillConfig` 等）已迁出至
/// `tiangong-plugin-skill` crate。`~/.tiangong/skills/` 由 Skill 插件自托管。

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
