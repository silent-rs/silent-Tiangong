//! Re-export：模型路由配置类型已迁移至 `tiangong-llm`。
//!
//! 此模块仅做转发，保持 `tiangong_core::models_config::*` 外部路径稳定。
//! 旧版的 `from_legacy` / `to_chat_provider_config` / `to_lite_provider_config` /
//! `from_llm_config` 已随 `ModelProviderConfig` 一并移除（client 直接消费 `ModelEndpoint`）。

pub use tiangong_llm::models_config::{
    ModelCapability, ModelEntry, ModelsConfig, ProviderConfig, ProviderReferences, ResolvedModel,
    RoutingSlot,
};
