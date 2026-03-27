use std::collections::BTreeMap;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpTransportMode {
    Auto,
    Stdio,
    Http,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedMcpTransport {
    Stdio,
    Http,
    Metadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    #[serde(default = "default_mcp_transport_mode")]
    pub transport: McpTransportMode,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub auth_header: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl McpServerConfig {
    pub fn resolved_transport(&self) -> ResolvedMcpTransport {
        match self.transport {
            McpTransportMode::Auto => {
                if !self.endpoint.trim().is_empty() || is_http_endpoint(&self.command) {
                    ResolvedMcpTransport::Http
                } else if !self.command.trim().is_empty() {
                    ResolvedMcpTransport::Stdio
                } else {
                    ResolvedMcpTransport::Metadata
                }
            }
            McpTransportMode::Stdio => ResolvedMcpTransport::Stdio,
            McpTransportMode::Http => ResolvedMcpTransport::Http,
        }
    }

    pub fn resolved_http_endpoint(&self) -> Option<&str> {
        if matches!(self.transport, McpTransportMode::Stdio) {
            return None;
        }
        let endpoint = self.endpoint.trim();
        if !endpoint.is_empty() {
            return Some(endpoint);
        }
        let command = self.command.trim();
        if is_http_endpoint(command) {
            return Some(command);
        }
        None
    }

    pub fn command_text(&self) -> &str {
        self.command.trim()
    }

    pub fn cwd_text(&self) -> Option<&str> {
        let cwd = self.cwd.trim();
        if cwd.is_empty() { None } else { Some(cwd) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_mcp_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentConfig {
    #[serde(default)]
    pub skills: SkillsConfig,
    #[serde(default)]
    pub mcp: McpConfig,
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

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_ms: default_mcp_timeout_ms(),
            servers: Vec::new(),
        }
    }
}

impl Default for McpTransportMode {
    fn default() -> Self {
        default_mcp_transport_mode()
    }
}

fn default_enabled() -> bool {
    true
}

fn default_max_matches() -> usize {
    3
}

fn default_mcp_timeout_ms() -> u64 {
    15_000
}

fn default_mcp_transport_mode() -> McpTransportMode {
    McpTransportMode::Auto
}

pub fn is_http_endpoint(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    value.starts_with("http://") || value.starts_with("https://")
}
