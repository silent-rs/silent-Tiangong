//! Memory 系统独立配置。
//!
//! 该模块只依赖 `tiangong-memory` 和 `tiangong-llm`，用于让 Memory
//! 的模型、Embedding、Rerank 配置脱离主模型路由。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
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
            protocol: ProviderProtocol::default(),
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
            protocol: ProviderProtocol::default(),
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
            protocol: ProviderProtocol::default(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
}

fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

pub fn default_memory_config_path() -> PathBuf {
    storage_root().join("memory").join("config.json")
}

fn user_home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(profile));
    }
    None
}

fn storage_root() -> PathBuf {
    user_home_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".tiangong")
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

    pub fn to_options(&self, workspace_id: Option<String>) -> MemoryOptions {
        let mut options = MemoryOptions::new(workspace_id);

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

        let options = config.to_options(Some("ws-1".into()));

        assert_eq!(options.workspace_id.as_deref(), Some("ws-1"));
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

        let options = config.to_options(None);

        assert!(options.model.is_none());
        assert!(options.embedding.is_none());
        assert!(options.rerank.is_none());
    }
}
