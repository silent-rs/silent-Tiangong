use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillSourceConfig {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillMcpRequirementConfig {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub source: String,
    #[serde(default, alias = "pkg")]
    pub package: String,
    #[serde(default)]
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillPermissionConfig {
    #[serde(default)]
    pub fs_read: Vec<String>,
    #[serde(default)]
    pub fs_write: Vec<String>,
    #[serde(default)]
    pub cmd_exec: Vec<String>,
    #[serde(default)]
    pub net: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InstalledSkillConfig {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub entry: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub installed_at: String,
    #[serde(default)]
    pub managed_mcp_servers: Vec<String>,
    #[serde(default)]
    pub source: SkillSourceConfig,
    #[serde(default)]
    pub requires_mcp: Vec<SkillMcpRequirementConfig>,
    #[serde(default)]
    pub permissions: SkillPermissionConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillsConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub dirs: Vec<String>,
    #[serde(default = "default_max_matches")]
    pub max_matches: usize,
    #[serde(default)]
    pub installed: Vec<InstalledSkillConfig>,
}

/// MCP 相关配置类型（`McpConfig` / `McpServerConfig` / `McpTransportMode` 等）
/// 已迁出至 `tiangong-plugin-mcp` crate，core 不再持有 MCP 概念。
/// `~/.tiangong/mcp.json` 由 MCP 插件自托管读写。

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

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dirs: Vec::new(),
            max_matches: default_max_matches(),
            installed: Vec::new(),
        }
    }
}

fn default_enabled() -> bool {
    true
}

fn default_max_matches() -> usize {
    3
}
