//! Embedding 能力抽象层
//!
//! 提供 `EmbeddingProvider` trait + OpenAI 兼容实现（POST /v1/embeddings）。

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Embedding 能力抽象 trait
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// 将一批文本嵌入为向量
    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>>;

    /// 返回向量维度（用于 Qdrant collection 初始化）
    fn dimension(&self) -> usize;

    /// 返回模型名称
    fn model(&self) -> &str;
}

// ==================== OpenAI 兼容实现 ====================

#[derive(Debug, Serialize)]
struct EmbeddingRequest<'a> {
    model: &'a str,
    input: &'a [String],
}

#[derive(Debug, Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
    index: usize,
}

/// OpenAI 兼容的 Embedding Provider
///
/// 支持任何实现 OpenAI `/v1/embeddings` 接口的服务。
#[derive(Debug, Clone)]
pub struct OpenAiEmbeddingProvider {
    base_url: String,
    api_key: String,
    model: String,
    dimension: usize,
    client: reqwest::Client,
}

impl OpenAiEmbeddingProvider {
    /// 创建一个新的 OpenAI 兼容 Embedding Provider
    ///
    /// # 参数
    /// - `base_url`: API 基础 URL（如 `https://api.openai.com`）
    /// - `api_key`: API 密钥
    /// - `model`: 模型名（如 `text-embedding-3-small`）
    /// - `dimension`: 向量维度（text-embedding-3-small=1536, ada-002=1536）
    pub fn new(base_url: &str, api_key: &str, model: &str, dimension: usize) -> Self {
        let base_url = base_url.trim_end_matches('/').to_string();
        Self {
            base_url,
            api_key: api_key.to_string(),
            model: model.to_string(),
            dimension,
            client: reqwest::Client::new(),
        }
    }

    /// 从环境变量创建（`API_BASE_URL`, `API_AUTH_TOKEN`）
    pub fn from_env(model: &str, dimension: usize) -> Option<Self> {
        let base_url = std::env::var("API_BASE_URL").ok()?;
        let api_key = std::env::var("API_AUTH_TOKEN").ok()?;
        Some(Self::new(&base_url, &api_key, model, dimension))
    }
}

#[async_trait]
impl EmbeddingProvider for OpenAiEmbeddingProvider {
    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let url = format!("{}/v1/embeddings", self.base_url);
        let req = EmbeddingRequest {
            model: &self.model,
            input: &texts,
        };

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&req)
            .send()
            .await
            .with_context(|| format!("Embedding 请求失败: {url}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Embedding API 返回错误 {status}: {body}");
        }

        let mut result: EmbeddingResponse = resp.json().await.context("解析 Embedding 响应失败")?;

        // 按 index 排序，保证顺序与输入一致
        result.data.sort_by_key(|d| d.index);
        Ok(result.data.into_iter().map(|d| d.embedding).collect())
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn model(&self) -> &str {
        &self.model
    }
}
