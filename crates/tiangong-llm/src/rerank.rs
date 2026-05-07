//! Rerank 能力抽象层。

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;

use crate::ProviderProtocol;

/// Rerank 能力抽象 trait。
#[async_trait]
pub trait RerankProvider: Send + Sync {
    /// 对候选文档按相关性重新排序。
    async fn rerank(&self, request: RerankRequest) -> Result<RerankResponse>;

    /// 返回模型名称。
    fn model(&self) -> &str;
}

/// Rerank 请求。
#[derive(Debug, Clone)]
pub struct RerankRequest {
    pub query: String,
    pub documents: Vec<String>,
    pub top_n: usize,
}

/// Rerank 响应。
#[derive(Debug, Clone)]
pub struct RerankResponse {
    pub results: Vec<RerankResult>,
}

/// 单条 Rerank 结果。
#[derive(Debug, Clone)]
pub struct RerankResult {
    pub index: usize,
    pub relevance_score: f64,
}

/// Rerank 端点配置。
#[derive(Debug, Clone)]
pub struct RerankEndpointConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub protocol: ProviderProtocol,
    pub timeout: Duration,
}
