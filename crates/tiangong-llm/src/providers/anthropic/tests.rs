use std::sync::Arc;

use async_trait::async_trait;
use futures_util::stream;
use serde_json::json;
use tiangong_anthropic::types::{
    ContentBlock, ContentBlockDeltaData, ContentBlockStartData, EventStream, MessageStartData,
    MessagesCreateRequest, MessagesCreateResponse, StreamEvent, Usage,
};

use crate::message::{ChatMessage, MessageContent, MessageRole};
use crate::provider::LlmProvider;
use crate::providers::anthropic::client::{AnthropicClient, AnthropicTransport};
use crate::providers::anthropic::config::AnthropicConfig;
use crate::providers::anthropic::provider::AnthropicProvider;
use crate::request::{ProviderRequest, ThinkingConfig};
use crate::stream::ProviderStreamEvent;
use crate::tool::{ToolChoice, ToolResult, ToolResultContent, ToolSpec};

#[derive(Clone, Default)]
struct MockAnthropicTransport {
    response: Option<MessagesCreateResponse>,
    stream_events: Vec<Result<StreamEvent, crate::error::LlmError>>,
}

#[async_trait]
impl AnthropicTransport for MockAnthropicTransport {
    async fn create(
        &self,
        _request: MessagesCreateRequest,
    ) -> Result<MessagesCreateResponse, crate::error::LlmError> {
        Ok(self.response.clone().expect("mock response"))
    }

    async fn create_stream(
        &self,
        _request: MessagesCreateRequest,
    ) -> Result<EventStream, crate::error::LlmError> {
        let items = self
            .stream_events
            .clone()
            .into_iter()
            .map(|item| match item {
                Ok(event) => Ok(event),
                Err(err) => Err(tiangong_anthropic::AnthropicError::Stream(err.to_string())),
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
        max_tokens: 1024,
        temperature: Some(0.2),
        top_p: None,
        stop_sequences: vec!["STOP".to_string()],
        metadata: None,
        thinking: Some(ThinkingConfig {
            budget_tokens: 4096,
        }),
        reasoning_effort: None,
        thinking_disabled: false,
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
        mapped.thinking,
        Some(tiangong_anthropic::types::ThinkingConfig::Enabled {
            budget_tokens: 4096
        })
    ));
    assert!(matches!(
        mapped.tool_choice,
        Some(tiangong_anthropic::types::ToolChoice::Auto)
    ));
}

#[test]
fn adjacent_user_messages_fold_into_ordered_text_blocks() {
    let mut request = sample_request();
    request.messages = vec![
        ChatMessage::text(MessageRole::User, "A"),
        ChatMessage::text(MessageRole::User, "B"),
    ];

    let mapped = super::mapping::to_anthropic_request(&request).expect("mapped request");

    assert_eq!(
        serde_json::to_value(mapped.messages).unwrap(),
        json!([{
            "role": "user",
            "content": [
                {"type": "text", "text": "A"},
                {"type": "text", "text": "B"}
            ]
        }])
    );
}

#[test]
fn tool_result_and_following_user_text_share_user_turn_in_block_order() {
    let mut request = sample_request();
    request.messages = vec![
        ChatMessage::text(MessageRole::User, "A"),
        ChatMessage::new(
            MessageRole::Assistant,
            vec![MessageContent::ToolCall(crate::tool::ToolCall {
                id: "call_1".to_string(),
                name: "search_web".to_string(),
                arguments: json!({"q": "rust"}),
            })],
        ),
        ChatMessage::new(
            MessageRole::Tool,
            vec![MessageContent::ToolResult(ToolResult {
                tool_call_id: "call_1".to_string(),
                content: ToolResultContent::Text("result".to_string()),
                is_error: false,
            })],
        ),
        ChatMessage::text(MessageRole::User, "B"),
    ];

    let mapped = super::mapping::to_anthropic_request(&request).expect("mapped request");

    assert_eq!(
        serde_json::to_value(mapped.messages).unwrap(),
        json!([
            {
                "role": "user",
                "content": [{"type": "text", "text": "A"}]
            },
            {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "call_1",
                    "name": "search_web",
                    "input": {"q": "rust"}
                }]
            },
            {
                "role": "user",
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "call_1",
                        "content": "result",
                        "is_error": false
                    },
                    {"type": "text", "text": "B"}
                ]
            }
        ])
    );
}

#[test]
fn test_tool_choice_none_maps_to_anthropic_none() {
    // ToolChoice::None 必须映射为 Anthropic 的 {"type": "none"}，
    // 在提供 tools schema 的同时禁止调用工具（如总结阶段）。
    let mut req = sample_request();
    req.tool_choice = Some(ToolChoice::None);
    let mapped = super::mapping::to_anthropic_request(&req).expect("mapped request");
    assert!(matches!(
        mapped.tool_choice,
        Some(tiangong_anthropic::types::ToolChoice::None)
    ));
    // tools schema 仍保留，保持 KV cache 前缀一致
    assert!(mapped.tools.is_some());
}

#[test]
fn test_tool_and_thinking_mapping_back_to_message_content() {
    let response = MessagesCreateResponse {
        id: "msg_1".to_string(),
        kind: "message".to_string(),
        role: tiangong_anthropic::types::MessageRole::Assistant,
        content: vec![
            ContentBlock::Thinking {
                thinking: "先搜索一下".to_string(),
                signature: Some("sig".to_string()),
            },
            ContentBlock::ToolUse {
                id: "call_1".to_string(),
                name: "search_web".to_string(),
                input: json!({"q": "rust"}),
            },
        ],
        model: "claude-3-7-sonnet".to_string(),
        stop_reason: Some("tool_use".to_string()),
        stop_sequence: None,
        usage: Some(Usage {
            input_tokens: Some(12),
            output_tokens: Some(7),
        }),
    };

    let mapped = super::mapping::from_anthropic_response(response).expect("mapped response");
    assert_eq!(mapped.reasoning_content.as_deref(), Some("先搜索一下"));
    assert_eq!(
        mapped.usage.as_ref().map(|usage| usage.total_tokens),
        Some(19)
    );
    assert!(matches!(
        mapped.assistant_message.content.first(),
        Some(MessageContent::Thinking(thinking)) if thinking.signature.as_deref() == Some("sig")
    ));
    assert!(matches!(
        mapped.assistant_message.content.get(1),
        Some(MessageContent::ToolCall(_))
    ));
}

#[tokio::test]
async fn test_provider_complete_and_stream_behavior() {
    let response = MessagesCreateResponse {
        id: "msg_1".to_string(),
        kind: "message".to_string(),
        role: tiangong_anthropic::types::MessageRole::Assistant,
        content: vec![ContentBlock::Text {
            text: "你好，世界".to_string(),
        }],
        model: "claude-3-7-sonnet".to_string(),
        stop_reason: Some("end_turn".to_string()),
        stop_sequence: None,
        usage: Some(Usage {
            input_tokens: Some(10),
            output_tokens: Some(4),
        }),
    };

    let stream_events = vec![
        Ok(StreamEvent::MessageStart {
            message: MessageStartData {
                id: "msg_1".to_string(),
                model: "claude-3-7-sonnet".to_string(),
                role: tiangong_anthropic::types::MessageRole::Assistant,
                content: vec![],
                stop_reason: None,
                stop_sequence: None,
                usage: Some(Usage {
                    input_tokens: Some(10),
                    output_tokens: Some(0),
                }),
            },
        }),
        Ok(StreamEvent::ContentBlockStart {
            index: 0,
            content_block: ContentBlockStartData::Thinking {
                thinking: "先想一下".to_string(),
                signature: String::new(),
            },
        }),
        Ok(StreamEvent::ContentBlockDelta {
            index: 1,
            delta: ContentBlockDeltaData::TextDelta {
                text: "你好".to_string(),
            },
        }),
        Ok(StreamEvent::MessageStop),
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
    assert!(seen.contains(&ProviderStreamEvent::ReasoningDelta("先想一下".to_string())));
    assert!(seen.contains(&ProviderStreamEvent::TextDelta("你好".to_string())));
    assert!(seen.contains(&ProviderStreamEvent::MessageEnd { stop_reason: None }));
}

#[tokio::test]
async fn test_provider_list_models() {
    let provider = AnthropicProvider::new(AnthropicClient::new(
        MockAnthropicTransport::default(),
        AnthropicConfig::new("test"),
    ));

    let models = provider.list_models().await.expect("list models");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "claude-3-7-sonnet");
}
