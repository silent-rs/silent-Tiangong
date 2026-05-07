//! 统一 provider 构建入口。

use std::sync::Arc;

use anyhow::Result;

use crate::model::ProviderProtocol;
use crate::providers::openai::{OpenAiCompatibleConfig, OpenAiCompatibleRerankProvider};
use crate::rerank::{RerankEndpointConfig, RerankProvider};

/// 根据端点配置创建 RerankProvider。
pub fn rerank_provider_from_config(
    config: &RerankEndpointConfig,
) -> Result<Arc<dyn RerankProvider>> {
    match config.protocol {
        ProviderProtocol::OpenAiCompatible => {
            let mut provider_config =
                OpenAiCompatibleConfig::new(config.api_key.clone(), config.base_url.clone());
            provider_config.timeout = config.timeout;
            Ok(Arc::new(OpenAiCompatibleRerankProvider::new(
                provider_config,
                config.model.clone(),
            )))
        }
        protocol => anyhow::bail!(
            "Rerank 暂不支持 {} 协议，请使用 OpenAI 兼容端点",
            protocol.as_str()
        ),
    }
}
