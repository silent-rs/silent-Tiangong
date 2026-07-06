//! Skill 配置类型。
//!
//! 原属 `tiangong-core::agent_config`，Skill 从 core 迁出后归入本 plugin。
//! skills 已从 [`tiangong_core::agent_config::AgentConfig`] 脱离，由本 plugin 自托管。

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
