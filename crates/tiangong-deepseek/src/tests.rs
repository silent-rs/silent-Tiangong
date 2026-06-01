use serde_json::json;

use crate::chat::parse_stream_chunk;
use crate::config::DeepSeekConfig;
use crate::error::DeepSeekError;
use crate::types::{
    BalanceResponse, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, Currency,
    ListModelsResponse, MessageRole, ReasoningEffort, StreamEvent, ThinkingConfig, ToolCall,
    ToolSpec, Usage,
};

// ── 配置 ──────────────────────────────────────────────

#[test]
fn config_default_values() {
    let config = DeepSeekConfig::new("test-key");
    assert_eq!(config.api_key, "test-key");
    assert_eq!(config.base_url, "https://api.deepseek.com");
}

// ── 错误类型 ──────────────────────────────────────────

#[test]
fn error_retryable() {
    assert!(DeepSeekError::Transport("timeout".into()).is_retryable());
    assert!(DeepSeekError::RateLimited("429".into()).is_retryable());
    assert!(!DeepSeekError::Authentication("401".into()).is_retryable());
    assert!(!DeepSeekError::InvalidRequest("400".into()).is_retryable());
    assert!(!DeepSeekError::Serialization("parse error".into()).is_retryable());
    assert!(!DeepSeekError::Api("unknown".into()).is_retryable());
}

// ── Chat 类型序列化 ──────────────────────────────────

#[test]
fn chat_message_role_serde() {
    let cases = vec![
        (MessageRole::System, "\"system\""),
        (MessageRole::User, "\"user\""),
        (MessageRole::Assistant, "\"assistant\""),
        (MessageRole::Tool, "\"tool\""),
    ];
    for (role, expected) in cases {
        assert_eq!(serde_json::to_string(&role).unwrap(), expected);
        assert_eq!(serde_json::from_str::<MessageRole>(expected).unwrap(), role);
    }
}

#[test]
fn chat_message_skip_none_fields() {
    let msg = ChatMessage {
        role: MessageRole::User,
        content: Some(json!("hello")),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        prefix: false,
    };
    let serialized = serde_json::to_string(&msg).unwrap();
    assert!(!serialized.contains("name"));
    assert!(!serialized.contains("tool_calls"));
    assert!(!serialized.contains("tool_call_id"));
    assert!(!serialized.contains("prefix"));
}

#[test]
fn chat_message_with_tool_calls() {
    let msg = ChatMessage {
        role: MessageRole::Assistant,
        content: None,
        name: None,
        tool_calls: Some(vec![ToolCall {
            id: "call_123".into(),
            kind: "function".into(),
            function: crate::types::FunctionCall {
                name: "get_weather".into(),
                arguments: r#"{"city":"Beijing"}"#.into(),
            },
        }]),
        tool_call_id: None,
        prefix: false,
    };
    let value = serde_json::to_value(&msg).unwrap();
    assert_eq!(value["tool_calls"][0]["type"], "function");
    assert_eq!(value["tool_calls"][0]["function"]["name"], "get_weather");
}

#[test]
fn chat_request_serialization() {
    let request = ChatCompletionRequest {
        model: "deepseek-v4-flash".into(),
        messages: vec![ChatMessage {
            role: MessageRole::User,
            content: Some(json!("hello")),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            prefix: false,
        }],
        max_tokens: Some(1024),
        temperature: Some(0.7),
        top_p: None,
        stream: None,
        stream_options: None,
        stop: None,
        tools: None,
        tool_choice: None,
        thinking: None,
        reasoning_effort: None,
        response_format: None,
        user_id: None,
    };
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(value["model"], "deepseek-v4-flash");
    assert_eq!(value["max_tokens"], 1024);
    assert!((value["temperature"].as_f64().unwrap() - 0.7).abs() < 0.01);
    assert!(!value.as_object().unwrap().contains_key("top_p"));
}

#[test]
fn thinking_config_serialization() {
    let enabled = ThinkingConfig {
        thinking_type: "enabled".into(),
        budget_tokens: Some(8192),
    };
    let value = serde_json::to_value(&enabled).unwrap();
    assert_eq!(value["type"], "enabled");
    assert_eq!(value["budget_tokens"], 8192);

    let disabled = ThinkingConfig {
        thinking_type: "disabled".into(),
        budget_tokens: None,
    };
    let value = serde_json::to_value(&disabled).unwrap();
    assert_eq!(value["type"], "disabled");
    assert!(!value.as_object().unwrap().contains_key("budget_tokens"));
}

#[test]
fn reasoning_effort_serde() {
    assert_eq!(
        serde_json::to_string(&ReasoningEffort::High).unwrap(),
        "\"high\""
    );
    assert_eq!(
        serde_json::to_string(&ReasoningEffort::Max).unwrap(),
        "\"max\""
    );
}

// ── Chat 响应反序列化 ──────────────────────────────────

#[test]
fn chat_response_deserialization() {
    let raw = json!({
        "id": "chatcmpl-abc123",
        "object": "chat.completion",
        "created": 1700000000u64,
        "model": "deepseek-v4-flash",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "Hello! How can I help?"
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 8,
            "total_tokens": 18,
            "prompt_cache_hit_tokens": 5,
            "prompt_cache_miss_tokens": 5
        },
        "system_fingerprint": "fp_123"
    });
    let response: ChatCompletionResponse = serde_json::from_value(raw).unwrap();
    assert_eq!(response.id, "chatcmpl-abc123");
    assert_eq!(response.choices.len(), 1);
    assert_eq!(
        response.choices[0].message.content.as_deref(),
        Some("Hello! How can I help?")
    );
    assert_eq!(response.usage.prompt_cache_hit_tokens, Some(5));
    assert_eq!(response.usage.prompt_cache_miss_tokens, Some(5));
}

#[test]
fn chat_response_with_reasoning() {
    let raw = json!({
        "id": "chatcmpl-reasoning",
        "object": "chat.completion",
        "created": 1700000000u64,
        "model": "deepseek-v4-pro",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "The answer is 42.",
                "reasoning_content": "Let me think about this..."
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150
        },
        "system_fingerprint": ""
    });
    let response: ChatCompletionResponse = serde_json::from_value(raw).unwrap();
    assert_eq!(
        response.choices[0].message.reasoning_content.as_deref(),
        Some("Let me think about this...")
    );
}

#[test]
fn chat_response_with_tool_calls() {
    let raw = json!({
        "id": "chatcmpl-tools",
        "object": "chat.completion",
        "created": 1700000000u64,
        "model": "deepseek-v4-flash",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_001",
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "arguments": "{\"city\":\"Beijing\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 20,
            "completion_tokens": 15,
            "total_tokens": 35
        },
        "system_fingerprint": ""
    });
    let response: ChatCompletionResponse = serde_json::from_value(raw).unwrap();
    let tool_calls = response.choices[0].message.tool_calls.as_ref().unwrap();
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].function.name, "get_weather");
    assert_eq!(response.choices[0].finish_reason, "tool_calls");
}

// ── 流式 chunk 解析 ──────────────────────────────────

#[test]
fn stream_text_delta() {
    let data = r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1700000000,"model":"deepseek-v4-flash","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#;
    let event = parse_stream_chunk(data).unwrap();
    assert!(matches!(event, StreamEvent::TextDelta(ref s) if s == "Hello"));
}

#[test]
fn stream_reasoning_delta() {
    let data = r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1700000000,"model":"deepseek-v4-pro","choices":[{"index":0,"delta":{"reasoning_content":"thinking..."},"finish_reason":null}]}"#;
    let event = parse_stream_chunk(data).unwrap();
    assert!(matches!(
        event,
        StreamEvent::ReasoningDelta(ref s) if s == "thinking..."
    ));
}

#[test]
fn stream_tool_call_start() {
    let data = r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1700000000,"model":"deepseek-v4-flash","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_001","type":"function","function":{"name":"get_weather","arguments":""}}]},"finish_reason":null}]}"#;
    let event = parse_stream_chunk(data).unwrap();
    match event {
        StreamEvent::ToolCallStart { id, name } => {
            assert_eq!(id, "call_001");
            assert_eq!(name, "get_weather");
        }
        _ => panic!("expected ToolCallStart, got {event:?}"),
    }
}

#[test]
fn stream_tool_call_delta() {
    let data = r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1700000000,"model":"deepseek-v4-flash","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":"}}]},"finish_reason":null}]}"#;
    let event = parse_stream_chunk(data).unwrap();
    match event {
        StreamEvent::ToolCallDelta { index, arguments } => {
            assert_eq!(index, 0);
            assert_eq!(arguments, "{\"city\":");
        }
        _ => panic!("expected ToolCallDelta, got {event:?}"),
    }
}

#[test]
fn stream_usage_event() {
    let data = r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1700000000,"model":"deepseek-v4-flash","choices":[],"usage":{"prompt_tokens":100,"completion_tokens":50,"total_tokens":150,"prompt_cache_hit_tokens":80}}"#;
    let event = parse_stream_chunk(data).unwrap();
    match event {
        StreamEvent::Usage(usage) => {
            assert_eq!(usage.prompt_tokens, 100);
            assert_eq!(usage.prompt_cache_hit_tokens, Some(80));
        }
        _ => panic!("expected Usage, got {event:?}"),
    }
}

#[test]
fn stream_empty_delta_is_error() {
    let data = r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1700000000,"model":"deepseek-v4-flash","choices":[{"index":0,"delta":{}}]}"#;
    assert!(parse_stream_chunk(data).is_err());
}

#[test]
fn stream_invalid_json_is_error() {
    let result = parse_stream_chunk("not json");
    assert!(result.is_err());
}

// ── 模型列表反序列化 ──────────────────────────────────

#[test]
fn list_models_response() {
    let raw = json!({
        "object": "list",
        "data": [
            {"id": "deepseek-v4-flash", "object": "model", "owned_by": "deepseek"},
            {"id": "deepseek-v4-pro", "object": "model", "owned_by": "deepseek"}
        ]
    });
    let response: ListModelsResponse = serde_json::from_value(raw).unwrap();
    assert_eq!(response.data.len(), 2);
    assert_eq!(response.data[0].id, "deepseek-v4-flash");
    assert_eq!(response.data[1].owned_by, "deepseek");
}

// ── 余额响应反序列化 ──────────────────────────────────

#[test]
fn balance_response_deserialization() {
    let raw = json!({
        "is_available": true,
        "balance_infos": [
            {
                "currency": "CNY",
                "total_balance": "10.50",
                "granted_balance": "5.00",
                "topped_up_balance": "5.50"
            },
            {
                "currency": "USD",
                "total_balance": "2.00",
                "granted_balance": "0.00",
                "topped_up_balance": "2.00"
            }
        ]
    });
    let response: BalanceResponse = serde_json::from_value(raw).unwrap();
    assert!(response.is_available);
    assert_eq!(response.balance_infos.len(), 2);
    assert_eq!(response.balance_infos[0].currency, Currency::Cny);
    assert_eq!(response.balance_infos[0].total_balance, "10.50");
    assert_eq!(response.balance_infos[1].currency, Currency::Usd);
}

#[test]
fn balance_response_unavailable() {
    let raw = json!({
        "is_available": false,
        "balance_infos": []
    });
    let response: BalanceResponse = serde_json::from_value(raw).unwrap();
    assert!(!response.is_available);
    assert!(response.balance_infos.is_empty());
}

// ── Usage 类型 ──────────────────────────────────────

#[test]
fn usage_with_cache_tokens() {
    let raw = json!({
        "prompt_tokens": 100,
        "completion_tokens": 50,
        "total_tokens": 150,
        "prompt_cache_hit_tokens": 80,
        "prompt_cache_miss_tokens": 20,
        "completion_tokens_details": {
            "reasoning_tokens": 30
        }
    });
    let usage: Usage = serde_json::from_value(raw).unwrap();
    assert_eq!(usage.prompt_tokens, 100);
    assert_eq!(usage.prompt_cache_hit_tokens, Some(80));
    assert_eq!(usage.prompt_cache_miss_tokens, Some(20));
    assert_eq!(
        usage.completion_tokens_details.unwrap().reasoning_tokens,
        Some(30)
    );
}

#[test]
fn usage_without_cache_tokens() {
    let raw = json!({
        "prompt_tokens": 50,
        "completion_tokens": 25,
        "total_tokens": 75
    });
    let usage: Usage = serde_json::from_value(raw).unwrap();
    assert_eq!(usage.prompt_cache_hit_tokens, None);
    assert_eq!(usage.prompt_cache_miss_tokens, None);
    assert!(usage.completion_tokens_details.is_none());
}

// ── 工具定义序列化 ──────────────────────────────────

#[test]
fn tool_spec_serialization() {
    let spec = ToolSpec {
        kind: "function".into(),
        function: crate::types::FunctionSpec {
            name: "get_weather".into(),
            description: Some("Get weather for a city".into()),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "city": {"type": "string", "description": "City name"}
                },
                "required": ["city"]
            })),
            strict: false,
        },
    };
    let value = serde_json::to_value(&spec).unwrap();
    assert_eq!(value["type"], "function");
    assert_eq!(value["function"]["name"], "get_weather");
    assert!(
        !value["function"]
            .as_object()
            .unwrap()
            .contains_key("strict")
    );
}
