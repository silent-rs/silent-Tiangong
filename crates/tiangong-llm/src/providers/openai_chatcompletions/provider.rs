use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use serde::Deserialize;

use crate::error::LlmError;
use crate::model::ProviderModelInfo;
use crate::provider::{LlmProvider, ProviderCapabilities};
use crate::request::ProviderRequest;
use crate::rerank::{RerankProvider, RerankRequest, RerankResponse, RerankResult};
use crate::response::ProviderResponse;
use crate::stream::ProviderStream;

use super::client::OpenAiClient;
use super::config::OpenAiChatConfig;
use super::error::map_openai_error;
use super::mapping::{build_request_json, parse_complete_response};

#[derive(Clone)]
pub struct OpenAiChatCompletionsProvider {
    client: OpenAiClient,
}

#[derive(Clone)]
pub struct OpenAiChatRerankProvider {
    client: OpenAiClient,
    model: String,
}

#[derive(Debug, Deserialize)]
struct OpenAiRerankResponse {
    #[serde(default)]
    results: Vec<OpenAiRerankResult>,
}

#[derive(Debug, Deserialize)]
struct OpenAiRerankResult {
    index: usize,
    #[serde(alias = "score")]
    relevance_score: f64,
}

impl OpenAiChatCompletionsProvider {
    pub fn new(config: OpenAiChatConfig) -> Self {
        Self {
            client: OpenAiClient::new(config),
        }
    }
}

impl OpenAiChatRerankProvider {
    pub fn new(config: OpenAiChatConfig, model: impl Into<String>) -> Self {
        Self {
            client: OpenAiClient::new(config),
            model: model.into(),
        }
    }
}

#[async_trait]
impl LlmProvider for OpenAiChatCompletionsProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tool_calling: true,
            system_prompt: true,
            list_models: true,
        }
    }

    async fn complete(&self, req: ProviderRequest) -> Result<ProviderResponse, LlmError> {
        let model = req.model.clone();
        let payload = build_request_json(&req, false)
            .map_err(|err| LlmError::InvalidRequest(err.to_string()))?;
        let response = self.client.complete(&model, payload).await?;
        parse_complete_response(&response).map_err(|err| LlmError::Provider {
            provider: "openai",
            message: err.to_string(),
        })
    }

    async fn stream(&self, req: ProviderRequest) -> Result<ProviderStream, LlmError> {
        let model = req.model.clone();
        let payload = build_request_json(&req, true)
            .map_err(|err| LlmError::InvalidRequest(err.to_string()))?;
        let stream = self.client.stream(&model, payload).await?;
        let mapped = stream
            .map(|item| match item {
                Ok(payload) => super::stream::parse_stream_payload(&payload),
                Err(err) => vec![Err(map_openai_error(&err))],
            })
            .flat_map(stream::iter);
        Ok(Box::pin(mapped))
    }

    async fn list_models(&self) -> Result<Vec<ProviderModelInfo>, LlmError> {
        self.client.list_models().await
    }
}

#[async_trait]
impl RerankProvider for OpenAiChatRerankProvider {
    async fn rerank(&self, request: RerankRequest) -> anyhow::Result<RerankResponse> {
        let response = self.client.rerank(&self.model, &request).await?;
        let parsed: OpenAiRerankResponse = serde_json::from_value(response)?;
        Ok(RerankResponse {
            results: parsed
                .results
                .into_iter()
                .map(|item| RerankResult {
                    index: item.index,
                    relevance_score: item.relevance_score,
                })
                .collect(),
        })
    }

    fn model(&self) -> &str {
        &self.model
    }
}
