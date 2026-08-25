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

/// 流式请求的响应形态：正常 SSE 流，或服务端忽略 stream 参数返回的一次性 JSON。
pub enum ResponsesStreamResponse {
    Sse(ResponsesByotStream),
    Complete(Value),
}

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
    ) -> Result<ResponsesStreamResponse, LlmError> {
        let base = normalize_api_base(&self.config.base_url)
            .map_err(|err| LlmError::Configuration(err.to_string()))?;
        let url = format!("{base}/responses");
        let api_key = self.config.api_key.clone();
        let request_timeout = self.config.timeout;
        self.with_retry("openai_stream", model, true, move || {
            let url = url.clone();
            let api_key = api_key.clone();
            let payload = payload.clone();
            let request_timeout = request_timeout;
            async move {
                use crate::providers::openai_chatcompletions::client::{
                    StreamBody, resolve_stream_body, stream_timeout_error,
                };
                // 建连、等待响应头与一次性响应体的读取均受用户配置的请求
                // 超时约束。SSE 流本身允许长时间增量生成，建流成功后不再
                // 受总时限限制。
                let client = reqwest::Client::builder().build()?;
                let mut request = client
                    .post(&url)
                    .header(reqwest::header::ACCEPT, "text/event-stream")
                    .json(&payload);
                if !api_key.trim().is_empty() {
                    request = request.bearer_auth(&api_key);
                }
                let response = tokio::time::timeout(request_timeout, request.send())
                    .await
                    .map_err(|_| stream_timeout_error(request_timeout))??;
                let status = response.status();
                if !status.is_success() {
                    let body = response.text().await.unwrap_or_default();
                    return Err(async_openai::error::OpenAIError::ApiError(
                        async_openai::error::ApiErrorResponse {
                            status_code: status,
                            api_error: async_openai::error::ApiError {
                                message: format!("{status}: {body}"),
                                r#type: None,
                                param: None,
                                code: None,
                            },
                        },
                    ));
                }
                match resolve_stream_body(response, request_timeout).await? {
                    StreamBody::Sse(stream) => Ok(ResponsesStreamResponse::Sse(stream)),
                    StreamBody::Complete(value) => {
                        // 服务端忽略 stream 参数返回一次性 JSON：SSE 解析器会把这些
                        // 行全部当未知字段丢弃且不报错，必须在 llm 层按完整响应接住。
                        tracing::info!(
                            operation = "openai_stream",
                            provider = "openai",
                            model,
                            "服务端未按 SSE 流式返回，转按一次性完整响应处理"
                        );
                        Ok(ResponsesStreamResponse::Complete(value))
                    }
                }
            }
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
