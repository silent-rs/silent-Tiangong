//! CoreConfig：TiangongCore 运行所需的最小配置契约
//!
//! 不包含 UI 偏好、session 策略、日志级别等外围配置。
//! 只关心：用什么模型、有什么工具、什么权限。
//!
//! 外部（CLI/GUI/Server/第三方）负责构建和更新配置，
//! TiangongCore 仅通过 CoreConfigProvider 只读消费。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use arc_swap::ArcSwap;

use serde::{Deserialize, Serialize};

use crate::agent_config::{McpConfig, McpServerConfig, SkillsConfig};
use crate::mcp::McpToolMeta;
use crate::permission::TrustMode;

const DEFAULT_CONTEXT_LIMIT: usize = 32_768;
const DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// 模型端点配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEndpoint {
    /// API 基础 URL
    pub base_url: String,
    /// API 密钥
    pub api_key: String,
    /// 模型名称
    pub model: String,
    /// 请求超时（毫秒）
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

impl Default for ModelEndpoint {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
}

/// LLM 配置 — TiangongCore 所需的模型端点
///
/// 扁平结构，直接描述端点，无需解析 Provider/Model/Routing。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LlmConfig {
    /// 主 Chat 端点（必须）
    pub chat: ModelEndpoint,
    /// 轻量级文本端点（标题生成、意图分类等简单任务，未配置时回退到 chat）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lite: Option<ModelEndpoint>,
    /// 图片生成端点
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_generation: Option<ModelEndpoint>,
    /// 语音合成端点
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tts: Option<ModelEndpoint>,
    /// 语音识别端点
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stt: Option<ModelEndpoint>,
    /// 视频生成端点
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_generation: Option<ModelEndpoint>,
}

impl LlmConfig {
    /// 检查是否有有效的 Chat 端点
    pub fn is_valid(&self) -> bool {
        !self.chat.base_url.is_empty() && !self.chat.api_key.is_empty()
    }
}

/// TiangongCore 运行所需的最小配置
#[derive(Debug, Clone)]
pub struct CoreConfig {
    /// LLM 模型端点配置
    pub llm: LlmConfig,
    /// MCP 服务配置（server 列表）
    pub mcp: McpConfig,
    /// MCP 能力数据（预填充，Core 不发起网络请求）
    pub mcp_capabilities: Vec<(String, Vec<McpToolMeta>)>,
    /// Skill 配置（已安装的 skill 列表）
    pub skills: SkillsConfig,
    /// 权限信任模式
    pub trust_mode: TrustMode,
    /// 上下文窗口大小（token 数）
    pub context_limit: usize,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            llm: LlmConfig::default(),
            mcp: McpConfig::default(),
            mcp_capabilities: Vec::new(),
            skills: SkillsConfig::default(),
            trust_mode: TrustMode::default(),
            context_limit: DEFAULT_CONTEXT_LIMIT,
        }
    }
}

impl CoreConfig {
    /// 快捷构建器
    pub fn builder() -> CoreConfigBuilder {
        CoreConfigBuilder::default()
    }
}

/// CoreConfig 构建器
#[derive(Debug, Default)]
pub struct CoreConfigBuilder {
    config: CoreConfig,
}

impl CoreConfigBuilder {
    /// 设置 Chat 端点（最常用的快捷方式）
    pub fn with_chat(mut self, base_url: &str, api_key: &str, model: &str) -> Self {
        self.config.llm.chat = ModelEndpoint {
            base_url: base_url.to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
        };
        self
    }

    /// 设置完整的 LlmConfig
    pub fn with_llm_config(mut self, llm: LlmConfig) -> Self {
        self.config.llm = llm;
        self
    }

    /// 添加 MCP server
    pub fn with_mcp_server(mut self, server: McpServerConfig) -> Self {
        self.config.mcp.servers.push(server);
        self
    }

    /// 设置完整的 McpConfig
    pub fn with_mcp_config(mut self, mcp: McpConfig) -> Self {
        self.config.mcp = mcp;
        self
    }

    /// 设置 Skills 配置
    pub fn with_skills_config(mut self, skills: SkillsConfig) -> Self {
        self.config.skills = skills;
        self
    }

    /// 设置信任模式
    pub fn with_trust_mode(mut self, mode: TrustMode) -> Self {
        self.config.trust_mode = mode;
        self
    }

    /// 设置上下文窗口大小
    pub fn with_context_limit(mut self, limit: usize) -> Self {
        self.config.context_limit = limit;
        self
    }

    /// 构建
    pub fn build(self) -> CoreConfig {
        self.config
    }
}

/// 配置提供者
///
/// 多线程安全：
/// - 读操作无锁（ArcSwap::load，纳秒级）
/// - 写操作原子替换（不阻塞读）
/// - generation 递增用于快速变更检测
#[derive(Clone)]
pub struct CoreConfigProvider {
    inner: Arc<ArcSwap<CoreConfig>>,
    generation: Arc<AtomicU64>,
}

impl CoreConfigProvider {
    /// 创建配置提供者
    pub fn new(config: CoreConfig) -> Self {
        Self {
            inner: Arc::new(ArcSwap::from_pointee(config)),
            generation: Arc::new(AtomicU64::new(1)),
        }
    }

    /// 获取当前配置快照（无锁读，纳秒级）
    pub fn snapshot(&self) -> Arc<CoreConfig> {
        self.inner.load_full()
    }

    /// 获取配置版本号（原子读，用于变更检测）
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// 更新配置（原子替换 + generation 递增）
    pub fn update(&self, f: impl FnOnce(&mut CoreConfig)) {
        let mut new_config = (*self.inner.load_full()).clone();
        f(&mut new_config);
        self.inner.store(Arc::new(new_config));
        self.generation.fetch_add(1, Ordering::Release);
    }

    /// 整体替换配置
    pub fn replace(&self, config: CoreConfig) {
        self.inner.store(Arc::new(config));
        self.generation.fetch_add(1, Ordering::Release);
    }
}

impl std::fmt::Debug for CoreConfigProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreConfigProvider")
            .field("generation", &self.generation())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_snapshot_and_generation() {
        let config = CoreConfig::default();
        let provider = CoreConfigProvider::new(config);

        assert_eq!(provider.generation(), 1);
        let snap = provider.snapshot();
        assert_eq!(snap.context_limit, DEFAULT_CONTEXT_LIMIT);
    }

    #[test]
    fn provider_update_increments_generation() {
        let provider = CoreConfigProvider::new(CoreConfig::default());
        assert_eq!(provider.generation(), 1);

        provider.update(|c| c.context_limit = 65536);
        assert_eq!(provider.generation(), 2);

        let snap = provider.snapshot();
        assert_eq!(snap.context_limit, 65536);
    }

    #[test]
    fn provider_clone_shares_state() {
        let provider = CoreConfigProvider::new(CoreConfig::default());
        let cloned = provider.clone();

        provider.update(|c| c.context_limit = 16384);
        assert_eq!(cloned.generation(), 2);
        assert_eq!(cloned.snapshot().context_limit, 16384);
    }

    #[test]
    fn builder_basic() {
        let config = CoreConfig::builder()
            .with_chat("https://api.example.com/v1", "sk-test", "gpt-4o")
            .with_trust_mode(TrustMode::FullTrust)
            .with_context_limit(65536)
            .build();

        assert_eq!(config.llm.chat.base_url, "https://api.example.com/v1");
        assert_eq!(config.llm.chat.model, "gpt-4o");
        assert!(config.llm.is_valid());
        assert_eq!(config.trust_mode, TrustMode::FullTrust);
        assert_eq!(config.context_limit, 65536);
    }

    #[test]
    fn llm_config_capabilities() {
        let mut llm = LlmConfig::default();
        assert!(llm.image_generation.is_none());
        assert!(llm.tts.is_none());

        llm.image_generation = Some(ModelEndpoint {
            base_url: "https://api.example.com/v1".into(),
            api_key: "sk-test".into(),
            model: "dall-e-3".into(),
            ..Default::default()
        });
        assert!(llm.image_generation.is_some());
    }
}
