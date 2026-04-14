use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::error::LlmError;
use crate::message::{ChatMessage, MessageContent, MessageRole};
use crate::request::ProviderRequest;
use crate::response::{ProviderResponse, StopReason};
use crate::stream::ProviderStreamEvent;
use crate::tool::{ToolCall, ToolChoice, ToolResult, ToolResultContent};
use crate::usage::TokenUsageData;

pub(super) fn to_anthropic_request(
    request: &ProviderRequest,
) -> Result<async_anthropic::types::CreateMessagesRequest, LlmError> {
    let messages = request
        .messages
        .iter()
        .filter_map(map_message)
        .collect::<Result<Vec<_>, _>>()?;

    let tools: Option<Vec<Map<String, Value>>> = if request.tools.is_empty() {
        None
    } else {
        Some(
            request
                .tools
                .iter()
                .map(|tool| {
                    let mut item = Map::new();
                    item.insert("name".to_string(), Value::String(tool.name.clone()));
                    item.insert(
                        "description".to_string(),
                        Value::String(tool.description.clone()),
                    );
                    item.insert("input_schema".to_string(), tool.input_schema.clone());
                    item
                })
                .collect(),
        )
    };

    let tool_choice = request.tool_choice.as_ref().map(|choice| match choice {
        ToolChoice::Auto => async_anthropic::types::ToolChoice::Auto,
        ToolChoice::Any => async_anthropic::types::ToolChoice::Any,
        ToolChoice::Tool(name) => async_anthropic::types::ToolChoice::Tool(name.clone()),
    });

    let mut builder = async_anthropic::types::CreateMessagesRequestBuilder::default();
    builder
        .model(request.model.clone())
        .messages(messages)
        .max_tokens(request.max_tokens.unwrap_or(4096) as i32);

    if let Some(system) = request
        .system
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        builder.system(system.clone());
    }
    if let Some(temperature) = request.temperature {
        builder.temperature(temperature);
    }
    if let Some(top_p) = request.top_p {
        builder.top_p(top_p);
    }
    if !request.stop_sequences.is_empty() {
        builder.stop_sequences(request.stop_sequences.clone());
    }
    if let Some(metadata) = request.metadata.clone() {
        builder.metadata(metadata);
    }
    if let Some(tools) = tools {
        builder.tools(tools);
    }
    if let Some(tool_choice) = tool_choice {
        builder.tool_choice(tool_choice);
    }

    builder
        .build()
        .map_err(|err| LlmError::InvalidRequest(format!("构建 Anthropic 请求失败：{err}")))
}

fn map_message(message: &ChatMessage) -> Option<Result<async_anthropic::types::Message, LlmError>> {
    match message.role {
        MessageRole::System => None,
        MessageRole::User | MessageRole::Assistant | MessageRole::Tool => {
            let role = match message.role {
                MessageRole::Assistant => async_anthropic::types::MessageRole::Assistant,
                _ => async_anthropic::types::MessageRole::User,
            };

            let content = message
                .content
                .iter()
                .map(map_content)
                .collect::<Result<Vec<_>, _>>();
            Some(content.map(|content| async_anthropic::types::Message {
                role,
                content: async_anthropic::types::MessageContentList(content),
            }))
        }
    }
}

fn map_content(
    content: &MessageContent,
) -> Result<async_anthropic::types::MessageContent, LlmError> {
    match content {
        MessageContent::Text(text) => Ok(async_anthropic::types::Text::from(text).into()),
        MessageContent::ToolCall(tool_call) => Ok(async_anthropic::types::ToolUse {
            id: tool_call.id.clone(),
            name: tool_call.name.clone(),
            input: tool_call.arguments.clone(),
        }
        .into()),
        MessageContent::ToolResult(tool_result) => Ok(async_anthropic::types::ToolResult {
            tool_use_id: tool_result.tool_call_id.clone(),
            content: Some(match &tool_result.content {
                ToolResultContent::Text(text) => text.clone(),
                ToolResultContent::Json(value) => value.to_string(),
            }),
            is_error: tool_result.is_error,
        }
        .into()),
        MessageContent::Image(_) => Err(LlmError::UnsupportedFeature(
            "Anthropic 图片输入映射暂未实现".to_string(),
        )),
    }
}

pub(super) fn from_anthropic_response(
    response: async_anthropic::types::CreateMessagesResponse,
) -> Result<ProviderResponse, LlmError> {
    let raw = serde_json::to_value(&response)
        .map(Some)
        .map_err(|err| LlmError::Serialization(err.to_string()))?;

    let content = response.content.unwrap_or_default();
    let assistant_message = ChatMessage {
        role: MessageRole::Assistant,
        content: content
            .into_iter()
            .map(from_content)
            .collect::<Result<Vec<_>, _>>()?,
    };

    let usage = response.usage.map(|usage| {
        TokenUsageData::new(
            usage.input_tokens.unwrap_or(0) as usize,
            usage.output_tokens.unwrap_or(0) as usize,
        )
    });

    Ok(ProviderResponse {
        id: response.id,
        model: response.model,
        assistant_message,
        reasoning_content: None,
        stop_reason: response.stop_reason.as_deref().map(map_stop_reason),
        usage,
        raw,
    })
}

fn from_content(
    content: async_anthropic::types::MessageContent,
) -> Result<MessageContent, LlmError> {
    match content {
        async_anthropic::types::MessageContent::Text(text) => Ok(MessageContent::Text(text.text)),
        async_anthropic::types::MessageContent::ToolUse(tool_use) => {
            Ok(MessageContent::ToolCall(ToolCall {
                id: tool_use.id,
                name: tool_use.name,
                arguments: tool_use.input,
            }))
        }
        async_anthropic::types::MessageContent::ToolResult(tool_result) => {
            let content = tool_result
                .content
                .map(ToolResultContent::Text)
                .unwrap_or_else(|| ToolResultContent::Text(String::new()));
            Ok(MessageContent::ToolResult(ToolResult {
                tool_call_id: tool_result.tool_use_id,
                content,
                is_error: tool_result.is_error,
            }))
        }
    }
}

fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "end_turn" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        other => StopReason::Other(other.to_string()),
    }
}

#[derive(Default)]
pub(super) struct AnthropicStreamState {
    tool_calls: BTreeMap<usize, ToolCallAccumulator>,
}

#[derive(Default)]
struct ToolCallAccumulator {
    id: String,
}

pub(super) fn map_stream_event(
    state: &mut AnthropicStreamState,
    event: async_anthropic::types::MessagesStreamEvent,
) -> Result<Vec<ProviderStreamEvent>, LlmError> {
    match event {
        async_anthropic::types::MessagesStreamEvent::MessageStart { usage, .. } => {
            let mut events = vec![ProviderStreamEvent::MessageStart];
            if let Some(usage) = usage {
                events.push(ProviderStreamEvent::Usage(TokenUsageData::new(
                    usage.input_tokens.unwrap_or(0) as usize,
                    usage.output_tokens.unwrap_or(0) as usize,
                )));
            }
            Ok(events)
        }
        async_anthropic::types::MessagesStreamEvent::ContentBlockStart {
            index,
            content_block,
        } => match content_block {
            async_anthropic::types::MessageContent::ToolUse(tool_use) => {
                state.tool_calls.insert(
                    index,
                    ToolCallAccumulator {
                        id: tool_use.id.clone(),
                    },
                );
                Ok(vec![ProviderStreamEvent::ToolCallStart(ToolCall {
                    id: tool_use.id,
                    name: tool_use.name,
                    arguments: tool_use.input,
                })])
            }
            _ => Ok(Vec::new()),
        },
        async_anthropic::types::MessagesStreamEvent::ContentBlockDelta { index, delta } => {
            match delta {
                async_anthropic::types::ContentBlockDelta::TextDelta { text } => {
                    Ok(vec![ProviderStreamEvent::TextDelta(text)])
                }
                async_anthropic::types::ContentBlockDelta::InputJsonDelta { partial_json } => {
                    let call_id = state
                        .tool_calls
                        .get(&index)
                        .map(|call| call.id.clone())
                        .unwrap_or_else(|| format!("tool_call_{index}"));
                    Ok(vec![ProviderStreamEvent::ToolCallDelta {
                        call_id,
                        partial_json,
                    }])
                }
            }
        }
        async_anthropic::types::MessagesStreamEvent::ContentBlockStop { index } => {
            if let Some(call) = state.tool_calls.remove(&index) {
                Ok(vec![ProviderStreamEvent::ToolCallEnd { call_id: call.id }])
            } else {
                Ok(Vec::new())
            }
        }
        async_anthropic::types::MessagesStreamEvent::MessageDelta { usage, .. } => {
            if let Some(usage) = usage {
                return Ok(vec![ProviderStreamEvent::Usage(TokenUsageData::new(
                    usage.input_tokens.unwrap_or(0) as usize,
                    usage.output_tokens.unwrap_or(0) as usize,
                ))]);
            }
            Ok(Vec::new())
        }
        async_anthropic::types::MessagesStreamEvent::MessageStop => {
            Ok(vec![ProviderStreamEvent::MessageEnd])
        }
    }
}

pub(super) fn map_stream_error(
    error: crate::error::LlmError,
) -> Vec<Result<ProviderStreamEvent, LlmError>> {
    vec![Err(error)]
}
