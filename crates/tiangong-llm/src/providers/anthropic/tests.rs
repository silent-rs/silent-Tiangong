use std::sync::Arc;

use async_trait::async_trait;
use futures_util::stream;
use serde_json::json;

use crate::message::{ChatMessage, MessageContent, MessageRole};
use crate::provider::LlmProvider;
use crate::providers::anthropic::client::{AnthropicClient, AnthropicTransport};
use crate::providers::anthropic::config::AnthropicConfig;
use crate::providers::anthropic::provider::AnthropicProvider;
use crate::request::ProviderRequest;
use crate::stream::ProviderStreamEvent;
use crate::tool::{ToolChoice, ToolResult, ToolResultContent, ToolSpec};

#[derive(Clone, Default)]
struct MockAnthropicTransport {
    response: Option<async_anthropic::types::CreateMessagesResponse>,
    stream_events: Vec<Result<async_anthropic::types::MessagesStreamEvent, crate::error::LlmError>>,
}

#[async_trait]
impl AnthropicTransport for MockAnthropicTransport {
    async fn create(
        &self,
        _request: async_anthropic::types::CreateMessagesRequest,
    ) -> Result<async_anthropic::types::CreateMessagesResponse, crate::error::LlmError> {
        Ok(self.response.clone().expect("mock response"))
    }

    async fn create_stream(
        &self,
        _request: async_anthropic::types::CreateMessagesRequest,
    ) -> Result<super::stream::AnthropicSdkStream, crate::error::LlmError> {
        let items = self
            .stream_events
            .clone()
            .into_iter()
            .map(|event| match event {
                Ok(event) => Ok(event),
                Err(err) => Err(async_anthropic::errors::AnthropicError::Unknown(
                    err.to_string(),
                )),
            });
        Ok(Box::pin(stream::iter(items)))
    }

    async fn list_models(
        &self,
    ) -> Result<Vec<crate::model::ProviderModelInfo>, crate::error::LlmError> {
        Ok(vec![crate::model::ProviderModelInfo {
            id: "claude-3-7-sonnet".to_string(),
            display_name: Some("Claude 3.7 Sonnet".to_string()),
        }])
    }
}

fn sample_request() -> ProviderRequest {
    ProviderRequest {
        model: "claude-3-7-sonnet".to_string(),
        system: Some("你是测试助手".to_string()),
        messages: vec![
            ChatMessage::text(MessageRole::User, "你好"),
            ChatMessage {
                role: MessageRole::Assistant,
                content: vec![MessageContent::ToolCall(crate::tool::ToolCall {
                    id: "call_1".to_string(),
                    name: "search_web".to_string(),
                    arguments: json!({"q": "rust"}),
                })],
            },
            ChatMessage {
                role: MessageRole::Tool,
                content: vec![MessageContent::ToolResult(ToolResult {
                    tool_call_id: "call_1".to_string(),
                    content: ToolResultContent::Json(json!({"answer": "ok"})),
                    is_error: false,
                })],
            },
        ],
        tools: vec![ToolSpec {
            name: "search_web".to_string(),
            description: "搜索网页".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "q": { "type": "string" }
                },
                "required": ["q"]
            }),
        }],
        tool_choice: Some(ToolChoice::Auto),
        max_tokens: Some(1024),
        temperature: Some(0.2),
        top_p: None,
        stop_sequences: vec!["STOP".to_string()],
        metadata: None,
    }
}

#[test]
fn test_request_mapping_with_system_and_tools() {
    let mapped = super::mapping::to_anthropic_request(&sample_request()).expect("mapped request");
    assert_eq!(mapped.model, "claude-3-7-sonnet");
    assert_eq!(mapped.system.as_deref(), Some("你是测试助手"));
    assert_eq!(mapped.messages.len(), 3);
    assert_eq!(mapped.tools.as_ref().map(Vec::len), Some(1));
    assert!(matches!(
        mapped.tool_choice,
        Some(async_anthropic::types::ToolChoice::Auto)
    ));
}

#[test]
fn test_tool_result_mapping_back_to_message_content() {
    let response = async_anthropic::types::CreateMessagesResponse {
        id: Some("msg_1".to_string()),
        content: Some(vec![async_anthropic::types::MessageContent::ToolUse(
            async_anthropic::types::ToolUse {
                id: "call_1".to_string(),
                name: "search_web".to_string(),
                input: json!({"q": "rust"}),
            },
        )]),
        model: Some("claude-3-7-sonnet".to_string()),
        stop_reason: Some("tool_use".to_string()),
        stop_sequence: None,
        usage: Some(async_anthropic::types::Usage {
            input_tokens: Some(12),
            output_tokens: Some(7),
        }),
    };

    let mapped = super::mapping::from_anthropic_response(response).expect("mapped response");
    assert_eq!(
        mapped.usage.as_ref().map(|usage| usage.total_tokens),
        Some(19)
    );
    assert!(matches!(
        mapped.assistant_message.content.first(),
        Some(MessageContent::ToolCall(_))
    ));
}

#[tokio::test]
async fn test_provider_complete_and_stream_behavior() {
    let response = async_anthropic::types::CreateMessagesResponse {
        id: Some("msg_1".to_string()),
        content: Some(vec![async_anthropic::types::MessageContent::Text(
            async_anthropic::types::Text {
                text: "你好，世界".to_string(),
            },
        )]),
        model: Some("claude-3-7-sonnet".to_string()),
        stop_reason: Some("end_turn".to_string()),
        stop_sequence: None,
        usage: Some(async_anthropic::types::Usage {
            input_tokens: Some(10),
            output_tokens: Some(4),
        }),
    };

    let stream_events = vec![
        Ok(async_anthropic::types::MessagesStreamEvent::MessageStart {
            message: async_anthropic::types::MessageStart {
                id: "msg_1".to_string(),
                model: "claude-3-7-sonnet".to_string(),
                role: "assistant".to_string(),
                content: vec![],
                stop_reason: None,
                stop_sequence: None,
                usage: None,
            },
            usage: Some(async_anthropic::types::Usage {
                input_tokens: Some(10),
                output_tokens: Some(0),
            }),
        }),
        Ok(
            async_anthropic::types::MessagesStreamEvent::ContentBlockDelta {
                index: 0,
                delta: async_anthropic::types::ContentBlockDelta::TextDelta {
                    text: "你好".to_string(),
                },
            },
        ),
        Ok(async_anthropic::types::MessagesStreamEvent::MessageStop),
    ];

    let transport = MockAnthropicTransport {
        response: Some(response),
        stream_events,
    };
    let provider = AnthropicProvider::new(AnthropicClient::new(
        transport,
        AnthropicConfig {
            api_key: "test".to_string(),
            base_url: None,
            timeout: std::time::Duration::from_secs(30),
            max_retries: 0,
            api_version: None,
            beta: None,
            retry_notifier: Some(Arc::new(|_, _, _, _| {})),
        },
    ));

    let complete = provider.complete(sample_request()).await.expect("complete");
    assert_eq!(complete.assistant_message.content.len(), 1);

    let mut stream = provider.stream(sample_request()).await.expect("stream");
    let mut seen = Vec::new();
    use futures_util::StreamExt;
    while let Some(event) = stream.next().await {
        seen.push(event.expect("stream event"));
    }

    assert!(seen.contains(&ProviderStreamEvent::MessageStart));
    assert!(seen.contains(&ProviderStreamEvent::TextDelta("你好".to_string())));
    assert!(seen.contains(&ProviderStreamEvent::MessageEnd));
}
