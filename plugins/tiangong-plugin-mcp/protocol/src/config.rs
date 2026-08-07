//! MCP 配置类型（协议层，纯数据结构）。
//!
//! 这些类型同时被 sidecar 和 WASM 引用，必须可序列化、不依赖 rmcp/tokio/anyhow。
//! 解析、校验、规范化等业务逻辑保留在 sidecar。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 用户在配置里声明的传输模式（Auto 由 sidecar 根据字段推断）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpTransportMode {
    #[default]
    Auto,
    Stdio,
    Http,
}

/// 解析后的实际传输模式（仅 sidecar 内部使用，不序列化进 mcp.json）。
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

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_ms: default_mcp_timeout_ms(),
            servers: Vec::new(),
        }
    }
}

fn default_enabled() -> bool {
    true
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

// ── 注册 / 编辑请求类型 ──
//
// `RegisterMcpServerOptions` 的 headers/env 用 Vec<(String,String)> 而非 BTreeMap，
// 以保留前端表单的输入顺序；sidecar 规范化时再聚合去重。

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegisterMcpServerOptions {
    #[serde(default)]
    pub transport: Option<McpTransportMode>,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub auth_header: Option<String>,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegisterMcpServerRequest {
    pub name: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub options: RegisterMcpServerOptions,
}

/// 更新顶层 MCP 配置项请求（enabled / timeout_ms）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateConfigEntryRequest {
    pub key: String,
    pub value: String,
}
