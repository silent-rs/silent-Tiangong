//! MCP 配置类型。
//!
//! 原属 `tiangong-core::agent_config`，MCP 管理插件化后迁入本 crate，
//! 由 plugin 自托管读写 `~/.tiangong/mcp.json`，core 不再持有 MCP 概念。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

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

impl Default for McpTransportMode {
    fn default() -> Self {
        default_mcp_transport_mode()
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

// ── 注册 / 编辑请求类型（原 app_state::RegisterMcpServerRequest / Options）──

#[derive(Debug, Clone, Default)]
pub struct RegisterMcpServerOptions {
    pub transport: Option<McpTransportMode>,
    pub endpoint: Option<String>,
    pub auth_header: Option<String>,
    pub headers: Vec<(String, String)>,
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default)]
pub struct RegisterMcpServerRequest {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub tags: Vec<String>,
    pub enabled: bool,
    pub options: RegisterMcpServerOptions,
}

/// 规范化后的 MCP server 字段（不含 name，name 作为主键由调用方处理）。
pub(crate) struct NormalizedMcpFields {
    pub(crate) transport: McpTransportMode,
    pub(crate) command: String,
    pub(crate) args: Vec<String>,
    pub(crate) endpoint: String,
    pub(crate) auth_header: String,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) enabled: bool,
    pub(crate) tags: Vec<String>,
}

/// 把注册/编辑请求规范化为可直接写入 [`McpServerConfig`] 的字段。
/// register_mcp_server 与 update_mcp_server 共用，避免 trim/filter 逻辑重复。
pub(crate) fn normalize_request_fields(
    request: RegisterMcpServerRequest,
) -> anyhow::Result<NormalizedMcpFields> {
    let tags = request
        .tags
        .into_iter()
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect::<Vec<_>>();
    let args = request
        .args
        .into_iter()
        .map(|arg| arg.trim().to_string())
        .filter(|arg| !arg.is_empty())
        .collect::<Vec<_>>();
    let transport = request.options.transport.unwrap_or_default();
    let endpoint = request
        .options
        .endpoint
        .unwrap_or_default()
        .trim()
        .to_string();
    let auth_header = request
        .options
        .auth_header
        .unwrap_or_default()
        .trim()
        .to_string();

    let headers = request
        .options
        .headers
        .into_iter()
        .filter_map(|(key, value)| {
            let key = key.trim().to_string();
            let value = value.trim().to_string();
            if key.is_empty() || value.is_empty() {
                None
            } else {
                Some((key, value))
            }
        })
        .collect::<BTreeMap<_, _>>();
    let env = request
        .options
        .env
        .into_iter()
        .filter_map(|(key, value)| {
            let key = key.trim().to_string();
            let value = value.trim().to_string();
            if key.is_empty() || value.is_empty() {
                None
            } else {
                Some((key, value))
            }
        })
        .collect::<BTreeMap<_, _>>();

    Ok(NormalizedMcpFields {
        transport,
        command: request.command.trim().to_string(),
        args,
        endpoint,
        auth_header,
        headers,
        env,
        enabled: request.enabled,
        tags,
    })
}
