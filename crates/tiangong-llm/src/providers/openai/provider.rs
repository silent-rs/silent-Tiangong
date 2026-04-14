use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use serde_json::Value;
use tokio::time::timeout;

use crate::error::LlmError;
use crate::model::ProviderModelInfo;
use crate::provider::{LlmProvider, ProviderCapabilities};
use crate::providers::openai::config::OpenAiCompatibleConfig;
use crate::providers::openai::error::{is_retryable_openai_error, map_openai_error};
use crate::providers::openai::mapping::{
    build_request_json, normalize_api_base, parse_complete_response,
};
use crate::request::ProviderRequest;
use crate::response::ProviderResponse;
use crate::stream::ProviderStream;

const MAX_RETRIES: u32 = 3;
const INITIAL_RETRY_DELAY_MS: u64 = 1000;

#[derive(Clone)]
pub struct OpenAiCompatibleProvider {
    config: OpenAiCompatibleConfig,
}

impl OpenAiCompatibleProvider {
    pub fn new(config: OpenAiCompatibleConfig) -> Self {
        Self { config }
    }

    fn build_client(&self) -> async_openai::Client<async_openai::config::OpenAIConfig> {
        let config = async_openai::config::OpenAIConfig::new()
            .with_api_key(self.config.api_key.clone())
            .with_api_base(
                normalize_api_base(&self.config.base_url)
                    .unwrap_or_else(|_| self.config.base_url.clone()),
            );
        let no_retry = backoff::ExponentialBackoff {
            max_elapsed_time: Some(Duration::from_nanos(1)),
            ..Default::default()
        };
        async_openai::Client::build(reqwest::Client::new(), config, no_retry)
    }

    async fn with_retry<F, Fut, T>(
        &self,
        operation: &'static str,
        model: &str,
        stream: bool,
        mut f: F,
    ) -> Result<T, LlmError>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T, async_openai::error::OpenAIError>>,
    {
        let mut attempt = 0u32;
        let mut delay_ms = INITIAL_RETRY_DELAY_MS;
        let max_retries = self.config.max_retries.max(MAX_RETRIES);
        loop {
            let start = std::time::Instant::now();
            tracing::info!(
                operation,
                provider = "openai_compatible",
                model,
                stream,
                attempt,
                "开始 OpenAI 兼容请求"
            );
            match f().await {
                Ok(value) => {
                    tracing::info!(
                        operation,
                        provider = "openai_compatible",
                        model,
                        stream,
                        attempt,
                        latency_ms = start.elapsed().as_millis() as u64,
                        "OpenAI 兼容请求完成"
                    );
                    return Ok(value);
                }
                Err(err) if attempt < max_retries && is_retryable_openai_error(&err) => {
                    attempt += 1;
                    if let Some(notifier) = &self.config.retry_notifier {
                        notifier(attempt, max_retries, delay_ms, &err.to_string());
                    }
                    tracing::warn!(
                        operation,
                        provider = "openai_compatible",
                        model,
                        stream,
                        attempt,
                        delay_ms,
                        error = %err,
                        "OpenAI 兼容请求失败，准备重试"
                    );
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    delay_ms *= 2;
                }
                Err(err) => {
                    tracing::warn!(
                        operation,
                        provider = "openai_compatible",
                        model,
                        stream,
                        attempt,
                        latency_ms = start.elapsed().as_millis() as u64,
                        error = %err,
                        "OpenAI 兼容请求失败"
                    );
                    return Err(map_openai_error(&err));
                }
            }
        }
    }
}

#[async_trait]
impl LlmProvider for OpenAiCompatibleProvider {
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
        let client = self.build_client();
        let payload = build_request_json(&req, false)
            .map_err(|err| LlmError::InvalidRequest(err.to_string()))?;
        let chat = client.chat();
        let response: Value = timeout(
            self.config.timeout,
            self.with_retry("openai_complete", &model, false, || {
                chat.create_byot::<_, Value>(payload.clone())
            }),
        )
        .await
        .map_err(|_| LlmError::Timeout(self.config.timeout.as_millis() as u64))??;
        parse_complete_response(&response).map_err(|err| LlmError::Provider {
            provider: "openai_compatible",
            message: err.to_string(),
        })
    }

    async fn stream(&self, req: ProviderRequest) -> Result<ProviderStream, LlmError> {
        let model = req.model.clone();
        let client = self.build_client();
        let payload = build_request_json(&req, true)
            .map_err(|err| LlmError::InvalidRequest(err.to_string()))?;
        let chat = client.chat();
        let stream = timeout(
            self.config.timeout,
            self.with_retry("openai_stream", &model, true, || {
                chat.create_stream_byot::<_, Value>(payload.clone())
            }),
        )
        .await
        .map_err(|_| LlmError::Timeout(self.config.timeout.as_millis() as u64))??;

        let mapped = stream
            .map(|item| match item {
                Ok(payload) => super::stream::parse_stream_payload(&payload),
                Err(err) => vec![Err(map_openai_error(&err))],
            })
            .flat_map(stream::iter);
        Ok(Box::pin(mapped))
    }

    async fn list_models(&self) -> Result<Vec<ProviderModelInfo>, LlmError> {
        let start = std::time::Instant::now();
        tracing::info!(
            operation = "openai_list_models",
            provider = "openai_compatible",
            model = "<list_models>",
            stream = false,
            attempt = 0,
            "开始 OpenAI 兼容请求"
        );
        let base = normalize_api_base(&self.config.base_url)
            .map_err(|err| LlmError::Configuration(err.to_string()))?;
        let url = format!("{base}/models");
        let client = reqwest::Client::builder()
            .timeout(self.config.timeout)
            .build()
            .map_err(|err| LlmError::Transport(err.to_string()))?;
        let response = client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .send()
            .await
            .with_context(|| format!("请求模型列表失败：{url}"))
            .map_err(|err| LlmError::Transport(err.to_string()))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            tracing::warn!(
                operation = "openai_list_models",
                provider = "openai_compatible",
                model = "<list_models>",
                stream = false,
                attempt = 0,
                latency_ms = start.elapsed().as_millis() as u64,
                status = status.as_u16(),
                "OpenAI 兼容请求失败"
            );
            let message = format!("获取模型列表失败：HTTP {status}，响应：{body}");
            if status.as_u16() == 429
                || status.as_u16() == 529
                || body.to_ascii_lowercase().contains("rate limit")
                || body.to_ascii_lowercase().contains("too many requests")
                || body.to_ascii_lowercase().contains("overloaded_error")
            {
                return Err(LlmError::RateLimited(message));
            }
            return Err(LlmError::Provider {
                provider: "openai_compatible",
                message,
            });
        }
        #[derive(serde::Deserialize)]
        struct ModelEntry {
            id: String,
        }
        #[derive(serde::Deserialize)]
        struct ModelsResponse {
            data: Vec<ModelEntry>,
        }
        let body = response
            .text()
            .await
            .map_err(|err| LlmError::Transport(err.to_string()))?;
        let parsed: ModelsResponse =
            serde_json::from_str(&body).map_err(|err| LlmError::Provider {
                provider: "openai_compatible",
                message: format!("failed to deserialize api response: {err}: {body}"),
            })?;
        tracing::info!(
            operation = "openai_list_models",
            provider = "openai_compatible",
            model = "<list_models>",
            stream = false,
            attempt = 0,
            latency_ms = start.elapsed().as_millis() as u64,
            "OpenAI 兼容请求完成"
        );
        Ok(parsed
            .data
            .into_iter()
            .map(|entry| ProviderModelInfo {
                id: entry.id,
                display_name: None,
            })
            .collect())
    }
}
