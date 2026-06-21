use std::time::Duration;

use futures_util::Stream;
use serde_json::Value;
use tokio::time::timeout;

use crate::error::LlmError;
use crate::model::ProviderModelInfo;

use super::config::OpenAiResponsesConfig;
use super::error::{is_retryable_responses_error, map_responses_error};
use super::mapping::normalize_api_base;

type ResponsesByotStream =
    std::pin::Pin<Box<dyn Stream<Item = Result<Value, async_openai::error::OpenAIError>> + Send>>;

const INITIAL_RETRY_DELAY_MS: u64 = 1000;

#[derive(Clone)]
pub struct ResponsesClient {
    config: OpenAiResponsesConfig,
}

impl ResponsesClient {
    pub fn new(config: OpenAiResponsesConfig) -> Self {
        Self { config }
    }

    pub async fn complete(&self, model: &str, payload: Value) -> Result<Value, LlmError> {
        let client = self.build_client();
        let responses = client.responses();
        timeout(
            self.config.timeout,
            self.with_retry("openai_complete", model, false, || {
                responses.create_byot::<_, Value>(payload.clone())
            }),
        )
        .await
        .map_err(|_| LlmError::Timeout(self.config.timeout.as_millis() as u64))?
    }

    pub async fn stream(
        &self,
        model: &str,
        payload: Value,
    ) -> Result<ResponsesByotStream, LlmError> {
        let client = self.build_client();
        let responses = client.responses();
        self.with_retry("openai_stream", model, true, || {
            responses.create_stream_byot::<_, Value>(payload.clone())
        })
        .await
    }

    pub async fn list_models(&self) -> Result<Vec<ProviderModelInfo>, LlmError> {
        // Responses 与 Chat 共用 /models 端点，复用 Chat Completions 的实现。
        crate::providers::openai_chatcompletions::client::list_models_via_config(
            &self.config.api_key,
            &self.config.base_url,
            self.config.timeout,
            "openai",
        )
        .await
    }

    fn build_client(&self) -> async_openai::Client<async_openai::config::OpenAIConfig> {
        let config = async_openai::config::OpenAIConfig::new()
            .with_api_key(self.config.api_key.clone())
            .with_api_base(
                normalize_api_base(&self.config.base_url)
                    .unwrap_or_else(|_| self.config.base_url.clone()),
            );
        async_openai::Client::with_config(config)
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
        let max_retries = self.config.max_retries;
        loop {
            let start = std::time::Instant::now();
            tracing::info!(
                operation,
                provider = "openai",
                model,
                stream,
                attempt,
                "开始 OpenAI Responses 请求"
            );
            match f().await {
                Ok(value) => {
                    tracing::info!(
                        operation,
                        provider = "openai",
                        model,
                        stream,
                        attempt,
                        latency_ms = start.elapsed().as_millis() as u64,
                        "OpenAI Responses 请求完成"
                    );
                    return Ok(value);
                }
                Err(err) if attempt < max_retries && is_retryable_responses_error(&err) => {
                    attempt += 1;
                    if let Some(notifier) = &self.config.retry_notifier {
                        notifier(attempt, max_retries, delay_ms, &err.to_string());
                    }
                    tracing::warn!(
                        operation,
                        provider = "openai",
                        model,
                        stream,
                        attempt,
                        delay_ms,
                        error = %err,
                        "OpenAI Responses 请求失败，准备重试"
                    );
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    delay_ms *= 2;
                }
                Err(err) => {
                    tracing::warn!(
                        operation,
                        provider = "openai",
                        model,
                        stream,
                        attempt,
                        latency_ms = start.elapsed().as_millis() as u64,
                        error = %err,
                        "OpenAI Responses 请求失败"
                    );
                    return Err(map_responses_error(&err));
                }
            }
        }
    }
}
