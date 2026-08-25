use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use futures_util::stream;
use serde_json::Value;

use crate::config::AnthropicConfig;
use crate::error::AnthropicError;
use crate::types::{
    ContentBlock, ContentBlockDeltaData, ContentBlockStartData, EventStream, MessageDeltaData,
    MessageStartData, MessagesCreateRequest, MessagesCreateResponse, ModelsListResponse,
    StreamEvent, ThinkingConfig,
};

/// 把一次性完整响应合成为等价的流事件序列。
///
/// 网关忽略 stream 参数返回 application/json 时，SDK 在内部按完整响应
/// 接住并合成事件流，调用方无需感知差异。
fn complete_response_events(response: MessagesCreateResponse) -> Vec<StreamEvent> {
    let mut events = vec![StreamEvent::MessageStart {
        message: MessageStartData {
            id: response.id,
            model: response.model,
            role: response.role,
            content: Vec::new(),
            stop_reason: None,
            stop_sequence: None,
            usage: None,
        },
    }];
    for (index, block) in response.content.into_iter().enumerate() {
        let content_block = match block {
            ContentBlock::Text { text } => ContentBlockStartData::Text { text },
            ContentBlock::ToolUse { id, name, input } => {
                ContentBlockStartData::ToolUse { id, name, input }
            }
            ContentBlock::Thinking {
                thinking,
                signature,
            } => ContentBlockStartData::Thinking {
                thinking,
                signature: signature.unwrap_or_default(),
            },
            ContentBlock::RedactedThinking { data } => {
                ContentBlockStartData::RedactedThinking { data }
            }
            ContentBlock::ToolResult { .. } | ContentBlock::Unknown => continue,
        };
        events.push(StreamEvent::ContentBlockStart {
            index,
            content_block,
        });
        events.push(StreamEvent::ContentBlockStop { index });
    }
    events.push(StreamEvent::MessageDelta {
        delta: MessageDeltaData {
            stop_reason: response.stop_reason,
            stop_sequence: response.stop_sequence,
        },
        usage: response.usage,
    });
    events.push(StreamEvent::MessageStop);
    events
}

/// 官方 Anthropic API 域名。
const OFFICIAL_API_HOST: &str = "api.anthropic.com";
/// 默认思考预算下，为正文（含工具调用）保留的输出空间。
const DEFAULT_BUDGET_TEXT_RESERVE_TOKENS: u32 = 8_192;
/// 官方协议要求的思考预算下限（官方拒绝小于 1024 的值）。
const MIN_THINKING_BUDGET_TOKENS: u32 = 1_024;

#[derive(Clone)]
pub struct AnthropicClient {
    http_client: reqwest::Client,
    stream_http_client: reqwest::Client,
    config: AnthropicConfig,
}

impl AnthropicClient {
    pub fn from_config(config: AnthropicConfig) -> Result<Self, AnthropicError> {
        let http_client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|err| AnthropicError::Transport(err.to_string()))?;
        let stream_http_client = reqwest::Client::builder()
            .build()
            .map_err(|err| AnthropicError::Transport(err.to_string()))?;
        Ok(Self {
            http_client,
            stream_http_client,
            config,
        })
    }

    pub async fn create(
        &self,
        mut request: MessagesCreateRequest,
    ) -> Result<MessagesCreateResponse, AnthropicError> {
        self.apply_official_thinking_budget(&mut request);
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
        self.apply_official_thinking_budget(&mut request);
        request.stream = Some(true);
        let response = self
            .stream_request_builder("/v1/messages")
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .json(&request)
            .send()
            .await
            .map_err(map_reqwest_error)?;

        if !response.status().is_success() {
            return Err(parse_error_response(response).await);
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !content_type.contains("text/event-stream") {
            // 网关忽略 stream 参数返回一次性 JSON：SSE 解析器会把这些行
            // 全部当未知字段丢弃且不报错，在 SDK 内按完整响应接住并合成事件流。
            let complete = parse_json_response(response).await?;
            return Ok(Box::pin(stream::iter(
                complete_response_events(complete).into_iter().map(Ok),
            )));
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
        self.request_builder_with_client(&self.http_client, path)
    }

    /// 官方 Anthropic 端点要求 thinking.enabled 必须携带 budget_tokens
    /// （≥1024 且严格小于 max_tokens）。DeepSeek/GLM 等兼容实现可省略预算，
    /// 且部分实现（实测 GLM）会执行预算并在思考超限时截断输出，因此仅对
    /// 官方端点填充默认推荐预算：max_tokens 保留正文空间后全部交给思考。
    fn apply_official_thinking_budget(&self, request: &mut MessagesCreateRequest) {
        if !matches!(
            request.thinking,
            Some(ThinkingConfig::Enabled {
                budget_tokens: None
            })
        ) || !self.is_official_endpoint()
        {
            return;
        }
        let budget = request
            .max_tokens
            .saturating_sub(DEFAULT_BUDGET_TEXT_RESERVE_TOKENS)
            .max(MIN_THINKING_BUDGET_TOKENS)
            .min(request.max_tokens.saturating_sub(1));
        request.thinking = Some(ThinkingConfig::Enabled {
            budget_tokens: Some(budget),
        });
    }

    /// 判断端点是否为官方 Anthropic API（DeepSeek/GLM 等兼容端点返回 false）。
    fn is_official_endpoint(&self) -> bool {
        let base = self.config.base_url.trim_end_matches('/');
        let host = base.split("://").nth(1).unwrap_or(base);
        host.eq_ignore_ascii_case(OFFICIAL_API_HOST)
    }

    fn stream_request_builder(&self, path: &str) -> reqwest::RequestBuilder {
        self.request_builder_with_client(&self.stream_http_client, path)
    }

    fn request_builder_with_client(
        &self,
        client: &reqwest::Client,
        path: &str,
    ) -> reqwest::RequestBuilder {
        let url = format!(
            "{}/{}",
            self.config.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        );
        let mut builder = client
            .post(url.clone())
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", &self.config.api_version)
            .header(reqwest::header::ACCEPT, "application/json");
        if path == "/v1/models" {
            builder = client
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
    classify_http_error(status, body_text)
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
        return Err(classify_http_error(status, body_text));
    }

    serde_json::from_slice(&body).map_err(|err| {
        AnthropicError::Serialization(format!(
            "{err}: {}",
            String::from_utf8_lossy(&body[..body.len().min(512)])
        ))
    })
}

fn classify_http_error(status: reqwest::StatusCode, body_text: String) -> AnthropicError {
    if is_rate_limited_status_or_body(status, &body_text) {
        return AnthropicError::RateLimited(body_text);
    }

    match status.as_u16() {
        400 => AnthropicError::InvalidRequest(body_text),
        401 | 403 => AnthropicError::Authentication(body_text),
        _ if status.is_server_error() => AnthropicError::Transport(body_text),
        _ => AnthropicError::Api(body_text),
    }
}

fn is_rate_limited_status_or_body(status: reqwest::StatusCode, body_text: &str) -> bool {
    let lower = body_text.to_ascii_lowercase();
    matches!(status.as_u16(), 429 | 529)
        || lower.contains("rate limit")
        || lower.contains("too many requests")
        || lower.contains("overloaded_error")
        || lower.contains("\"type\":\"overloaded_error\"")
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
        _other => Ok(StreamEvent::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AnthropicConfig;
    use std::time::Duration;

    fn client_with_base_url(base_url: &str) -> AnthropicClient {
        AnthropicClient::from_config(AnthropicConfig {
            api_key: "test-key".to_string(),
            base_url: base_url.to_string(),
            timeout: Duration::from_secs(1),
            api_version: "2023-06-01".to_string(),
            beta: None,
        })
        .unwrap()
    }

    fn request_with(max_tokens: u32, thinking: Option<ThinkingConfig>) -> MessagesCreateRequest {
        MessagesCreateRequest {
            model: "claude-sonnet-4-5".to_string(),
            max_tokens,
            system: None,
            messages: Vec::new(),
            temperature: None,
            stop_sequences: None,
            top_p: None,
            metadata: None,
            tools: None,
            tool_choice: None,
            stream: None,
            thinking,
        }
    }

    #[test]
    fn official_endpoint_fills_default_budget() {
        let client = client_with_base_url("https://api.anthropic.com");
        let mut request = request_with(32_768, Some(ThinkingConfig::enabled()));
        client.apply_official_thinking_budget(&mut request);
        assert_eq!(
            request.thinking,
            Some(ThinkingConfig::Enabled {
                budget_tokens: Some(24_576)
            })
        );
    }

    #[test]
    fn official_endpoint_keeps_explicit_budget() {
        let client = client_with_base_url("https://api.anthropic.com");
        let mut request = request_with(32_768, Some(ThinkingConfig::with_budget(2_048)));
        client.apply_official_thinking_budget(&mut request);
        assert_eq!(
            request.thinking,
            Some(ThinkingConfig::Enabled {
                budget_tokens: Some(2_048)
            })
        );
    }

    #[test]
    fn compatible_endpoint_leaves_budget_unset() {
        // DeepSeek/GLM 等兼容端点：实测会执行预算并在思考超限时截断，
        // 不下发预算，思考量由上游决定。
        for base_url in [
            "https://open.bigmodel.cn/api/anthropic",
            "http://127.0.0.1:3456/v1",
        ] {
            let client = client_with_base_url(base_url);
            let mut request = request_with(32_768, Some(ThinkingConfig::enabled()));
            client.apply_official_thinking_budget(&mut request);
            assert_eq!(request.thinking, Some(ThinkingConfig::enabled()));
        }
    }

    #[test]
    fn small_max_tokens_clamps_budget() {
        let client = client_with_base_url("https://api.anthropic.com");
        // max_tokens 小于正文预留时：取下限 1024，并保证严格小于 max_tokens。
        let mut request = request_with(1_500, Some(ThinkingConfig::enabled()));
        client.apply_official_thinking_budget(&mut request);
        assert_eq!(
            request.thinking,
            Some(ThinkingConfig::Enabled {
                budget_tokens: Some(1_024)
            })
        );
    }

    #[test]
    fn disabled_thinking_is_untouched() {
        let client = client_with_base_url("https://api.anthropic.com");
        let mut request = request_with(32_768, Some(ThinkingConfig::Disabled));
        client.apply_official_thinking_budget(&mut request);
        assert_eq!(request.thinking, Some(ThinkingConfig::Disabled));
    }

    #[test]
    fn complete_response_events_synthesizes_stream_sequence() {
        // 网关忽略 stream 参数返回一次性 JSON 时，完整响应应合成为等价事件流：
        // MessageStart → 每个内容块 Start/Stop（完整内容在 Start）→ MessageDelta → MessageStop。
        let response: MessagesCreateResponse = serde_json::from_value(serde_json::json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-5",
            "content": [
                { "type": "text", "text": "一次性完整回复" },
                { "type": "tool_use", "id": "call_tool", "name": "read_file", "input": {"path": "TODO.md"} }
            ],
            "stop_reason": "tool_use",
            "usage": { "input_tokens": 3, "output_tokens": 5 }
        }))
        .unwrap();

        let events = complete_response_events(response);

        assert!(
            matches!(&events[0], StreamEvent::MessageStart { message } if message.id == "msg_1")
        );
        assert!(matches!(&events[1],
            StreamEvent::ContentBlockStart { index: 0, content_block: ContentBlockStartData::Text { text } }
            if text == "一次性完整回复"));
        assert!(matches!(
            &events[2],
            StreamEvent::ContentBlockStop { index: 0 }
        ));
        assert!(matches!(&events[3],
            StreamEvent::ContentBlockStart { index: 1, content_block: ContentBlockStartData::ToolUse { id, name, input } }
            if id == "call_tool" && name == "read_file" && input["path"] == "TODO.md"));
        assert!(matches!(
            &events[4],
            StreamEvent::ContentBlockStop { index: 1 }
        ));
        assert!(matches!(&events[5],
            StreamEvent::MessageDelta { delta, usage: Some(_) } if delta.stop_reason.as_deref() == Some("tool_use")));
        assert!(matches!(&events[6], StreamEvent::MessageStop));
        assert_eq!(events.len(), 7);
    }
}
