//! Memory 启动参数。
//!
//! 配置文件加载由 `tiangong-config` 等上层 crate 负责；Memory 只接收
//! 已解析好的端点参数，保持自身不依赖 core/config。

use tiangong_llm::ProviderProtocol;

#[derive(Debug, Clone, Default)]
pub struct MemoryOptions {
    pub workspace_id: Option<String>,
    pub model: Option<MemoryModelConfig>,
    pub embedding: Option<MemoryEmbeddingConfig>,
    pub vector_mode: MemoryVectorMode,
}

impl MemoryOptions {
    pub fn new(workspace_id: Option<String>) -> Self {
        Self {
            workspace_id,
            model: None,
            embedding: None,
            vector_mode: MemoryVectorMode::default(),
        }
    }

    pub fn with_model(mut self, model: MemoryModelConfig) -> Self {
        self.model = Some(model);
        self
    }

    pub fn with_embedding(mut self, embedding: MemoryEmbeddingConfig) -> Self {
        self.embedding = Some(embedding);
        self
    }

    pub fn with_vector_mode(mut self, vector_mode: MemoryVectorMode) -> Self {
        self.vector_mode = vector_mode;
        self
    }
}

#[derive(Debug, Clone)]
pub struct MemoryModelConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub protocol: ProviderProtocol,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone)]
pub struct MemoryEmbeddingConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub protocol: ProviderProtocol,
    pub timeout_ms: u64,
    pub dimension: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MemoryVectorMode {
    /// 有 embedding 配置时默认使用内置 flat 向量索引。
    #[default]
    Auto,
    /// 禁用向量层，仅使用 SQLite + Tantivy。
    Disabled,
    /// 使用内置 SQLite flat 向量索引。
    Embedded,
    /// 使用外部 Qdrant 服务。
    ExternalQdrant,
}
