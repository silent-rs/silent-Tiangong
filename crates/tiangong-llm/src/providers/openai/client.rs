use std::time::Duration;

use anyhow::Context;
use futures_util::Stream;
use serde_json::{Value, json};
use tokio::time::timeout;

use crate::error::LlmError;
use crate::model::ProviderModelInfo;
use crate::rerank::RerankRequest;

use super::config::OpenAiCompatibleConfig;
use super::error::{is_retryable_openai_error, map_openai_error};
use super::mapping::normalize_api_base;

type OpenAiByotStream =
    std::pin::Pin<Box<dyn Stream<Item = Result<Value, async_openai::error::OpenAIError>> + Send>>;

const MAX_RETRIES: u32 = 3;
const INITIAL_RETRY_DELAY_MS: u64 = 1000;

#[derive(Clone)]
pub struct OpenAiClient {
    config: OpenAiCompatibleConfig,
}

impl OpenAiClient {
    pub fn new(config: OpenAiCompatibleConfig) -> Self {
        Self { config }
    }

    pub async fn complete(&self, model: &str, payload: Value) -> Result<Value, LlmError> {
        let client = self.build_client();
        let chat = client.chat();
        timeout(
            self.config.timeout,
            self.with_retry("openai_complete", model, false, || {
                chat.create_byot::<_, Value>(payload.clone())
            }),
        )
        .await
        .map_err(|_| LlmError::Timeout(self.config.timeout.as_millis() as u64))?
    }

    pub async fn stream(&self, model: &str, payload: Value) -> Result<OpenAiByotStream, LlmError> {
        let client = self.build_client();
        let chat = client.chat();
        self.with_retry("openai_stream", model, true, || {
            chat.create_stream_byot::<_, Value>(payload.clone())
        })
        .await
    }

    pub async fn rerank(&self, model: &str, request: &RerankRequest) -> Result<Value, LlmError> {
        if request.documents.is_empty() {
            return Ok(json!({ "results": [] }));
        }

        let base = normalize_rerank_api_base(&self.config.base_url)
            .map_err(|err| LlmError::Configuration(err.to_string()))?;
        let url = format!("{base}/rerank");
        let payload = json!({
            "model": model,
            "query": &request.query,
            "documents": &request.documents,
            "top_n": request.top_n.max(1).min(request.documents.len()),
        });
        let client = reqwest::Client::builder()
            .timeout(self.config.timeout)
            .build()
            .map_err(|err| LlmError::Transport(err.to_string()))?;

        let mut req = client.post(&url).json(&payload);
        if !self.config.api_key.trim().is_empty() {
            req = req.bearer_auth(&self.config.api_key);
        }

        let start = std::time::Instant::now();
        tracing::info!(
            operation = "openai_rerank",
            provider = "openai_compatible",
            model,
            stream = false,
            attempt = 0,
            "开始 OpenAI 兼容请求"
        );
        let response = req
            .send()
            .await
            .with_context(|| format!("Rerank 请求失败：{url}"))
            .map_err(|err| LlmError::Transport(err.to_string()))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            tracing::warn!(
                operation = "openai_rerank",
                provider = "openai_compatible",
                model,
                stream = false,
                attempt = 0,
                latency_ms = start.elapsed().as_millis() as u64,
                status = status.as_u16(),
                "OpenAI 兼容请求失败"
            );
            return Err(LlmError::Provider {
                provider: "openai_compatible",
                message: format!("Rerank API 返回错误 {status}: {body}"),
            });
        }

        let body = response
            .json::<Value>()
            .await
            .map_err(|err| LlmError::Serialization(err.to_string()))?;
        tracing::info!(
            operation = "openai_rerank",
            provider = "openai_compatible",
            model,
            stream = false,
            attempt = 0,
            latency_ms = start.elapsed().as_millis() as u64,
            "OpenAI 兼容请求完成"
        );
        Ok(body)
    }

    pub async fn list_models(&self) -> Result<Vec<ProviderModelInfo>, LlmError> {
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

fn normalize_rerank_api_base(base_url: &str) -> anyhow::Result<String> {
    let base = normalize_api_base(base_url)?;
    let cleaned = base.trim_end_matches('/');
    let cleaned = cleaned.strip_suffix("/rerank").unwrap_or(cleaned);
    Ok(cleaned.to_string())
}

#[cfg(test)]
mod tests {
    use super::normalize_rerank_api_base;

    #[test]
    fn normalizes_openai_compatible_rerank_api_base() {
        assert_eq!(
            normalize_rerank_api_base("http://127.0.0.1:8000/v1").unwrap(),
            "http://127.0.0.1:8000/v1"
        );
        assert_eq!(
            normalize_rerank_api_base("http://127.0.0.1:8000/v1/rerank").unwrap(),
            "http://127.0.0.1:8000/v1"
        );
        assert_eq!(
            normalize_rerank_api_base("http://127.0.0.1:8000/rerank").unwrap(),
            "http://127.0.0.1:8000"
        );
    }
}
