use async_trait::async_trait;
use futures_util::{StreamExt, stream};

use crate::error::LlmError;
use crate::model::ProviderModelInfo;
use crate::provider::{LlmProvider, ProviderCapabilities};
use crate::request::ProviderRequest;
use crate::response::ProviderResponse;
use crate::stream::ProviderStream;

use super::client::ResponsesClient;
use super::config::OpenAiResponsesConfig;
use super::error::map_responses_error;
use super::mapping::{build_request_json, parse_complete_response};
use super::stream::parse_stream_event;

#[derive(Clone)]
pub struct OpenAiResponsesProvider {
    client: ResponsesClient,
}

impl OpenAiResponsesProvider {
    pub fn new(config: OpenAiResponsesConfig) -> Self {
        Self {
            client: ResponsesClient::new(config),
        }
    }
}

#[async_trait]
impl LlmProvider for OpenAiResponsesProvider {
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
                Ok(payload) => parse_stream_event(&payload),
                Err(err) => vec![Err(map_responses_error(&err))],
            })
            .flat_map(stream::iter);
        Ok(Box::pin(mapped))
    }

    async fn list_models(&self) -> Result<Vec<ProviderModelInfo>, LlmError> {
        self.client.list_models().await
    }
}
