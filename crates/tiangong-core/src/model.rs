//! Re-export：client / 数据类型 / 协议已迁移至 `tiangong-llm`。
//!
//! 此模块仅做转发，保持 `tiangong_core::model::*` 外部路径稳定。
//! 旧的 `ModelProviderConfig`（含 `from_env` / `default_*` 辅助函数）已彻底删除——
//! client 直接消费扁平的 [`tiangong_llm::ModelEndpoint`]。

pub use tiangong_llm::ProviderProtocol;
pub use tiangong_llm::StopReason;
pub use tiangong_llm::provider_client::{
    ModelClient, ModelFunctionResponse, ModelRequest, ModelResponse, ModelStreamChunk,
    OnRetryCallback, SingleProviderClient, ToolCallArgumentFailure,
};
pub use tiangong_llm::request::{ReasoningEffort, ThinkingConfig};
pub use tiangong_llm::tool::{ToolCall, ToolChoice, ToolSpec};
pub use tiangong_types::TokenUsage;
