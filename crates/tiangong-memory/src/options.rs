//! Memory 启动参数。
//!
//! 配置文件加载由 `tiangong-config` 等上层 crate 负责；Memory 只接收
//! 已解析好的端点参数，保持自身不依赖 core/config。

use tiangong_llm::ProviderProtocol;

#[derive(Debug, Clone, Default)]
pub struct MemoryOptions {
    pub workspace_id: Option<String>,
    pub embedding: Option<MemoryEmbeddingConfig>,
}

impl MemoryOptions {
    pub fn new(workspace_id: Option<String>) -> Self {
        Self {
            workspace_id,
            embedding: None,
        }
    }

    pub fn with_embedding(mut self, embedding: MemoryEmbeddingConfig) -> Self {
        self.embedding = Some(embedding);
        self
    }
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
