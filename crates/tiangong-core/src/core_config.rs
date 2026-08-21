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

use crate::model::ProviderProtocol;
use crate::models_config::ModelsConfig;
use crate::permission::TrustMode;

/// 模型端点配置（定义已迁移至 `tiangong-llm`，此处仅做 re-export 保持外部路径稳定）。
pub use tiangong_llm::ModelEndpoint;

const DEFAULT_CONTEXT_LIMIT: usize = 200_000;
const DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// 默认 context_window（模型名无法解析时的回退值）。
pub fn default_context_limit() -> usize {
    DEFAULT_CONTEXT_LIMIT
}

/// LLM 配置 — TiangongCore 运行所需的模型端点
///
/// core 只关心它运行必需的 chat（主对话）与 lite（轻量任务）端点；
/// 其他能力（image/video/tts/stt/multimodal/embedding/rerank）由各 plugin
/// 自行从 `ModelsConfig` 路由解析，不经此配置中转。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LlmConfig {
    /// 主 Chat 端点（必须）
    pub chat: ModelEndpoint,
    /// 轻量级文本端点（标题生成、意图分类等简单任务，未配置时回退到 chat）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lite: Option<ModelEndpoint>,
}

impl LlmConfig {
    /// 从 ModelsConfig 解析出 core 运行所需的 chat + lite 端点。
    pub fn from_models_config(models: &ModelsConfig) -> Self {
        use crate::models_config::RoutingSlot;

        let resolve = |slot: RoutingSlot| -> Option<ModelEndpoint> {
            let resolved = models.resolve_slot(slot)?;
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
            chat: resolve(RoutingSlot::Chat).unwrap_or_default(),
            lite: resolve(RoutingSlot::Lite),
        }
    }

    /// 检查是否有有效的 Chat 端点
    pub fn is_valid(&self) -> bool {
        !self.chat.base_url.is_empty() && !self.chat.api_key.is_empty()
    }
}

/// TiangongCore 运行所需的最小配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreConfig {
    /// LLM 模型端点配置
    pub llm: LlmConfig,
    /// 权限信任模式
    pub trust_mode: TrustMode,
    /// 新对话默认权限信任模式
    pub default_trust_mode: TrustMode,
    /// 用户自定义 system prompt
    pub custom_system_prompt: String,
    /// 思考强度设置
    #[serde(default = "default_reasoning_effort")]
    #[serde(deserialize_with = "crate::model::deserialize_reasoning_effort_flexible")]
    pub reasoning_effort: crate::model::ReasoningEffort,
    /// 上下文窗口大小（token 数）
    pub context_limit: usize,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            llm: LlmConfig::default(),
            trust_mode: TrustMode::default(),
            default_trust_mode: TrustMode::default(),
            custom_system_prompt: String::new(),
            reasoning_effort: crate::model::ReasoningEffort::Medium,
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

fn default_reasoning_effort() -> crate::model::ReasoningEffort {
    crate::model::ReasoningEffort::Medium
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models_config::ModelCapability;

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
    fn llm_config_from_models_config_preserves_protocol() {
        use crate::models_config::RoutingSlot;

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
        models.routing.insert(
            RoutingSlot::Chat,
            crate::models_config::ModelEntry {
                provider: "anthropic".into(),
                model: "claude-sonnet-4".into(),
                capabilities: vec![ModelCapability::Chat],
                options: serde_json::json!({}),
                context_window: None,
            },
        );

        let llm = LlmConfig::from_models_config(&models);
        assert_eq!(llm.chat.protocol, ProviderProtocol::Anthropic);
    }
}
