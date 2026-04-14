use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use futures_util::stream;
use serde_json::Value;

use crate::config::AnthropicConfig;
use crate::error::AnthropicError;
use crate::types::{
    ContentBlockDeltaData, ContentBlockStartData, EventStream, MessageDeltaData, MessageStartData,
    MessagesCreateRequest, MessagesCreateResponse, ModelsListResponse, StreamEvent,
};

#[derive(Clone)]
pub struct AnthropicClient {
    http_client: reqwest::Client,
    config: AnthropicConfig,
}

impl AnthropicClient {
    pub fn from_config(config: AnthropicConfig) -> Result<Self, AnthropicError> {
        let http_client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|err| AnthropicError::Transport(err.to_string()))?;
        Ok(Self {
            http_client,
            config,
        })
    }

    pub async fn create(
        &self,
        request: MessagesCreateRequest,
    ) -> Result<MessagesCreateResponse, AnthropicError> {
        let response = self
            .request_builder("/v1/messages")
            .json(&request)
            .send()
            .await
            .map_err(map_reqwest_error)?;
        parse_json_response(response).await
    }

    pub async fn create_stream(
        &self,
        mut request: MessagesCreateRequest,
    ) -> Result<EventStream, AnthropicError> {
        request.stream = Some(true);
        let response = self
            .request_builder("/v1/messages")
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .json(&request)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        if !response.status().is_success() {
            return Err(parse_error_response(response).await);
        }

        let sse_stream = response.bytes_stream().eventsource();
        let stream = stream::unfold(sse_stream, |mut sse_stream| async move {
            match sse_stream.next().await {
                Some(Ok(message)) => {
                    let item = parse_sse_event(&message.event, &message.data);
                    Some((item, sse_stream))
                }
                Some(Err(err)) => Some((Err(AnthropicError::Stream(err.to_string())), sse_stream)),
                None => None,
            }
        });
        Ok(Box::pin(stream))
    }

    pub async fn list_models(&self) -> Result<ModelsListResponse, AnthropicError> {
        let response = self
            .request_builder("/v1/models")
            .send()
            .await
            .map_err(map_reqwest_error)?;
        parse_json_response(response).await
    }

    fn request_builder(&self, path: &str) -> reqwest::RequestBuilder {
        let url = format!(
            "{}/{}",
            self.config.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        );
        let mut builder = self
            .http_client
            .post(url.clone())
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", &self.config.api_version)
            .header(reqwest::header::ACCEPT, "application/json");
        if path == "/v1/models" {
            builder = self
                .http_client
                .get(url)
                .header("x-api-key", &self.config.api_key)
                .header("anthropic-version", &self.config.api_version)
                .header(reqwest::header::ACCEPT, "application/json");
        }
        if let Some(beta) = &self.config.beta {
            builder = builder.header("anthropic-beta", beta);
        }
        builder
    }
}

fn map_reqwest_error(err: reqwest::Error) -> AnthropicError {
    if err.is_timeout() {
        AnthropicError::Transport(format!("timeout: {err}"))
    } else {
        AnthropicError::Transport(err.to_string())
    }
}

async fn parse_error_response(response: reqwest::Response) -> AnthropicError {
    let status = response.status();
    let body = response.bytes().await.unwrap_or_default();
    let body_text = String::from_utf8_lossy(&body).to_string();

    match status.as_u16() {
        400 => AnthropicError::InvalidRequest(body_text),
        401 | 403 => AnthropicError::Authentication(body_text),
        429 => AnthropicError::RateLimited(body_text),
        _ if status.is_server_error() => AnthropicError::Transport(body_text),
        _ => AnthropicError::Api(body_text),
    }
}

async fn parse_json_response<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, AnthropicError> {
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|err| AnthropicError::Transport(err.to_string()))?;

    if !status.is_success() {
        let body_text = String::from_utf8_lossy(&body).to_string();
        return Err(match status.as_u16() {
            400 => AnthropicError::InvalidRequest(body_text),
            401 | 403 => AnthropicError::Authentication(body_text),
            429 => AnthropicError::RateLimited(body_text),
            _ if status.is_server_error() => AnthropicError::Transport(body_text),
            _ => AnthropicError::Api(body_text),
        });
    }

    serde_json::from_slice(&body).map_err(|err| {
        AnthropicError::Serialization(format!(
            "{err}: {}",
            String::from_utf8_lossy(&body[..body.len().min(512)])
        ))
    })
}

fn parse_sse_event(event_type: &str, data: &str) -> Result<StreamEvent, AnthropicError> {
    if event_type == "ping" {
        return Ok(StreamEvent::Ping);
    }
    if event_type == "error" {
        let payload: Value = serde_json::from_str(data)
            .map_err(|err| AnthropicError::Serialization(err.to_string()))?;
        return Ok(StreamEvent::Error {
            message: payload.to_string(),
        });
    }

    let value: Value =
        serde_json::from_str(data).map_err(|err| AnthropicError::Serialization(err.to_string()))?;
    match event_type {
        "message_start" => Ok(StreamEvent::MessageStart {
            message: serde_json::from_value::<MessageStartData>(
                value
                    .get("message")
                    .cloned()
                    .ok_or_else(|| AnthropicError::Serialization(data.to_string()))?,
            )
            .map_err(|err| AnthropicError::Serialization(err.to_string()))?,
        }),
        "content_block_start" => Ok(StreamEvent::ContentBlockStart {
            index: value
                .get("index")
                .and_then(Value::as_u64)
                .ok_or_else(|| AnthropicError::Serialization(data.to_string()))?
                as usize,
            content_block: serde_json::from_value::<ContentBlockStartData>(
                value
                    .get("content_block")
                    .cloned()
                    .ok_or_else(|| AnthropicError::Serialization(data.to_string()))?,
            )
            .map_err(|err| AnthropicError::Serialization(err.to_string()))?,
        }),
        "content_block_delta" => Ok(StreamEvent::ContentBlockDelta {
            index: value
                .get("index")
                .and_then(Value::as_u64)
                .ok_or_else(|| AnthropicError::Serialization(data.to_string()))?
                as usize,
            delta: serde_json::from_value::<ContentBlockDeltaData>(
                value
                    .get("delta")
                    .cloned()
                    .ok_or_else(|| AnthropicError::Serialization(data.to_string()))?,
            )
            .map_err(|err| AnthropicError::Serialization(err.to_string()))?,
        }),
        "content_block_stop" => Ok(StreamEvent::ContentBlockStop {
            index: value
                .get("index")
                .and_then(Value::as_u64)
                .ok_or_else(|| AnthropicError::Serialization(data.to_string()))?
                as usize,
        }),
        "message_delta" => Ok(StreamEvent::MessageDelta {
            delta: serde_json::from_value::<MessageDeltaData>(
                value
                    .get("delta")
                    .cloned()
                    .ok_or_else(|| AnthropicError::Serialization(data.to_string()))?,
            )
            .map_err(|err| AnthropicError::Serialization(err.to_string()))?,
            usage: value
                .get("usage")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|err| AnthropicError::Serialization(err.to_string()))?,
        }),
        "message_stop" => Ok(StreamEvent::MessageStop),
        other => Err(AnthropicError::Stream(format!(
            "不支持的 SSE 事件：{other}"
        ))),
    }
}
