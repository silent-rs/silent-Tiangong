//! Memory 启动参数。
//!
//! Memory 的独立磁盘配置定义在 `config` 模块；这里保留 actor
//! 启动时消费的已解析参数。

use serde::{Deserialize, Serialize};
use tiangong_llm::{EmbeddingEndpointConfig, LlmEndpointConfig, RerankEndpointConfig};

#[derive(Debug, Clone, Default)]
pub struct MemoryOptions {
    pub workspace_id: Option<String>,
    pub model: Option<LlmEndpointConfig>,
    pub embedding: Option<EmbeddingEndpointConfig>,
    pub rerank: Option<RerankEndpointConfig>,
    pub vector_mode: MemoryVectorMode,
}

impl MemoryOptions {
    pub fn new(workspace_id: Option<String>) -> Self {
        Self {
            workspace_id,
            model: None,
            embedding: None,
            rerank: None,
            vector_mode: MemoryVectorMode::default(),
        }
    }

    pub fn with_model(mut self, model: LlmEndpointConfig) -> Self {
        self.model = Some(model);
        self
    }

    pub fn with_embedding(mut self, embedding: EmbeddingEndpointConfig) -> Self {
        self.embedding = Some(embedding);
        self
    }

    pub fn with_rerank(mut self, rerank: RerankEndpointConfig) -> Self {
        self.rerank = Some(rerank);
        self
    }

    pub fn with_vector_mode(mut self, vector_mode: MemoryVectorMode) -> Self {
        self.vector_mode = vector_mode;
        self
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryVectorMode {
    /// 有 embedding 配置时默认使用嵌入式 Qdrant Edge HNSW 索引。
    #[default]
    Auto,
    /// 禁用向量层，仅使用 SQLite + Tantivy。
    Disabled,
    /// 使用内置 SQLite flat 向量索引。
    Embedded,
    /// 使用嵌入式 Qdrant Edge HNSW 索引。
    EmbeddedQdrantEdge,
    /// 使用外部 Qdrant 服务。
    ExternalQdrant,
}
