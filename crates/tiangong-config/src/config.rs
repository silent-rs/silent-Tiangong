//! TiangongConfig：完整应用配置

use serde::{Deserialize, Serialize};
use tiangong_core::agent_config::{McpConfig, SkillsConfig};
use tiangong_core::core_config::CoreConfig;
use tiangong_core::mcp::McpToolMeta;
use tiangong_core::models_config::ModelsConfig;
use tiangong_core::permission::TrustMode;

/// Server 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// 监听地址
    #[serde(default = "default_host")]
    pub host: String,
    /// 监听端口
    #[serde(default = "default_port")]
    pub port: u16,
    /// API 认证 Token
    #[serde(default)]
    pub auth_token: Option<String>,
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}
fn default_port() -> u16 {
    8080
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            auth_token: None,
        }
    }
}

/// Connector 类型
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum ConnectorType {
    #[default]
    Webhook,
    Telegram,
    Discord,
    Lark,
}

/// Connector 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorConfig {
    pub name: String,
    pub connector_type: ConnectorType,
    pub enabled: bool,
    pub settings: serde_json::Value,
}

/// 天工完整应用配置
///
/// 包含 Core 所需的配置（models/mcp/skills/trust_mode）
/// 以及应用层配置（server/connectors）。
#[derive(Debug, Clone, Default)]
pub struct TiangongConfig {
    // ===== Core 所需配置 =====
    /// LLM 模型配置
    pub models: ModelsConfig,
    /// MCP 服务配置
    pub mcp: McpConfig,
    /// MCP 能力数据
    pub mcp_capabilities: Vec<(String, Vec<McpToolMeta>)>,
    /// Skill 配置
    pub skills: SkillsConfig,
    /// 权限信任模式
    pub trust_mode: TrustMode,

    // ===== 应用层配置 =====
    /// Server 配置
    pub server: ServerConfig,
    /// Connector 配置列表
    pub connectors: Vec<ConnectorConfig>,
}

impl TiangongConfig {
    /// 转换为 CoreConfig（提取 Core 所需的最小子集）
    ///
    /// 将 ModelsConfig（3 层）解析为 LlmConfig（扁平端点）
    pub fn to_core_config(&self) -> CoreConfig {
        CoreConfig {
            llm: tiangong_core::core_config::LlmConfig::from_models_config(&self.models),
            mcp: self.mcp.clone(),
            mcp_capabilities: self.mcp_capabilities.clone(),
            skills: self.skills.clone(),
            trust_mode: self.trust_mode,
            default_trust_mode: self.trust_mode,
            custom_system_prompt: String::new(),
            context_limit: 0, // 0 表示自动根据模型名称解析
        }
    }

    /// 创建 CoreConfigProvider（用于注入 TiangongCore）
    pub fn into_core_config_provider(self) -> tiangong_core::core_config::CoreConfigProvider {
        tiangong_core::core_config::CoreConfigProvider::new(self.to_core_config())
    }
}
