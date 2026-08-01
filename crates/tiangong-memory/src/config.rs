//! Memory 系统独立配置。
//!
//! 该模块只依赖 `tiangong-memory` 和 `tiangong-llm`，用于让 Memory
//! 的模型、Embedding、Rerank 配置脱离主模型路由。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tiangong_llm::models_config::{ModelEntry, ModelsConfig, ResolvedModel, RoutingSlot};
use tiangong_llm::{
    EmbeddingEndpointConfig, LlmEndpointConfig, ProviderProtocol, RerankEndpointConfig,
};

use crate::{MemoryOptions, MemoryVectorMode};

const DEFAULT_TIMEOUT_MS: u64 = 60_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<MemoryLlmConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<MemoryEmbeddingConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank: Option<MemoryRerankConfig>,
    #[serde(default)]
    pub vector_mode: MemoryVectorMode,
}

/// Memory 设置页保存的模型选择。
///
/// 页面只接触主模型配置中的 key；端点地址和密钥在宿主侧解析，避免暴露给 iframe。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfigSelection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank_key: Option<String>,
    #[serde(default = "default_vector_mode_selection")]
    pub vector_mode: String,
}

impl Default for MemoryConfigSelection {
    fn default() -> Self {
        Self {
            model_key: None,
            embedding_key: None,
            rerank_key: None,
            vector_mode: default_vector_mode_selection(),
        }
    }
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            model: None,
            embedding: None,
            rerank: None,
            vector_mode: MemoryVectorMode::Auto,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryLlmConfig {
    #[serde(default, alias = "provider", skip_serializing_if = "Option::is_none")]
    pub provider_key: Option<String>,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    #[serde(default)]
    pub protocol: ProviderProtocol,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

impl Default for MemoryLlmConfig {
    fn default() -> Self {
        Self {
            provider_key: None,
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            // Memory 模型端点通常为第三方 OpenAI 兼容服务，默认走 Chat Completions。
            protocol: ProviderProtocol::OpenAiChatCompletions,
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEmbeddingConfig {
    #[serde(default, alias = "provider", skip_serializing_if = "Option::is_none")]
    pub provider_key: Option<String>,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    #[serde(default)]
    pub protocol: ProviderProtocol,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub dimension: usize,
}

impl Default for MemoryEmbeddingConfig {
    fn default() -> Self {
        Self {
            provider_key: None,
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            // Embedding 端点通常为第三方 OpenAI 兼容服务，默认走 Chat Completions。
            protocol: ProviderProtocol::OpenAiChatCompletions,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            dimension: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRerankConfig {
    #[serde(default, alias = "provider", skip_serializing_if = "Option::is_none")]
    pub provider_key: Option<String>,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    #[serde(default)]
    pub protocol: ProviderProtocol,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

impl Default for MemoryRerankConfig {
    fn default() -> Self {
        Self {
            provider_key: None,
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            // Rerank 端点通常为第三方 OpenAI 兼容服务，默认走 Chat Completions。
            protocol: ProviderProtocol::OpenAiChatCompletions,
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
}

fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

fn default_vector_mode_selection() -> String {
    "auto".to_string()
}

pub fn default_memory_config_path() -> PathBuf {
    crate::paths::memory_data_dir().join("config.json")
}

/// Memory 禁用标记文件路径：~/.tiangong/memory/.disabled
///
/// 用于 `tiangong memory enable/disable` 实现对称开关（RFC 0015 §6.3）。
/// MemoryConfig 无顶层 enabled 字段，改用此标记文件存在性表示禁用，
/// 不破坏 MemoryConfig 结构、不丢失端点配置。
pub fn memory_disabled_marker_path() -> PathBuf {
    crate::paths::memory_data_dir().join(".disabled")
}

/// 判断 Memory 是否被显式禁用（标记文件存在即禁用）。
pub fn is_memory_disabled() -> bool {
    memory_disabled_marker_path().exists()
}

/// 禁用 Memory（创建标记文件）。
pub fn disable_memory() -> Result<()> {
    disable_memory_at(&memory_disabled_marker_path())
}

/// 在指定路径创建禁用标记（供测试使用）。
pub fn disable_memory_at(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建目录失败：{}", parent.display()))?;
    }
    fs::write(path, "disabled by `tiangong memory disable`\n")
        .with_context(|| format!("写入禁用标记失败：{}", path.display()))
}

/// 启用 Memory（删除标记文件）。文件不存在视为已启用。
pub fn enable_memory() -> Result<()> {
    enable_memory_at(&memory_disabled_marker_path())
}

/// 在指定路径删除禁用标记（供测试使用）。
pub fn enable_memory_at(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    fs::remove_file(path).with_context(|| format!("删除禁用标记失败：{}", path.display()))
}

fn resolve_api_key(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.starts_with("${") && trimmed.ends_with('}') && trimmed.len() > 3 {
        let name = &trimmed[2..trimmed.len() - 1];
        std::env::var(name).unwrap_or_else(|_| value.to_string())
    } else {
        value.to_string()
    }
}

fn is_text_endpoint_valid(base_url: &str, api_key: &str, model: &str) -> bool {
    !base_url.trim().is_empty() && !api_key.trim().is_empty() && !model.trim().is_empty()
}

impl MemoryConfig {
    pub fn load() -> Result<Self> {
        Self::load_from_path(&default_memory_config_path())
    }

    pub fn load_or_default() -> Self {
        match Self::load() {
            Ok(config) => config,
            Err(err) => {
                tracing::debug!("读取 Memory 独立配置失败，使用默认配置: {err}");
                Self::default()
            }
        }
    }

    pub fn load_from_path(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(path)
            .with_context(|| format!("读取 Memory 配置失败：{}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("解析 Memory 配置失败：{}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        self.save_to_path(&default_memory_config_path())
    }

    pub fn save_to_path(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建 Memory 配置目录失败：{}", parent.display()))?;
        }
        let content = serde_json::to_string_pretty(self).context("序列化 Memory 配置失败")?;
        fs::write(path, content)
            .with_context(|| format!("写入 Memory 配置失败：{}", path.display()))
    }

    pub fn to_options(&self) -> MemoryOptions {
        let mut options = MemoryOptions::new();

        if let Some(model) = self
            .model
            .as_ref()
            .filter(|model| is_text_endpoint_valid(&model.base_url, &model.api_key, &model.model))
        {
            options = options.with_model(LlmEndpointConfig {
                source_provider: model.provider_key.clone(),
                base_url: model.base_url.clone(),
                api_key: resolve_api_key(&model.api_key),
                model: model.model.clone(),
                protocol: model.protocol,
                timeout: Duration::from_millis(model.timeout_ms),
                max_retries: 3,
            });
        }

        if let Some(embedding) = self.embedding.as_ref().filter(|embedding| {
            is_text_endpoint_valid(&embedding.base_url, &embedding.api_key, &embedding.model)
                && embedding.dimension > 0
        }) {
            options = options.with_embedding(EmbeddingEndpointConfig {
                base_url: embedding.base_url.clone(),
                api_key: resolve_api_key(&embedding.api_key),
                model: embedding.model.clone(),
                protocol: embedding.protocol,
                timeout: Duration::from_millis(embedding.timeout_ms),
                dimension: embedding.dimension,
            });
        }

        if let Some(rerank) = self.rerank.as_ref().filter(|rerank| {
            is_text_endpoint_valid(&rerank.base_url, &rerank.api_key, &rerank.model)
        }) {
            options = options.with_rerank(RerankEndpointConfig {
                base_url: rerank.base_url.clone(),
                api_key: resolve_api_key(&rerank.api_key),
                model: rerank.model.clone(),
                protocol: rerank.protocol,
                timeout: Duration::from_millis(rerank.timeout_ms),
            });
        }

        options.with_vector_mode(self.vector_mode)
    }

    /// 将已解析的运行参数转换为可通过 IPC 传递的配置。
    pub fn from_options(options: &MemoryOptions) -> Self {
        Self {
            model: options.model.as_ref().map(|model| MemoryLlmConfig {
                provider_key: model.source_provider.clone(),
                base_url: model.base_url.clone(),
                api_key: model.api_key.clone(),
                model: model.model.clone(),
                protocol: model.protocol,
                timeout_ms: duration_millis(model.timeout),
            }),
            embedding: options
                .embedding
                .as_ref()
                .map(|embedding| MemoryEmbeddingConfig {
                    provider_key: None,
                    base_url: embedding.base_url.clone(),
                    api_key: embedding.api_key.clone(),
                    model: embedding.model.clone(),
                    protocol: embedding.protocol,
                    timeout_ms: duration_millis(embedding.timeout),
                    dimension: embedding.dimension,
                }),
            rerank: options.rerank.as_ref().map(|rerank| MemoryRerankConfig {
                provider_key: None,
                base_url: rerank.base_url.clone(),
                api_key: rerank.api_key.clone(),
                model: rerank.model.clone(),
                protocol: rerank.protocol,
                timeout_ms: duration_millis(rerank.timeout),
            }),
            vector_mode: options.vector_mode,
        }
    }
}

impl MemoryConfigSelection {
    pub fn from_memory(config: &MemoryConfig, models: &ModelsConfig) -> Self {
        Self {
            model_key: config.model.as_ref().and_then(|endpoint| {
                find_model_key(
                    models,
                    &endpoint.base_url,
                    &endpoint.model,
                    endpoint.protocol,
                )
            }),
            embedding_key: config.embedding.as_ref().and_then(|endpoint| {
                find_model_key(
                    models,
                    &endpoint.base_url,
                    &endpoint.model,
                    endpoint.protocol,
                )
            }),
            rerank_key: config.rerank.as_ref().and_then(|endpoint| {
                find_model_key(
                    models,
                    &endpoint.base_url,
                    &endpoint.model,
                    endpoint.protocol,
                )
            }),
            vector_mode: vector_mode_key(config.vector_mode).to_string(),
        }
    }

    pub fn to_memory(&self, models: &ModelsConfig) -> Result<MemoryConfig> {
        Ok(MemoryConfig {
            model: selected_key(&self.model_key)
                .map(|key| resolve_memory_llm(models, key))
                .transpose()?,
            embedding: selected_key(&self.embedding_key)
                .map(|key| resolve_memory_embedding(models, key))
                .transpose()?,
            rerank: selected_key(&self.rerank_key)
                .map(|key| resolve_memory_rerank(models, key))
                .transpose()?,
            vector_mode: parse_vector_mode(&self.vector_mode),
        })
    }
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn selected_key(value: &Option<String>) -> Option<&str> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn resolved_model_by_key(models: &ModelsConfig, model_key: &str) -> Result<ResolvedModel> {
    if let Some(slot) = RoutingSlot::from_key(model_key)
        && let Some(resolved) = models.resolve_slot(slot)
    {
        return Ok(resolved);
    }

    let resolve_entry = |entry: &ModelEntry| {
        let provider = models.providers.get(&entry.provider)?;
        Some(ResolvedModel {
            provider: entry.provider.clone(),
            base_url: provider.base_url.clone(),
            api_key: ModelsConfig::resolve_api_key(&provider.api_key),
            timeout_ms: provider.timeout_ms,
            protocol: provider.protocol,
            context_window: entry.context_window,
            model: entry.model.clone(),
            options: entry.options.clone(),
        })
    };

    if let Some(entry) = models.models.get(model_key) {
        return resolve_entry(entry)
            .ok_or_else(|| anyhow::anyhow!("模型 {model_key} 引用的 Provider 不存在"));
    }

    models
        .routing
        .values()
        .find_map(|entry| {
            (entry.model == model_key)
                .then(|| resolve_entry(entry))
                .flatten()
        })
        .ok_or_else(|| anyhow::anyhow!("模型不存在：{model_key}"))
}

fn resolve_memory_llm(models: &ModelsConfig, model_key: &str) -> Result<MemoryLlmConfig> {
    let resolved = resolved_model_by_key(models, model_key)?;
    Ok(MemoryLlmConfig {
        provider_key: Some(resolved.provider),
        base_url: resolved.base_url,
        api_key: resolved.api_key,
        model: resolved.model,
        protocol: resolved.protocol,
        timeout_ms: resolved.timeout_ms,
    })
}

fn resolve_memory_embedding(
    models: &ModelsConfig,
    model_key: &str,
) -> Result<MemoryEmbeddingConfig> {
    let resolved = resolved_model_by_key(models, model_key)?;
    let dimension = resolved
        .options
        .get("dimension")
        .and_then(|value| value.as_u64())
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| anyhow::anyhow!("Embedding 模型 {model_key} 缺少 options.dimension"))?;
    Ok(MemoryEmbeddingConfig {
        provider_key: Some(resolved.provider),
        base_url: resolved.base_url,
        api_key: resolved.api_key,
        model: resolved.model,
        protocol: resolved.protocol,
        timeout_ms: resolved.timeout_ms,
        dimension,
    })
}

fn resolve_memory_rerank(models: &ModelsConfig, model_key: &str) -> Result<MemoryRerankConfig> {
    let resolved = resolved_model_by_key(models, model_key)?;
    Ok(MemoryRerankConfig {
        provider_key: Some(resolved.provider),
        base_url: resolved.base_url,
        api_key: resolved.api_key,
        model: resolved.model,
        protocol: resolved.protocol,
        timeout_ms: resolved.timeout_ms,
    })
}

fn find_model_key(
    models: &ModelsConfig,
    base_url: &str,
    model_name: &str,
    protocol: ProviderProtocol,
) -> Option<String> {
    models
        .models
        .iter()
        .find_map(|(key, entry)| {
            let provider = models.providers.get(&entry.provider)?;
            (provider.base_url == base_url
                && provider.protocol == protocol
                && entry.model == model_name)
                .then(|| key.clone())
        })
        .or_else(|| {
            models.routing.iter().find_map(|(slot, entry)| {
                let provider = models.providers.get(&entry.provider)?;
                (provider.base_url == base_url
                    && provider.protocol == protocol
                    && entry.model == model_name)
                    .then(|| slot.key().to_string())
            })
        })
}

fn vector_mode_key(mode: MemoryVectorMode) -> &'static str {
    match mode {
        MemoryVectorMode::Auto => "auto",
        MemoryVectorMode::Disabled => "disabled",
        MemoryVectorMode::EmbeddedLanceDb => "embedded_lance_db",
    }
}

fn parse_vector_mode(value: &str) -> MemoryVectorMode {
    match value.trim().to_ascii_lowercase().as_str() {
        "disabled" => MemoryVectorMode::Disabled,
        "embedded" | "lancedb" | "embedded_lancedb" | "embedded_lance_db" => {
            MemoryVectorMode::EmbeddedLanceDb
        }
        _ => MemoryVectorMode::Auto,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_dedicated_memory_config_to_options() {
        let config = MemoryConfig {
            model: Some(MemoryLlmConfig {
                base_url: "https://memory.example/v1".into(),
                api_key: "sk-memory".into(),
                model: "memory-small".into(),
                timeout_ms: 20_000,
                ..Default::default()
            }),
            embedding: Some(MemoryEmbeddingConfig {
                base_url: "https://embedding.example/v1".into(),
                api_key: "sk-embedding".into(),
                model: "bge-m3".into(),
                dimension: 1024,
                timeout_ms: 10_000,
                ..Default::default()
            }),
            rerank: Some(MemoryRerankConfig {
                base_url: "https://rerank.example/v1".into(),
                api_key: "sk-rerank".into(),
                model: "bge-reranker".into(),
                ..Default::default()
            }),
            vector_mode: MemoryVectorMode::EmbeddedLanceDb,
        };

        let options = config.to_options();

        assert_eq!(options.vector_mode, MemoryVectorMode::EmbeddedLanceDb);
        assert_eq!(options.model.expect("model 应存在").model, "memory-small");
        assert_eq!(options.embedding.expect("embedding 应存在").dimension, 1024);
        assert_eq!(options.rerank.expect("rerank 应存在").model, "bge-reranker");
    }

    #[test]
    fn ignores_incomplete_endpoints() {
        let config = MemoryConfig {
            model: Some(MemoryLlmConfig {
                base_url: "https://memory.example/v1".into(),
                api_key: String::new(),
                model: "memory-small".into(),
                ..Default::default()
            }),
            embedding: Some(MemoryEmbeddingConfig {
                base_url: "https://embedding.example/v1".into(),
                api_key: "sk-embedding".into(),
                model: "bge-m3".into(),
                dimension: 0,
                ..Default::default()
            }),
            rerank: Some(MemoryRerankConfig {
                base_url: "https://rerank.example/v1".into(),
                api_key: String::new(),
                model: "bge-reranker".into(),
                ..Default::default()
            }),
            ..Default::default()
        };

        let options = config.to_options();

        assert!(options.model.is_none());
        assert!(options.embedding.is_none());
        assert!(options.rerank.is_none());
    }

    #[test]
    fn disable_enable_marker_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join(".disabled");

        assert!(!marker.exists());
        disable_memory_at(&marker).unwrap();
        assert!(marker.exists());

        enable_memory_at(&marker).unwrap();
        assert!(!marker.exists());
        // 再次启用不报错
        enable_memory_at(&marker).unwrap();
    }
}
