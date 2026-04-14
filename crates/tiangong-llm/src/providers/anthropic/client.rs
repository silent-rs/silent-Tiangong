use std::time::Duration;

use async_trait::async_trait;
use backoff::ExponentialBackoff;
use serde::Deserialize;

use crate::error::LlmError;
use crate::model::ProviderModelInfo;
use crate::providers::anthropic::config::AnthropicConfig;
use crate::providers::anthropic::error::map_anthropic_error;
use crate::providers::anthropic::stream::AnthropicSdkStream;

#[async_trait]
pub(crate) trait AnthropicTransport: Send + Sync {
    async fn create(
        &self,
        request: async_anthropic::types::CreateMessagesRequest,
    ) -> Result<async_anthropic::types::CreateMessagesResponse, LlmError>;

    async fn create_stream(
        &self,
        request: async_anthropic::types::CreateMessagesRequest,
    ) -> Result<AnthropicSdkStream, LlmError>;

    async fn list_models(&self) -> Result<Vec<ProviderModelInfo>, LlmError>;
}

#[derive(Clone)]
pub struct AsyncAnthropicTransport {
    client: async_anthropic::Client,
    http_client: reqwest::Client,
    base_url: String,
    api_key: String,
    api_version: String,
    beta: Option<String>,
}

impl AsyncAnthropicTransport {
    pub fn new(config: &AnthropicConfig) -> Result<Self, LlmError> {
        let http_client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|err| LlmError::Transport(format!("构建 Anthropic HTTP 客户端失败：{err}")))?;

        let no_retry = ExponentialBackoff {
            max_elapsed_time: Some(Duration::from_nanos(1)),
            ..Default::default()
        };

        let api_version = config.resolve_api_version();
        let base_url = config
            .normalized_base_url()
            .unwrap_or_else(|| "https://api.anthropic.com".to_string());

        let mut builder_binding = async_anthropic::Client::builder();
        let mut builder = builder_binding
            .http_client(http_client.clone())
            .api_key(config.api_key.clone())
            .version(api_version.clone())
            .backoff(no_retry);

        builder = builder.base_url(base_url.clone());
        if let Some(beta) = &config.beta {
            builder = builder.beta(beta.clone());
        }

        let client = builder.build().map_err(|err| {
            LlmError::Configuration(format!("构建 Anthropic SDK 客户端失败：{err}"))
        })?;

        Ok(Self {
            client,
            http_client,
            base_url,
            api_key: config.api_key.clone(),
            api_version,
            beta: config.beta.clone(),
        })
    }
}

#[async_trait]
impl AnthropicTransport for AsyncAnthropicTransport {
    async fn create(
        &self,
        request: async_anthropic::types::CreateMessagesRequest,
    ) -> Result<async_anthropic::types::CreateMessagesResponse, LlmError> {
        self.client
            .messages()
            .create(request)
            .await
            .map_err(map_anthropic_error)
    }

    async fn create_stream(
        &self,
        request: async_anthropic::types::CreateMessagesRequest,
    ) -> Result<AnthropicSdkStream, LlmError> {
        Ok(self.client.messages().create_stream(request).await)
    }

    async fn list_models(&self) -> Result<Vec<ProviderModelInfo>, LlmError> {
        let url = format!("{}/v1/models", self.base_url.trim_end_matches('/'));
        let mut request = self
            .http_client
            .get(url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", &self.api_version)
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some(beta) = &self.beta {
            request = request.header("anthropic-beta", beta);
        }

        let response = request.send().await.map_err(|err| {
            if err.is_timeout() {
                LlmError::Timeout(0)
            } else {
                LlmError::Transport(err.to_string())
            }
        })?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| LlmError::Transport(format!("读取 Anthropic 响应体失败：{err}")))?;

        if !status.is_success() {
            return Err(map_list_models_http_error(status, &body));
        }

        parse_models_response(&body)
    }
}

#[derive(Clone)]
pub(crate) struct AnthropicClient<T = AsyncAnthropicTransport> {
    transport: T,
    config: AnthropicConfig,
}

#[derive(Debug, Deserialize)]
struct AnthropicModelsEnvelope {
    #[serde(default)]
    data: Vec<AnthropicModelEntry>,
}

#[derive(Debug, Deserialize)]
struct AnthropicModelEntry {
    id: String,
    #[serde(default)]
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicDirectModelEntry {
    id: String,
    #[serde(default)]
    display_name: Option<String>,
}

fn parse_models_response(body: &str) -> Result<Vec<ProviderModelInfo>, LlmError> {
    if let Ok(response) = serde_json::from_str::<AnthropicModelsEnvelope>(body) {
        return Ok(response
            .data
            .into_iter()
            .map(|model| ProviderModelInfo {
                display_name: model.display_name.or_else(|| Some(model.id.clone())),
                id: model.id,
            })
            .collect());
    }

    if let Ok(response) = serde_json::from_str::<Vec<AnthropicDirectModelEntry>>(body) {
        return Ok(response
            .into_iter()
            .map(|model| ProviderModelInfo {
                display_name: model.display_name.or_else(|| Some(model.id.clone())),
                id: model.id,
            })
            .collect());
    }

    Err(LlmError::Serialization(format!(
        "解析 Anthropic 模型列表失败，响应体前 512 字节：{}",
        body.chars().take(512).collect::<String>()
    )))
}

fn map_list_models_http_error(status: reqwest::StatusCode, body: &str) -> LlmError {
    let message = body.chars().take(512).collect::<String>();
    match status {
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
            LlmError::Authentication(format!("请检查 Anthropic API Key：{message}"))
        }
        reqwest::StatusCode::TOO_MANY_REQUESTS => LlmError::RateLimited(message),
        reqwest::StatusCode::BAD_REQUEST => LlmError::InvalidRequest(message),
        _ if status.is_server_error() => LlmError::Transport(format!(
            "Anthropic 模型列表请求失败，HTTP {}: {}",
            status.as_u16(),
            message
        )),
        _ => LlmError::Provider {
            provider: "anthropic",
            message: format!("模型列表请求失败，HTTP {}: {}", status.as_u16(), message),
        },
    }
}

#[cfg(test)]
mod unit_tests {
    use super::parse_models_response;

    #[test]
    fn parse_official_models_response() {
        let models = parse_models_response(
            r#"{
                "data":[
                    {
                        "type":"model",
                        "id":"claude-sonnet-4-20250514",
                        "display_name":"Claude Sonnet 4",
                        "created_at":"2025-05-14T00:00:00Z"
                    }
                ],
                "has_more":false,
                "first_id":"claude-sonnet-4-20250514",
                "last_id":"claude-sonnet-4-20250514"
            }"#,
        )
        .expect("parse official response");

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "claude-sonnet-4-20250514");
        assert_eq!(models[0].display_name.as_deref(), Some("Claude Sonnet 4"));
    }

    #[test]
    fn parse_proxy_models_response_without_display_name() {
        let models = parse_models_response(
            r#"{
                "data":[
                    {"id":"claude-3-5-sonnet-latest","object":"model"}
                ]
            }"#,
        )
        .expect("parse proxy response");

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "claude-3-5-sonnet-latest");
        assert_eq!(
            models[0].display_name.as_deref(),
            Some("claude-3-5-sonnet-latest")
        );
    }
}

impl AnthropicClient<AsyncAnthropicTransport> {
    pub fn from_config(config: AnthropicConfig) -> Result<Self, LlmError> {
        let transport = AsyncAnthropicTransport::new(&config)?;
        Ok(Self { transport, config })
    }
}

impl<T> AnthropicClient<T>
where
    T: AnthropicTransport,
{
    #[cfg(test)]
    pub fn new(transport: T, config: AnthropicConfig) -> Self {
        Self { transport, config }
    }

    pub async fn create(
        &self,
        request: async_anthropic::types::CreateMessagesRequest,
    ) -> Result<async_anthropic::types::CreateMessagesResponse, LlmError> {
        let model = request.model.clone();
        self.run_with_retry("anthropic_complete", "anthropic", &model, false, || {
            let request = request.clone();
            async move { self.transport.create(request).await }
        })
        .await
    }

    pub async fn create_stream(
        &self,
        request: async_anthropic::types::CreateMessagesRequest,
    ) -> Result<AnthropicSdkStream, LlmError> {
        let model = request.model.clone();
        self.run_with_retry("anthropic_stream", "anthropic", &model, true, || {
            let request = request.clone();
            async move { self.transport.create_stream(request).await }
        })
        .await
    }

    pub async fn list_models(&self) -> Result<Vec<ProviderModelInfo>, LlmError> {
        self.run_with_retry(
            "anthropic_list_models",
            "anthropic",
            "<list_models>",
            false,
            || async { self.transport.list_models().await },
        )
        .await
    }

    async fn run_with_retry<F, Fut, R>(
        &self,
        operation: &'static str,
        provider: &'static str,
        model: &str,
        stream: bool,
        mut f: F,
    ) -> Result<R, LlmError>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<R, LlmError>>,
    {
        let mut attempt = 0u32;
        let mut delay_ms = 1000u64;

        loop {
            let start = std::time::Instant::now();
            tracing::info!(
                operation,
                provider,
                model,
                stream,
                attempt,
                "开始 Anthropic 请求"
            );
            match f().await {
                Ok(result) => {
                    tracing::info!(
                        operation,
                        provider,
                        model,
                        stream,
                        attempt,
                        latency_ms = start.elapsed().as_millis() as u64,
                        "Anthropic 请求完成"
                    );
                    return Ok(result);
                }
                Err(err) if err.is_retryable() && attempt < self.config.max_retries => {
                    attempt += 1;
                    tracing::warn!(
                        operation,
                        provider,
                        model,
                        stream,
                        attempt,
                        delay_ms,
                        error = %err,
                        "Anthropic 请求失败，准备重试"
                    );
                    if let Some(notifier) = &self.config.retry_notifier {
                        notifier(attempt, self.config.max_retries, delay_ms, &err.to_string());
                    }
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    delay_ms *= 2;
                }
                Err(err) => {
                    tracing::warn!(
                        operation,
                        provider,
                        model,
                        stream,
                        attempt,
                        latency_ms = start.elapsed().as_millis() as u64,
                        error = %err,
                        "Anthropic 请求失败"
                    );
                    return Err(err);
                }
            }
        }
    }
}
