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
use serde_json::Value;

use crate::agent_config::{McpConfig, McpServerConfig, SkillsConfig};
use crate::mcp::McpToolMeta;
use crate::model::ProviderProtocol;
use crate::models_config::{ModelCapability, ModelsConfig};
use crate::permission::TrustMode;

const DEFAULT_CONTEXT_LIMIT: usize = 200_000;
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
    /// Provider 协议
    #[serde(default)]
    pub protocol: ProviderProtocol,
    /// 请求超时（毫秒）
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub options: Value,
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
            protocol: ProviderProtocol::default(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            options: Value::Object(serde_json::Map::new()),
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
    /// 多模态理解端点
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multimodal: Option<ModelEndpoint>,
    /// 向量嵌入端点
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<ModelEndpoint>,
    /// 结果重排端点
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank: Option<ModelEndpoint>,
}

impl LlmConfig {
    /// 从 ModelsConfig 解析出 Core 运行所需的扁平端点配置
    pub fn from_models_config(models: &ModelsConfig) -> Self {
        let resolve = |cap: ModelCapability| -> Option<ModelEndpoint> {
            let resolved = models.resolve_for_capability(cap)?;
            Some(ModelEndpoint {
                base_url: resolved.base_url,
                api_key: resolved.api_key,
                model: resolved.model,
                protocol: resolved.protocol,
                timeout_ms: resolved.timeout_ms,
                options: resolved.options,
            })
        };

        Self {
            chat: resolve(ModelCapability::Chat).unwrap_or_default(),
            lite: resolve(ModelCapability::Lite),
            image_generation: resolve(ModelCapability::ImageGeneration),
            tts: resolve(ModelCapability::Tts),
            stt: resolve(ModelCapability::Stt),
            video_generation: resolve(ModelCapability::VideoGeneration),
            multimodal: resolve(ModelCapability::Multimodal),
            embedding: resolve(ModelCapability::Embedding),
            rerank: resolve(ModelCapability::Rerank),
        }
    }

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
    /// 新对话默认权限信任模式
    pub default_trust_mode: TrustMode,
    /// 用户自定义 system prompt
    pub custom_system_prompt: String,
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
            default_trust_mode: TrustMode::default(),
            custom_system_prompt: String::new(),
            context_limit: DEFAULT_CONTEXT_LIMIT,
        }
    }
}

impl CoreConfig {
    /// 快捷构建器
    pub fn builder() -> CoreConfigBuilder {
        CoreConfigBuilder::default()
    }

    /// 根据 CoreConfig 生成 Memory 启动参数。
    pub fn to_memory_options(
        &self,
        workspace_id: Option<String>,
    ) -> tiangong_memory::MemoryOptions {
        tiangong_memory::MemoryConfig::load_or_default().to_options(workspace_id)
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
            protocol: ProviderProtocol::default(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            options: Value::Object(serde_json::Map::new()),
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

    pub fn with_default_trust_mode(mut self, mode: TrustMode) -> Self {
        self.config.default_trust_mode = mode;
        self
    }

    pub fn with_custom_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.config.custom_system_prompt = prompt.into();
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

/// 返回内嵌的默认 context_windows.json 内容，用于首次安装时释放到用户目录
pub fn default_context_windows_json() -> &'static str {
    include_str!("context/context_windows.json")
}

/// 根据模型名称从映射表解析 context_window
pub fn resolve_context_limit(model_name: &str) -> usize {
    const DEFAULT_MAP: &str = include_str!("context/context_windows.json");

    let path = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".tiangong")
        .join("context_windows.json");

    let content = if path.exists() {
        std::fs::read_to_string(&path).unwrap_or_else(|_| DEFAULT_MAP.to_string())
    } else {
        DEFAULT_MAP.to_string()
    };

    let map: std::collections::HashMap<String, Value> = match serde_json::from_str(&content) {
        Ok(m) => m,
        Err(err) => {
            tracing::warn!(
                "解析 context_windows.json 失败：{err}，使用默认值 {DEFAULT_CONTEXT_LIMIT}"
            );
            return DEFAULT_CONTEXT_LIMIT;
        }
    };

    // 精确匹配
    if let Some(Some(n)) = map.get(model_name).map(|v| v.as_u64()) {
        return n as usize;
    }

    // 前缀匹配：用最长的匹配前缀
    let mut best_match: Option<usize> = None;
    let mut best_len = 0;
    for (key, val) in &map {
        if key.starts_with('_') {
            continue;
        }
        if model_name.starts_with(key)
            && key.len() > best_len
            && let Some(n) = val.as_u64()
        {
            best_match = Some(n as usize);
            best_len = key.len();
        }
    }
    best_match.unwrap_or(DEFAULT_CONTEXT_LIMIT)
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

    #[test]
    fn llm_config_from_models_config_preserves_protocol() {
        let mut models = ModelsConfig::default();
        models.providers.insert(
            "anthropic".to_string(),
            crate::models_config::ProviderConfig {
                base_url: "https://api.anthropic.com".into(),
                api_key: "sk-ant".into(),
                timeout_ms: 30_000,
                protocol: ProviderProtocol::Anthropic,
            },
        );
        models.models.insert(
            "chat-model".to_string(),
            crate::models_config::ModelEntry {
                provider: "anthropic".into(),
                model: "claude-sonnet-4".into(),
                capabilities: vec![ModelCapability::Chat],
                options: serde_json::json!({}),
            },
        );
        models
            .routing
            .insert(ModelCapability::Chat, "chat-model".into());

        let llm = LlmConfig::from_models_config(&models);
        assert_eq!(llm.chat.protocol, ProviderProtocol::Anthropic);
    }
}
