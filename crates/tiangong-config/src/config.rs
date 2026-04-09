//! TiangongConfig：完整应用配置

use serde::{Deserialize, Serialize};
use tiangong_core::agent_config::{McpConfig, SkillsConfig};
use tiangong_core::core_config::{CoreConfig, LlmConfig, ModelEndpoint};
use tiangong_core::mcp::McpToolMeta;
use tiangong_core::models_config::{ModelCapability, ModelsConfig};
use tiangong_core::permission::TrustMode;

const DEFAULT_CONTEXT_LIMIT: usize = 32_768;

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
#[derive(Debug, Clone)]
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
    /// 上下文窗口大小
    pub context_limit: usize,

    // ===== 应用层配置 =====
    /// Server 配置
    pub server: ServerConfig,
    /// Connector 配置列表
    pub connectors: Vec<ConnectorConfig>,
}

impl Default for TiangongConfig {
    fn default() -> Self {
        Self {
            models: ModelsConfig::default(),
            mcp: McpConfig::default(),
            mcp_capabilities: Vec::new(),
            skills: SkillsConfig::default(),
            trust_mode: TrustMode::default(),
            context_limit: DEFAULT_CONTEXT_LIMIT,
            server: ServerConfig::default(),
            connectors: Vec::new(),
        }
    }
}

impl TiangongConfig {
    /// 转换为 CoreConfig（提取 Core 所需的最小子集）
    ///
    /// 将 ModelsConfig（3 层）解析为 LlmConfig（扁平端点）
    pub fn to_core_config(&self) -> CoreConfig {
        CoreConfig {
            llm: self.resolve_llm_config(),
            mcp: self.mcp.clone(),
            mcp_capabilities: self.mcp_capabilities.clone(),
            skills: self.skills.clone(),
            trust_mode: self.trust_mode,
            context_limit: self.context_limit,
        }
    }

    /// 从 ModelsConfig 解析出 LlmConfig
    fn resolve_llm_config(&self) -> LlmConfig {
        let resolve = |cap: ModelCapability| -> Option<ModelEndpoint> {
            let resolved = self.models.resolve_for_capability(cap)?;
            Some(ModelEndpoint {
                base_url: resolved.base_url,
                api_key: resolved.api_key,
                model: resolved.model,
                timeout_ms: resolved.timeout_ms,
            })
        };

        LlmConfig {
            chat: resolve(ModelCapability::Chat).unwrap_or_default(),
            lite: resolve(ModelCapability::Lite),
            image_generation: resolve(ModelCapability::ImageGeneration),
            tts: resolve(ModelCapability::Tts),
            stt: resolve(ModelCapability::Stt),
            video_generation: resolve(ModelCapability::VideoGeneration),
        }
    }

    /// 创建 CoreConfigProvider（用于注入 TiangongCore）
    pub fn into_core_config_provider(self) -> tiangong_core::core_config::CoreConfigProvider {
        tiangong_core::core_config::CoreConfigProvider::new(self.to_core_config())
    }
}
